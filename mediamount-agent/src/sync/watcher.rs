/// NAS directory watcher via ReadDirectoryChangesW.
///
/// Watches the NAS root with subtree support. Live events are handled eagerly
/// for the entire tree (client path derived from NAS root prefix swap).
/// Buffer overflow fallback uses the `watched` map (visited folders only)
/// to avoid walking an entire large share.

use cloud_filter::{metadata::Metadata, placeholder_file::PlaceholderFile};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE,
    FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

use super::cache::CacheIndex;
use super::write_through::EchoSuppressor;

/// Watches a NAS share for changes and syncs placeholders to the client folder.
pub struct NasWatcher {
    /// Map of NAS folder → client folder for visited folders (used for overflow fallback).
    watched: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    nas_root: PathBuf,
    client_root: PathBuf,
    /// Echo suppression — skip placeholders for files we just uploaded.
    echo: Arc<EchoSuppressor>,
    /// Cache index for recording known files during live sync.
    cache: Arc<CacheIndex>,
    /// Shutdown signal.
    shutdown: Arc<AtomicBool>,
    /// Directory handle stored as usize for CancelIoEx on shutdown.
    dir_handle: Arc<AtomicUsize>,
    /// Thread handle for join on stop.
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl NasWatcher {
    pub fn new(
        nas_root: PathBuf,
        client_root: PathBuf,
        echo: Arc<EchoSuppressor>,
        cache: Arc<CacheIndex>,
    ) -> Self {
        Self {
            watched: Arc::new(Mutex::new(HashMap::new())),
            nas_root,
            client_root,
            echo,
            cache,
            shutdown: Arc::new(AtomicBool::new(false)),
            dir_handle: Arc::new(AtomicUsize::new(0)),
            thread: Mutex::new(None),
        }
    }

    /// Register a folder pair for watching. Called when FETCH_PLACEHOLDERS fires.
    /// Used by the overflow fallback (full_diff_all_watched) to scope the diff.
    pub fn register(&self, nas_dir: PathBuf, client_dir: PathBuf) {
        let mut map = self.watched.lock().unwrap();
        map.insert(nas_dir, client_dir);
    }

    /// Start the background watcher thread.
    pub fn start(&self) {
        let nas_root = self.nas_root.clone();
        let client_root = self.client_root.clone();
        let watched = self.watched.clone();
        let echo = self.echo.clone();
        let cache = self.cache.clone();
        let shutdown = self.shutdown.clone();
        let dir_handle = self.dir_handle.clone();

        let handle = std::thread::Builder::new()
            .name("nas-watcher".into())
            .spawn(move || {
                if let Err(e) = run_watcher_loop(
                    &nas_root,
                    &client_root,
                    &watched,
                    &echo,
                    &cache,
                    &shutdown,
                    &dir_handle,
                ) {
                    if !shutdown.load(Ordering::Relaxed) {
                        log::error!("[sync-watcher] Watcher exited with error: {}", e);
                    }
                }
            })
            .expect("Failed to spawn NAS watcher thread");

        *self.thread.lock().unwrap() = Some(handle);
    }

    /// Stop the watcher thread.
    pub fn stop(&self) {
        log::info!("[sync-watcher] Stopping NAS watcher");
        self.shutdown.store(true, Ordering::SeqCst);
        self.cancel_io();
        self.join_thread(3);
    }

    /// Restart the watcher after a reconnect. Preserves the watched folder map.
    /// Runs full_diff on all watched folders to catch changes missed while offline.
    pub fn restart(&self) {
        log::info!("[sync-watcher] Restarting NAS watcher");
        // Ensure old thread is dead
        self.cancel_io();
        self.join_thread(1);
        // Reset for new thread
        self.shutdown.store(false, Ordering::SeqCst);
        self.dir_handle.store(0, Ordering::SeqCst);
        // Spawn new watcher — start() handles the rest
        self.start();
    }

    fn cancel_io(&self) {
        let h = self.dir_handle.load(Ordering::SeqCst);
        if h != 0 {
            let handle = HANDLE(h as *mut std::ffi::c_void);
            unsafe {
                let _ = windows::Win32::System::IO::CancelIoEx(handle, None);
            }
        }
    }

    fn join_thread(&self, timeout_secs: u64) {
        if let Some(t) = self.thread.lock().unwrap().take() {
            let start = std::time::Instant::now();
            loop {
                if t.is_finished() {
                    let _ = t.join();
                    break;
                }
                if start.elapsed() > std::time::Duration::from_secs(timeout_secs) {
                    log::warn!("[sync-watcher] NAS watcher thread join timed out");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn run_watcher_loop(
    nas_root: &Path,
    client_root: &Path,
    watched: &Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    echo: &Arc<EchoSuppressor>,
    cache: &Arc<CacheIndex>,
    shutdown: &AtomicBool,
    dir_handle_store: &AtomicUsize,
) -> Result<(), String> {
    let wide_path: Vec<u16> = nas_root
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            HANDLE::default(),
        )
    }
    .map_err(|e| format!("Failed to open NAS directory for watching: {}", e))?;

    // Store handle so stop() can cancel via CancelIoEx
    dir_handle_store.store(handle.0 as usize, Ordering::SeqCst);

    log::info!("[sync-watcher] Watching {:?}", nas_root);

    // Reconcile visited folders missed while offline (scoped to watched map)
    full_diff_all_watched(watched, echo, cache);

    let mut buffer = vec![0u8; 16384];
    let mut bytes_returned: u32 = 0;
    let filter =
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE
            | FILE_NOTIFY_CHANGE_LAST_WRITE;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let result = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                true, // watch subtree
                filter,
                Some(&mut bytes_returned),
                None,
                None,
            )
        };

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match result {
            Ok(()) if bytes_returned > 0 => {
                process_events(&buffer, bytes_returned, nas_root, client_root, echo, cache);
            }
            Ok(()) => {
                // Buffer overflow — only diff visited folders (safe on large shares)
                log::warn!("[sync-watcher] Buffer overflow, running full diff on visited folders");
                full_diff_all_watched(watched, echo, cache);
            }
            Err(e) => {
                if !shutdown.load(Ordering::Relaxed) {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return Err(format!("ReadDirectoryChangesW error: {}", e));
                }
                break;
            }
        }
    }

    unsafe {
        let _ = CloseHandle(handle);
    }
    log::info!("[sync-watcher] Watcher stopped");
    Ok(())
}

/// Process live events eagerly — derive client path via prefix swap, no map lookup.
fn process_events(
    buffer: &[u8],
    bytes_returned: u32,
    nas_root: &Path,
    client_root: &Path,
    echo: &EchoSuppressor,
    cache: &CacheIndex,
) {
    let mut offset: usize = 0;

    loop {
        if offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > bytes_returned as usize {
            break;
        }

        let info =
            unsafe { &*(buffer.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION) };

        let name_len = info.FileNameLength as usize / 2;
        let name_slice =
            unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_len) };
        let relative_str = String::from_utf16_lossy(name_slice);

