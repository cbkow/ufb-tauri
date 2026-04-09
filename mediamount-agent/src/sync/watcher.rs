/// NAS directory watcher via ReadDirectoryChangesW.
///
/// Watches the NAS root with subtree support. When a change is detected in a
/// folder that the user has visited (registered during FETCH_PLACEHOLDERS),
/// the watcher pushes/removes placeholders in the corresponding client folder.

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

use super::write_through::EchoSuppressor;

/// Watches a NAS share for changes and syncs placeholders to the client folder.
pub struct NasWatcher {
    /// Map of NAS folder → client folder for all visited folders.
    watched: Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    nas_root: PathBuf,
    /// Echo suppression — skip placeholders for files we just uploaded.
    echo: Arc<EchoSuppressor>,
    /// Shutdown signal.
    shutdown: Arc<AtomicBool>,
    /// Directory handle stored as usize for CancelIoEx on shutdown.
    dir_handle: Arc<AtomicUsize>,
    /// Thread handle for join on stop.
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl NasWatcher {
    pub fn new(nas_root: PathBuf, echo: Arc<EchoSuppressor>) -> Self {
        Self {
            watched: Arc::new(Mutex::new(HashMap::new())),
            nas_root,
            echo,
            shutdown: Arc::new(AtomicBool::new(false)),
            dir_handle: Arc::new(AtomicUsize::new(0)),
            thread: Mutex::new(None),
        }
    }

    /// Register a folder pair for watching. Called when FETCH_PLACEHOLDERS fires.
    pub fn register(&self, nas_dir: PathBuf, client_dir: PathBuf) {
        let mut map = self.watched.lock().unwrap();
        map.insert(nas_dir, client_dir);
    }

    /// Start the background watcher thread.
    pub fn start(&self) {
        let nas_root = self.nas_root.clone();
        let watched = self.watched.clone();
        let echo = self.echo.clone();
        let shutdown = self.shutdown.clone();
        let dir_handle = self.dir_handle.clone();

        let handle = std::thread::Builder::new()
            .name("nas-watcher".into())
            .spawn(move || {
                if let Err(e) = run_watcher_loop(&nas_root, &watched, &echo, &shutdown, &dir_handle) {
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
    watched: &Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    echo: &Arc<EchoSuppressor>,
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

    // Reconcile any changes missed while offline (e.g., after a reconnect)
    full_diff_all_watched(nas_root, watched, echo);

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
                process_events(&buffer, bytes_returned, nas_root, watched, echo);
            }
            Ok(()) => {
                log::warn!("[sync-watcher] Buffer overflow, running full diff on watched folders");
                full_diff_all_watched(nas_root, watched, echo);
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

fn process_events(
    buffer: &[u8],
    bytes_returned: u32,
    nas_root: &Path,
    watched: &Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    echo: &EchoSuppressor,
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
        // Normalize parent: strip trailing backslash for consistent map lookup.
        // Path::parent() on UNC paths like \\server\share\file returns \\server\share\
        // but the watched map stores \\server\share (no trailing separator).
        let raw_parent = nas_path.parent().unwrap_or(nas_root);
        let parent_str = raw_parent.to_string_lossy();
        let parent_nas = if parent_str.ends_with('\\') || parent_str.ends_with('/') {
            PathBuf::from(parent_str.trim_end_matches(|c| c == '\\' || c == '/'))
        } else {
            raw_parent.to_path_buf()
        };
        let parent_nas = parent_nas.as_path();
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

        let client_dir = {
            let map = watched.lock().unwrap();
            let result = map.get(parent_nas).cloned();
            if result.is_none() {
                log::debug!(
                    "[sync-watcher] {} {} (no watched folder for {:?})",
                    action_name, relative_str, parent_nas
                );
            }
            result
        };

        if let Some(client_dir) = client_dir {
            match action {
                1 | 5 => {
                    // FILE_ACTION_ADDED or FILE_ACTION_RENAMED_NEW — create placeholder
                    if echo.is_suppressed(&nas_path) {
                        log::debug!("[sync-watcher] {} {} (echo suppressed)", action_name, relative_str);
                    } else {
                        log::debug!("[sync-watcher] {} {}", action_name, relative_str);
                        push_placeholder(&nas_path, &client_dir, &file_name, &relative_str);
                    }
                }
                2 | 4 => {
                    // FILE_ACTION_REMOVED or FILE_ACTION_RENAMED_OLD — remove placeholder
                    if echo.is_suppressed(&nas_path) {
                        log::debug!("[sync-watcher] {} {} (echo suppressed)", action_name, relative_str);
                    } else {
                        log::debug!("[sync-watcher] {} {}", action_name, relative_str);
                        remove_placeholder(&client_dir, &file_name, &relative_str);
                    }
                }
                3 => {
                    // FILE_ACTION_MODIFIED — update placeholder metadata (file size)
                    if !echo.is_suppressed(&nas_path) {
                        update_placeholder(&nas_path, &client_dir, &file_name, &relative_str);
                    }
                }
                _ => {}
            }
        }

        if info.NextEntryOffset == 0 {
            break;
        }
        offset += info.NextEntryOffset as usize;
    }
}

fn push_placeholder(nas_path: &Path, client_dir: &Path, file_name: &str, display: &str) {
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
                log::debug!("[sync-watcher] + {} ({})", display, meta.len());
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


fn remove_placeholder(client_dir: &Path, file_name: &str, display: &str) {
    let client_path = client_dir.join(file_name);
    if !client_path.exists() {
        return;
    }

    let result = if client_path.is_dir() {
        fs::remove_dir_all(&client_path)
    } else {
        fs::remove_file(&client_path)
    };
    match result {
        Ok(()) => log::debug!("[sync-watcher] - {}", display),
        // Access denied / not found is expected — the CF API delete callback
        // may have already removed it, or it's still being processed.
        Err(e) => log::debug!("[sync-watcher] Remove skipped {}: {}", display, e),
    }
}

fn update_placeholder(nas_path: &Path, client_dir: &Path, file_name: &str, display: &str) {
    let client_path = client_dir.join(file_name);
    if !client_path.exists() || client_path.is_dir() {
        return;
    }

    let nas_size = match fs::metadata(nas_path) {
        Ok(m) => m.len(),
        Err(_) => return,
    };

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
        Ok(_) => log::info!("[sync-watcher] ~ {} ({})", display, nas_size),
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

/// Full readdir + diff on all watched folders. Used when the event buffer overflows.
fn full_diff_all_watched(
    _nas_root: &Path,
    watched: &Arc<Mutex<HashMap<PathBuf, PathBuf>>>,
    echo: &EchoSuppressor,
) {
    let folders: Vec<(PathBuf, PathBuf)> = {
        let map = watched.lock().unwrap();
        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };

    for (nas_dir, client_dir) in folders {
        diff_folder(&nas_dir, &client_dir, echo);
    }
}

/// Diff a single NAS folder against its client counterpart and reconcile.
fn diff_folder(nas_dir: &Path, client_dir: &Path, echo: &EchoSuppressor) {
    if !client_dir.is_dir() || !nas_dir.is_dir() {
        return;
    }

    let nas_entries: std::collections::HashSet<String> = fs::read_dir(nas_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('#') && !n.starts_with('@'))
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
            push_placeholder(&nas_path, client_dir, name, name);
        }
    }

    // Gone from NAS
    for name in client_entries.difference(&nas_entries) {
        remove_placeholder(client_dir, name, name);
    }
}
