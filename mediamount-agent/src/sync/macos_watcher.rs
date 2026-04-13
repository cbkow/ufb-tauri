/// NAS directory watcher for macOS via FSEvents + periodic poll fallback.
///
/// Watches `/Volumes/{share}` for changes and posts Darwin notifications
/// to signal the FileProvider extension to re-enumerate.
///
/// FSEvents on SMB mounts can miss events or batch them with long delays,
/// so a 30-second poll on visited folders provides a safety net.

use notify::{RecommendedWatcher, RecursiveMode, Watcher, Event};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Posts Darwin notifications when NAS contents change.
/// The FileProvider extension listens and calls signalEnumerator().
pub struct MacosNasWatcher {
    nas_root: PathBuf,
    domain: String,
    shutdown: Arc<AtomicBool>,
    echo: Arc<EchoSuppressor>,
    /// Folders with detected changes — consumed by get_changes_since().
    dirty_folders: Arc<Mutex<HashSet<String>>>,
    /// Cached folder state for poll fallback: path → (entry_count, dir_mtime)
    folder_state: Arc<Mutex<HashMap<PathBuf, FolderSnapshot>>>,
    watcher_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    poll_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Clone)]
struct FolderSnapshot {
    entry_count: usize,
    mtime: std::time::SystemTime,
}

impl MacosNasWatcher {
    pub fn new(nas_root: PathBuf, domain: String) -> Self {
        Self {
            nas_root,
            domain,
            shutdown: Arc::new(AtomicBool::new(false)),
            echo: Arc::new(EchoSuppressor::new()),
            dirty_folders: Arc::new(Mutex::new(HashSet::new())),
            folder_state: Arc::new(Mutex::new(HashMap::new())),
            watcher_thread: Mutex::new(None),
            poll_thread: Mutex::new(None),
        }
    }

    /// Get a reference to the echo suppressor (for the fileops server to use).
    pub fn echo_suppressor(&self) -> Arc<EchoSuppressor> {
        self.echo.clone()
    }

    /// Get a reference to the dirty folders set (for the cache to consume).
    pub fn dirty_folders(&self) -> Arc<Mutex<HashSet<String>>> {
        Arc::clone(&self.dirty_folders)
    }

    pub fn start(&self) {
        self.start_fsevents();
        self.start_poll();
    }

    pub fn stop(&self) {
        log::info!("[macos-watcher] Stopping NAS watcher for {}", self.domain);
        self.shutdown.store(true, Ordering::SeqCst);

        // Join threads
        if let Some(t) = self.watcher_thread.lock().unwrap().take() {
            let _ = t.join();
        }
        if let Some(t) = self.poll_thread.lock().unwrap().take() {
            let _ = t.join();
        }
    }

    pub fn restart(&self) {
        self.stop();
        self.shutdown.store(false, Ordering::SeqCst);
        self.start();
    }

    fn start_fsevents(&self) {
        let nas_root = self.nas_root.clone();
        let domain = self.domain.clone();
        let shutdown = self.shutdown.clone();
        let echo = Arc::clone(&self.echo);
        let dirty_folders = Arc::clone(&self.dirty_folders);

        let handle = std::thread::Builder::new()
            .name(format!("fsevents-{}", domain))
            .spawn(move || {
                run_fsevents_loop(&nas_root, &domain, &shutdown, &echo, &dirty_folders);
            })
            .expect("Failed to spawn FSEvents watcher thread");

        *self.watcher_thread.lock().unwrap() = Some(handle);
    }

    fn start_poll(&self) {
        let nas_root = self.nas_root.clone();
        let domain = self.domain.clone();
        let shutdown = self.shutdown.clone();
        let folder_state = self.folder_state.clone();
        let echo = Arc::clone(&self.echo);

        let handle = std::thread::Builder::new()
            .name(format!("poll-{}", domain))
            .spawn(move || {
                run_poll_loop(&nas_root, &domain, &shutdown, &folder_state, &echo);
            })
            .expect("Failed to spawn poll watcher thread");

        *self.poll_thread.lock().unwrap() = Some(handle);
    }
}