        // Skip Synology internal paths and write-through temp files
        if relative_str
            .split('\\')
            .any(|c| c.starts_with('@') || c.starts_with('#') || c.contains(".~sync."))
        {
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
            continue;
        }

        let nas_path = nas_root.join(&relative_str);
        let client_path = client_root.join(&relative_str);
        let client_dir = client_path.parent().unwrap_or(client_root);
        let file_name = nas_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let action = info.Action.0;
        let action_name = match action {
            1 => "ADDED",
            2 => "REMOVED",
            3 => "MODIFIED",
            4 => "RENAMED_OLD",
            5 => "RENAMED_NEW",
            _ => "UNKNOWN",
        };

        match action {
            1 | 5 => {
                // FILE_ACTION_ADDED or FILE_ACTION_RENAMED_NEW — create placeholder
                if echo.is_suppressed(&nas_path) {
                    log::debug!("[sync-watcher] {} {} (echo suppressed)", action_name, relative_str);
                } else {
                    // Ensure parent directory exists locally (may not if user hasn't browsed there)
                    if !client_dir.exists() {
                        let _ = ensure_parent_placeholders(nas_root, client_root, client_dir, cache);
                    }
                    push_placeholder(&nas_path, client_dir, &file_name, &relative_str, cache);
                }
            }
            2 | 4 => {
                // FILE_ACTION_REMOVED or FILE_ACTION_RENAMED_OLD — remove placeholder
                if echo.is_suppressed(&nas_path) {
                    log::debug!("[sync-watcher] {} {} (echo suppressed)", action_name, relative_str);
                } else {
                    remove_placeholder(client_dir, &file_name, &relative_str, cache);
                }
            }
            3 => {
                // FILE_ACTION_MODIFIED — update placeholder metadata (file size)
                if !echo.is_suppressed(&nas_path) {
                    update_placeholder(&nas_path, client_dir, &file_name, &relative_str, cache);
                }
            }
            _ => {}
        }

        if info.NextEntryOffset == 0 {
            break;
        }
        offset += info.NextEntryOffset as usize;
    }
}

/// Ensure all parent directories exist as placeholders between client_root and target_dir.
/// Creates directory placeholders for any missing intermediate folders.
fn ensure_parent_placeholders(
    nas_root: &Path,
    client_root: &Path,
    target_dir: &Path,
    cache: &CacheIndex,
) -> Result<(), ()> {
    // Collect missing ancestors from target_dir up to client_root
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut dir = target_dir.to_path_buf();
    while dir != client_root && !dir.exists() {
        missing.push(dir.clone());
        dir = match dir.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
    }

    // Create from shallowest to deepest
    for dir in missing.into_iter().rev() {
        let relative = dir.strip_prefix(client_root).map_err(|_| ())?;
        let nas_dir = nas_root.join(relative);
        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let parent = dir.parent().unwrap_or(client_root);

        let blob = nas_dir.to_string_lossy().as_bytes().to_vec();
        let placeholder = PlaceholderFile::new(&dir_name)
            .metadata(Metadata::directory())
            .mark_in_sync()
            .blob(blob);
        match placeholder.create::<PathBuf>(parent.to_path_buf()) {
            Ok(_) => {
                log::debug!("[sync-watcher] + dir {}", relative.display());
            }
            Err(e) => {
                // Directory might have been created by CF API concurrently
                if !dir.exists() {
                    log::warn!(
                        "[sync-watcher] Failed to create dir placeholder {}: {}",
                        relative.display(),
                        e
                    );
                    return Err(());
                }
            }
        }
    }
    let _ = cache; // available for future folder tracking
    Ok(())
}

fn push_placeholder(nas_path: &Path, client_dir: &Path, file_name: &str, display: &str, cache: &CacheIndex) {
    let client_path = client_dir.join(file_name);
    if client_path.exists() {
        return;
    }

    if let Ok(meta) = fs::metadata(nas_path) {
        let blob = nas_path.to_string_lossy().as_bytes().to_vec();
        let cf_meta = if meta.is_dir() {
            Metadata::directory()
        } else {
            Metadata::file().size(meta.len())
        };
        let placeholder = PlaceholderFile::new(file_name)
            .metadata(cf_meta)
            .mark_in_sync()
            .blob(blob);
        match placeholder.create::<PathBuf>(client_dir.to_path_buf()) {
            Ok(_) => {
                log::info!("[sync-watcher] + {} ({})", display, meta.len());
                // Record in DB (files only)
                if !meta.is_dir() {
                    let entry_mtime = meta
                        .modified()
                        .map(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs() as i64
                        })
                        .unwrap_or(0);
                    cache.record_known_file(&client_path, meta.len(), entry_mtime);
                }
                // 0-byte placeholder — NAS might not send MODIFIED reliably.
                // Poll for the real size as a fallback.
                if !meta.is_dir() && meta.len() == 0 {
                    let nas = nas_path.to_path_buf();
                    let client = client_dir.join(file_name);
                    let name = display.to_string();
                    std::thread::spawn(move || {
                        deferred_size_update(&nas, &client, &name);
                    });
                }
            }
            Err(e) => log::warn!("[sync-watcher] Failed to create placeholder {}: {}", display, e),
        }
    }
}


fn remove_placeholder(client_dir: &Path, file_name: &str, display: &str, cache: &CacheIndex) {
    let client_path = client_dir.join(file_name);
    if !client_path.exists() {
        return;
    }

    let is_dir = client_path.is_dir();

    // Safety: only remove CF placeholders. Real files/directories that aren't
    // on the NAS should be left for write-through to upload, not deleted.
    if !is_dir && !super::cache::is_cf_placeholder(&client_path) {
        log::info!("[sync-watcher] Skipping removal of real file: {}", display);
        return;
    }

    let result = if is_dir {
        // Only remove empty placeholder directories — never remove_dir_all
        // which could destroy user content in subdirectories.
        fs::remove_dir(&client_path)
    } else {
        fs::remove_file(&client_path)
    };
    match result {
        Ok(()) => {
            log::debug!("[sync-watcher] - {}", display);
            if !is_dir {
                cache.remove_known_file(&client_path);
            }
        }
        // Access denied / not found / not empty is expected
        Err(e) => log::debug!("[sync-watcher] Remove skipped {}: {}", display, e),
    }
}