/// FSEvents-based watcher using the `notify` crate.
fn run_fsevents_loop(
    nas_root: &Path,
    domain: &str,
    shutdown: &AtomicBool,
    echo: &Arc<EchoSuppressor>,
    dirty_folders: &Arc<Mutex<HashSet<String>>>,
) {
    let domain_owned = domain.to_string();
    let echo_clone = Arc::clone(echo);
    let nas_root_clone = nas_root.to_path_buf();
    let dirty_folders_clone = Arc::clone(dirty_folders);

    // Debounce: track last notification time
    let last_notify = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
    let last_notify_clone = last_notify.clone();

    let mut watcher = match RecommendedWatcher::new(
        move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                // Filter out noise
                for path in &event.paths {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with('@')
                            || name.starts_with('#')
                            || name == ".DS_Store"
                            || name.contains(".~sync.")
                        {
                            return;
                        }
                    }

                    // Check echo suppression
                    let relative = path.strip_prefix(&nas_root_clone).unwrap_or(path);
                    if echo_clone.is_suppressed(relative) {
                        log::debug!("[macos-watcher] FSEvent suppressed: {}", relative.display());
                        return;
                    }
                }

                // Record dirty folders from event paths
                for path in &event.paths {
                    // Get the parent folder as a relative path from NAS root
                    let parent = if path.is_dir() { path.clone() } else {
                        path.parent().unwrap_or(path).to_path_buf()
                    };
                    if let Ok(relative) = parent.strip_prefix(&nas_root_clone) {
                        let rel_str = relative.to_string_lossy().to_string();
                        dirty_folders_clone.lock().unwrap().insert(rel_str);
                    }
                }

                // Debounce: only notify once per 500ms window
                let mut last = last_notify_clone.lock().unwrap();
                if last.elapsed() > Duration::from_millis(500) {
                    *last = Instant::now();
                    log::info!("[macos-watcher] FSEvent detected, notifying {}", domain_owned);
                    post_darwin_notification(&domain_owned);
                } else {
                    log::debug!("[macos-watcher] FSEvent debounced for {}", domain_owned);
                }
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            log::error!("[macos-watcher] Failed to create FSEvents watcher: {}", e);
            return;
        }
    };

    if let Err(e) = watcher.watch(nas_root, RecursiveMode::Recursive) {
        log::error!("[macos-watcher] Failed to watch {:?}: {}", nas_root, e);
        return;
    }

    log::info!("[macos-watcher] FSEvents watching {:?} for domain {}", nas_root, domain);

    // Keep the watcher alive until shutdown
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
    }

    log::info!("[macos-watcher] FSEvents watcher stopped for {}", domain);
}

/// Periodic poll fallback — checks visited folders every 30 seconds.
fn run_poll_loop(
    nas_root: &Path,
    domain: &str,
    shutdown: &AtomicBool,
    folder_state: &Mutex<HashMap<PathBuf, FolderSnapshot>>,
    _echo: &Arc<EchoSuppressor>,
) {
    log::info!("[macos-watcher] Poll fallback started for {} (5s interval)", domain);

    // Seed with root folder
    if let Some(snapshot) = snapshot_folder(nas_root) {
        folder_state.lock().unwrap().insert(nas_root.to_path_buf(), snapshot);
    }

    loop {
        // Sleep in small increments so we can check shutdown (5s = 10 x 500ms)
        for _ in 0..10 {
            if shutdown.load(Ordering::Relaxed) {
                log::info!("[macos-watcher] Poll fallback stopped for {}", domain);
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        log::info!("[macos-watcher] Poll cycle for {}", domain);
        let mut changed = false;
        let mut state = folder_state.lock().unwrap();

        // Check root and any previously seen folders
        let folders: Vec<PathBuf> = state.keys().cloned().collect();
        for folder in folders {
            if let Some(new_snapshot) = snapshot_folder(&folder) {
                if let Some(old_snapshot) = state.get(&folder) {
                    if new_snapshot.entry_count != old_snapshot.entry_count
                        || new_snapshot.mtime != old_snapshot.mtime
                    {
                        log::info!(
                            "[macos-watcher] Poll detected change in {:?} (entries: {} → {}, mtime changed: {})",
                            folder,
                            old_snapshot.entry_count,
                            new_snapshot.entry_count,
                            new_snapshot.mtime != old_snapshot.mtime
                        );
                        changed = true;
                    }
                }
                state.insert(folder, new_snapshot);
            }
        }

        // Also check root even if not in map yet
        if !state.contains_key(nas_root) {
            if let Some(snapshot) = snapshot_folder(nas_root) {
                state.insert(nas_root.to_path_buf(), snapshot);
            }
        }

        drop(state);

        if changed {
            post_darwin_notification(domain);
        }
    }
}

/// Take a lightweight snapshot of a folder: entry count + mtime.
fn snapshot_folder(path: &Path) -> Option<FolderSnapshot> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let count = std::fs::read_dir(path)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            !n.starts_with('.') && !n.starts_with('@') && !n.starts_with('#')
        })
        .count();
    Some(FolderSnapshot {
        entry_count: count,
        mtime,
    })
}