fn update_placeholder(nas_path: &Path, client_dir: &Path, file_name: &str, display: &str, cache: &CacheIndex) {
    let client_path = client_dir.join(file_name);
    if !client_path.exists() || client_path.is_dir() {
        return;
    }

    let nas_meta = match fs::metadata(nas_path) {
        Ok(m) => m,
        Err(_) => return,
    };

    let nas_size = nas_meta.len();
    if nas_size == 0 {
        return;
    }

    // Delete and recreate — CfUpdatePlaceholder doesn't reliably update
    // size on 0-byte placeholders via Win32 handles.
    let _ = fs::remove_file(&client_path);
    let blob = nas_path.to_string_lossy().as_bytes().to_vec();
    let cf_meta = Metadata::file().size(nas_size);
    let placeholder = PlaceholderFile::new(file_name)
        .metadata(cf_meta)
        .mark_in_sync()
        .blob(blob);
    match placeholder.create::<PathBuf>(client_dir.to_path_buf()) {
        Ok(_) => {
            log::info!("[sync-watcher] ~ {} ({})", display, nas_size);
            let entry_mtime = nas_meta
                .modified()
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                })
                .unwrap_or(0);
            cache.record_known_file(&client_path, nas_size, entry_mtime);
        }
        Err(e) => log::debug!("[sync-watcher] Update failed {}: {}", display, e),
    }
}

/// Fallback for 0-byte placeholders: poll NAS until the file has data, then update.
/// Used when Synology doesn't reliably send MODIFIED after a batch copy.
fn deferred_size_update(nas_path: &Path, client_path: &Path, display: &str) {
    let parent = match client_path.parent() {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    let file_name = match client_path.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return,
    };

    // Poll at increasing intervals up to ~65s total
    for delay_ms in [2000, 3000, 5000, 10000, 15000, 30000] {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if !client_path.exists() {
            return; // Placeholder was removed
        }
        if let Ok(meta) = fs::metadata(nas_path) {
            if meta.len() > 0 {
                // Delete the 0-byte placeholder and recreate with correct size.
                // CfUpdatePlaceholder doesn't reliably update size on 0-byte placeholders.
                let _ = fs::remove_file(client_path);
                let blob = nas_path.to_string_lossy().as_bytes().to_vec();
                let cf_meta = Metadata::file().size(meta.len());
                let placeholder = PlaceholderFile::new(&file_name)
                    .metadata(cf_meta)
                    .mark_in_sync()
                    .blob(blob);
                match placeholder.create::<PathBuf>(parent) {
                    Ok(_) => log::info!("[sync-watcher] Recreated placeholder: {} ({})", display, meta.len()),
                    Err(e) => log::warn!("[sync-watcher] Failed to recreate placeholder {}: {}", display, e),
                }
                return;
            }
        }
    }
    log::debug!("[sync-watcher] Deferred update gave up: {}", display);
}

/// Full readdir + diff on visited folders only. Used on buffer overflow and startup.
/// Scoped to the watched map to avoid walking an entire large share.
fn full_diff_all_watched(
    watched: &Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    echo: &EchoSuppressor,
    cache: &CacheIndex,
) {
    let folders: Vec<(PathBuf, PathBuf)> = {
        let map = watched.lock().unwrap();
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };

    for (nas_dir, client_dir) in folders {
        diff_folder(&nas_dir, &client_dir, echo, cache);
    }
}

/// Diff a single NAS folder against its client counterpart and reconcile.
pub fn diff_folder(nas_dir: &Path, client_dir: &Path, echo: &EchoSuppressor, cache: &CacheIndex) {
    if !client_dir.is_dir() || !nas_dir.is_dir() {
        return;
    }

    let nas_entries: std::collections::HashSet<String> = fs::read_dir(nas_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('#') && !n.starts_with('@') && !n.starts_with('.'))
        .collect();

    let client_entries: std::collections::HashSet<String> = fs::read_dir(client_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    // New on NAS
    for name in nas_entries.difference(&client_entries) {
        let nas_path = nas_dir.join(name);
        if !echo.is_suppressed(&nas_path) {
            push_placeholder(&nas_path, client_dir, name, name, cache);
        }
    }

    // Gone from NAS
    for name in client_entries.difference(&nas_entries) {
        remove_placeholder(client_dir, name, name, cache);
    }
}