/// Post a distributed notification that the FileProvider extension listens for.
/// Uses CFNotificationCenter with the distributed center (cross-process, works in sandboxed extensions).
pub(crate) fn post_darwin_notification(domain: &str) {
    let name = format!("com.unionfiles.ufb.nas-changed.{}", domain);
    log::info!("[macos-watcher] Posting notification: {}", name);

    // Use Core Foundation distributed notification center
    extern "C" {
        fn CFNotificationCenterGetDistributedCenter() -> *const std::ffi::c_void;
        fn CFNotificationCenterPostNotification(
            center: *const std::ffi::c_void,
            name: *const std::ffi::c_void,
            object: *const std::ffi::c_void,
            user_info: *const std::ffi::c_void,
            deliver_immediately: bool,
        );
        fn CFStringCreateWithCString(
            alloc: *const std::ffi::c_void,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    let c_name = std::ffi::CString::new(name).unwrap();
    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        let cf_name = CFStringCreateWithCString(std::ptr::null(), c_name.as_ptr(), 0x08000100); // kCFStringEncodingUTF8
        CFNotificationCenterPostNotification(center, cf_name, std::ptr::null(), std::ptr::null(), true);
        CFRelease(cf_name);
    }
}

/// Post a "clear cache" notification for a domain.
/// The extension will evict all materialized files.
pub fn post_clear_cache_notification(domain: &str) {
    let name = format!("com.unionfiles.ufb.clear-cache.{}", domain);
    log::info!("[macos-watcher] Posting clear cache notification: {}", name);

    extern "C" {
        fn CFNotificationCenterGetDistributedCenter() -> *const std::ffi::c_void;
        fn CFNotificationCenterPostNotification(
            center: *const std::ffi::c_void,
            name: *const std::ffi::c_void,
            object: *const std::ffi::c_void,
            user_info: *const std::ffi::c_void,
            deliver_immediately: bool,
        );
        fn CFStringCreateWithCString(
            alloc: *const std::ffi::c_void,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const std::ffi::c_void;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    let c_name = std::ffi::CString::new(name).unwrap();
    unsafe {
        let center = CFNotificationCenterGetDistributedCenter();
        let cf_name = CFStringCreateWithCString(std::ptr::null(), c_name.as_ptr(), 0x08000100);
        CFNotificationCenterPostNotification(center, cf_name, std::ptr::null(), std::ptr::null(), true);
        CFRelease(cf_name);
    }
}

/// Echo suppressor — prevents our own writes from triggering re-enumeration.
/// Thread-safe HashMap of path → expiry time.
pub struct EchoSuppressor {
    entries: Mutex<HashMap<PathBuf, Instant>>,
}

impl EchoSuppressor {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Mark a path as suppressed for the next 5 seconds.
    pub fn suppress(&self, path: &Path) {
        let mut map = self.entries.lock().unwrap();
        map.insert(path.to_path_buf(), Instant::now() + Duration::from_secs(5));
        // Clean expired entries
        map.retain(|_, expiry| *expiry > Instant::now());
    }

    /// Check if a path is currently suppressed.
    pub fn is_suppressed(&self, path: &Path) -> bool {
        let map = self.entries.lock().unwrap();
        map.get(path)
            .map(|expiry| *expiry > Instant::now())
            .unwrap_or(false)
    }

}
