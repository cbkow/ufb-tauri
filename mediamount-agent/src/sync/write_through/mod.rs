/// Write-through: local saves in the sync root are uploaded to the NAS
/// and converted to hydrated placeholders.
///
/// Architecture:
///   Client watcher (blocking thread, ReadDirectoryChangesW on sync root)
///     -> Upload coordinator (async tokio task, debounce + state machine)
///       -> Upload worker (blocking thread, chunked SMB write + conflict check)
///         -> Placeholder conversion (convert local file to CF placeholder)

mod client_watcher;
mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub use client_watcher::ClientFsEvent;
pub use worker::{UploadJob, UploadResult};

// ── Echo suppressor ─────────────────────────────────────────────────

const ECHO_TTL: Duration = Duration::from_secs(5);

/// Prevents the NAS watcher from creating duplicate placeholders for files
/// we just uploaded. Upload worker writes; NAS watcher reads.
pub struct EchoSuppressor {
    entries: Mutex<HashMap<PathBuf, std::time::Instant>>,
}

impl EchoSuppressor {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Mark a NAS path as recently uploaded (suppressed for 5s).
    pub fn suppress(&self, nas_path: PathBuf) {
        let mut map = self.entries.lock().unwrap();
        map.insert(nas_path, std::time::Instant::now());
    }

    /// Check if a NAS path should be ignored. Also prunes expired entries.
    pub fn is_suppressed(&self, nas_path: &Path) -> bool {
        let mut map = self.entries.lock().unwrap();
        map.retain(|_, t| t.elapsed() < ECHO_TTL);
        map.contains_key(nas_path)
    }
}

// ── Per-file state machine ──────────────────────────────────────────

enum FileState {
    /// Waiting for quiescence (3s since last MODIFIED event).
    Debouncing {
        deadline: tokio::time::Instant,
    },
    /// Upload in progress. Cancel via the oneshot sender.
    Uploading {
        cancel_tx: oneshot::Sender<()>,
    },
}

// ── WriteThrough — owns all threads and the coordinator task ────────

/// Manages the write-through pipeline for a single sync root.
pub struct WriteThrough {
    shutdown: Arc<AtomicBool>,
    /// Client watcher directory handle stored as usize for Send safety.
    /// HANDLE is just an opaque Win32 identifier, safe to use from any thread.
    client_dir_handle: Arc<AtomicUsize>,
    watcher_thread: Option<std::thread::JoinHandle<()>>,
    worker_thread: Option<std::thread::JoinHandle<()>>,
    coordinator_task: Option<tokio::task::JoinHandle<()>>,
}

impl WriteThrough {
    /// Start the write-through pipeline. Must be called from a tokio context
    /// (e.g., inside spawn_blocking from a tokio runtime).
    pub fn start(
        client_root: PathBuf,
        nas_root: PathBuf,
        echo: Arc<EchoSuppressor>,
    ) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let client_dir_handle = Arc::new(AtomicUsize::new(0));

        // Client watcher → coordinator
        let (event_tx, event_rx) = mpsc::channel::<ClientFsEvent>(256);
        // Coordinator → upload worker
        let (job_tx, job_rx) = std::sync::mpsc::channel::<UploadJob>();
        // Upload worker → coordinator
        let (result_tx, result_rx) = mpsc::channel::<UploadResult>(64);

        let watcher_thread = client_watcher::start(
            client_root.clone(),
            event_tx,
            shutdown.clone(),
            client_dir_handle.clone(),
        );

        let worker_thread = worker::start(job_rx, result_tx, echo.clone());

        let handle = tokio::runtime::Handle::current();
        let cr = client_root.clone();
        let nr = nas_root.clone();
        let coordinator_task = handle.spawn(run_coordinator(
            cr, nr, event_rx, job_tx, result_rx, echo,
        ));

        log::info!(
            "[write-through] Started for {:?} → {:?}",
            client_root,
            nas_root
        );

        Self {
            shutdown,
            client_dir_handle,
            watcher_thread: Some(watcher_thread),
            worker_thread: Some(worker_thread),
            coordinator_task: Some(coordinator_task),
        }
    }

    /// Stop the write-through pipeline.
    pub fn stop(&mut self) {
        log::info!("[write-through] Stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        // Cancel pending ReadDirectoryChangesW on the client watcher
        let h = self.client_dir_handle.load(Ordering::SeqCst);
        if h != 0 {
            let handle = windows::Win32::Foundation::HANDLE(h as *mut std::ffi::c_void);
            unsafe {
                let _ = windows::Win32::System::IO::CancelIoEx(handle, None);
            }
        }

        // Abort the coordinator (drops channels, which stops the worker)
        if let Some(task) = self.coordinator_task.take() {
            task.abort();
        }

        // Wait for threads (with timeout — don't block shutdown)
        for (name, thread) in [
            ("client-watcher", self.watcher_thread.take()),
            ("upload-worker", self.worker_thread.take()),
        ] {
            if let Some(t) = thread {
                let start = std::time::Instant::now();
                loop {
                    if t.is_finished() {
                        let _ = t.join();
                        break;
                    }
                    if start.elapsed() > std::time::Duration::from_secs(3) {
                        log::warn!("[write-through] {} thread join timed out", name);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        log::info!("[write-through] Stopped");
    }
}

impl Drop for WriteThrough {
    fn drop(&mut self) {
        if !self.shutdown.load(Ordering::Relaxed) {
            self.stop();
        }
    }
}

// ── Coordinator ─────────────────────────────────────────────────────

const DEBOUNCE_SECS: u64 = 3;
const TICK_MS: u64 = 500;
const CONVERSION_SUPPRESS_SECS: u64 = 2;

/// Async coordinator: receives client FS events, manages debounce timers,
/// dispatches upload jobs, handles results.
async fn run_coordinator(
    client_root: PathBuf,
    nas_root: PathBuf,
    mut event_rx: mpsc::Receiver<ClientFsEvent>,
    job_tx: std::sync::mpsc::Sender<UploadJob>,
    mut result_rx: mpsc::Receiver<UploadResult>,
    echo: Arc<EchoSuppressor>,
) {
    let mut states: HashMap<PathBuf, FileState> = HashMap::new();
    // Paths recently converted to placeholders — ignore events from our own conversion
    let mut recently_converted: HashMap<PathBuf, std::time::Instant> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(TICK_MS));

    // Startup recovery: queue any non-placeholder files already in sync root
    startup_recovery(&client_root, &nas_root, &job_tx, &echo).await;

    loop {
        tokio::select! {
            event = event_rx.recv() => {
                match event {
                    Some(ClientFsEvent::Modified(path)) => {
                        // Skip the first event after our own placeholder conversion
                        recently_converted.retain(|_, t| t.elapsed() < Duration::from_secs(CONVERSION_SUPPRESS_SECS));
                        if recently_converted.remove(&path).is_some() {
                            log::debug!("[write-through] Ignoring post-conversion event: {:?}", path);
                        } else {
                            on_modified(&mut states, &path);
                        }
                    }
                    Some(ClientFsEvent::Removed(path)) => {
                        if let Some(FileState::Uploading { cancel_tx }) = states.remove(&path) {
                            let _ = cancel_tx.send(());
                        }
                    }
                    None => break,
                }
            }
            result = result_rx.recv() => {
                match result {
                    Some(r) => on_upload_result(&mut states, r, &mut recently_converted).await,
                    None => break,
                }
            }
            _ = tick.tick() => {
                check_deadlines(&mut states, &client_root, &nas_root, &job_tx);
            }
        }
    }

    log::info!("[write-through] Coordinator exiting");
}

/// Handle a MODIFIED event: set or reset the debounce timer.
fn on_modified(states: &mut HashMap<PathBuf, FileState>, path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(DEBOUNCE_SECS);

    match states.get(path) {
        None | Some(FileState::Debouncing { .. }) => {
            // Start or reset debounce
            states.insert(path.to_path_buf(), FileState::Debouncing { deadline });
        }
        Some(FileState::Uploading { .. }) => {
            // Cancel active upload, reset to debounce
            if let Some(FileState::Uploading { cancel_tx }) = states.remove(path) {
                let _ = cancel_tx.send(());
            }
            states.insert(path.to_path_buf(), FileState::Debouncing { deadline });
        }
    }
}

/// Check debounce deadlines and dispatch upload jobs for expired ones.
fn check_deadlines(
    states: &mut HashMap<PathBuf, FileState>,
    client_root: &Path,
    nas_root: &Path,
    job_tx: &std::sync::mpsc::Sender<UploadJob>,
) {
    let now = tokio::time::Instant::now();
    let mut ready: Vec<PathBuf> = Vec::new();

    for (path, state) in states.iter() {
        if let FileState::Debouncing { deadline } = state {
            if now >= *deadline {
                ready.push(path.clone());
            }
        }
    }

    for path in ready {
        let nas_path = to_nas_path(&path, client_root, nas_root);
        let (cancel_tx, cancel_rx) = oneshot::channel();

        let job = UploadJob {
            local_path: path.clone(),
            nas_path,
            cancel_rx,
        };

        if job_tx.send(job).is_ok() {
            states.insert(path, FileState::Uploading { cancel_tx });
        } else {
            // Worker channel closed
            states.remove(&path);
        }
    }
}

/// Handle upload result: convert to placeholder on success.
async fn on_upload_result(
    states: &mut HashMap<PathBuf, FileState>,
    result: UploadResult,
    recently_converted: &mut HashMap<PathBuf, std::time::Instant>,
) {
    match result {
        UploadResult::Success {
            ref local_path,
            ref nas_path,
        } => {
            states.remove(local_path);
            // Suppress post-conversion events for this path
            recently_converted.insert(local_path.clone(), std::time::Instant::now());
            // Convert local file to hydrated placeholder (blocking Win32 call)
            let lp = local_path.clone();
            let np = nas_path.clone();
            tokio::task::spawn_blocking(move || {
                convert_to_placeholder(&lp, &np);
            })
            .await
            .ok();
        }
        UploadResult::Cancelled { ref local_path } => {
            // State was already reset by the event that triggered cancellation
            log::debug!("[write-through] Upload cancelled: {:?}", local_path);
        }
        UploadResult::Failed {
            ref local_path,
            ref error,
        } => {
            log::error!(
                "[write-through] Upload failed for {:?}: {}",
                local_path,
                error
            );
            // Reset to idle — will retry on next modification
            states.remove(local_path);
        }
    }
}

/// Convert a local file to a hydrated CF placeholder, or re-mark an
/// existing placeholder as in-sync after upload.
fn convert_to_placeholder(local_path: &Path, nas_path: &Path) {
    let blob = nas_path.to_string_lossy().as_bytes().to_vec();

    // Check if already a placeholder (reparse point)
    let is_placeholder = {
        let wide: Vec<u16> = local_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let attrs = unsafe {
            windows::Win32::Storage::FileSystem::GetFileAttributesW(
                windows::core::PCWSTR(wide.as_ptr()),
            )
        };
        attrs != u32::MAX && (attrs & 0x400) != 0 // FILE_ATTRIBUTE_REPARSE_POINT = 0x400
    };

    if is_placeholder {
        // Already a placeholder — just mark as in-sync with updated blob
        match cloud_filter::placeholder::Placeholder::open(local_path) {
            Ok(mut ph) => {
                match ph.mark_in_sync(true, None) {
                    Ok(_) => {
                        log::info!("[write-through] Re-synced placeholder: {:?}", local_path);
                    }
                    Err(e) => {
                        log::warn!(
                            "[write-through] Failed to mark in-sync {:?}: {}",
                            local_path, e
                        );
                    }
                }
            }
            Err(e) => {
                log::warn!(
                    "[write-through] Failed to open placeholder {:?}: {}",
                    local_path, e
                );
            }
        }
    } else {
        // Regular file — convert to placeholder
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(local_path)
        {
            Ok(f) => f,
            Err(e) => {
                log::warn!(
                    "[write-through] Failed to open for conversion {:?}: {}",
                    local_path, e
                );
                return;
            }
        };

        let mut ph: cloud_filter::placeholder::Placeholder = file.into();
        let opts = cloud_filter::placeholder::ConvertOptions::default()
            .mark_in_sync()
            .blob(blob);

        match ph.convert_to_placeholder(opts, None) {
            Ok(_) => {
                log::info!("[write-through] Converted to placeholder: {:?}", local_path);
            }
            Err(e) => {
                log::warn!(
                    "[write-through] Placeholder conversion failed {:?}: {}",
                    local_path, e
                );
            }
        }
    }
}

/// Map a client path to its NAS counterpart.
fn to_nas_path(local: &Path, client_root: &Path, nas_root: &Path) -> PathBuf {
    let relative = local.strip_prefix(client_root).unwrap_or(Path::new(""));
    nas_root.join(relative)
}

/// Startup recovery: clean orphaned temp files on NAS and queue
/// any non-placeholder files in the sync root for upload.
async fn startup_recovery(
    client_root: &PathBuf,
    nas_root: &PathBuf,
    job_tx: &std::sync::mpsc::Sender<UploadJob>,
    _echo: &Arc<EchoSuppressor>,
) {
    let cr = client_root.clone();
    let nr = nas_root.clone();
    let tx = job_tx.clone();

    let result = tokio::task::spawn_blocking(move || {
        do_startup_recovery(&cr, &nr, &tx);
    })
    .await;

    if let Err(e) = result {
        log::warn!("[write-through] Startup recovery task failed: {}", e);
    }
}

fn do_startup_recovery(
    client_root: &Path,
    nas_root: &Path,
    job_tx: &std::sync::mpsc::Sender<UploadJob>,
) {
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into());

    // 1. Clean orphaned temp files on NAS
    clean_orphaned_temps(nas_root, &hostname);

    // 2. Queue non-placeholder files for upload
    queue_non_placeholders(client_root, client_root, nas_root, job_tx);
}

fn clean_orphaned_temps(nas_dir: &Path, hostname: &str) {
    let pattern = format!(".~sync.{}", hostname);
    let entries = match std::fs::read_dir(nas_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(&pattern) {
            log::info!("[write-through] Cleaning orphaned temp: {:?}", entry.path());
            let _ = std::fs::remove_file(entry.path());
        }
        // Recurse into subdirectories
        if entry.path().is_dir() && !name.starts_with('#') && !name.starts_with('@') {
            clean_orphaned_temps(&entry.path(), hostname);
        }
    }
}

fn queue_non_placeholders(
    client_root: &Path,
    scan_dir: &Path,
    nas_root: &Path,
    job_tx: &std::sync::mpsc::Sender<UploadJob>,
) {
    let entries = match std::fs::read_dir(scan_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();

        // Skip filtered names
        if name.contains(".~sync.") || name.starts_with("~$") || name.ends_with(".tmp") {
            continue;
        }

        if path.is_dir() {
            queue_non_placeholders(client_root, &path, nas_root, job_tx);
        } else if client_watcher::is_file(&path) {
            let relative = path.strip_prefix(client_root).unwrap_or(Path::new(""));
            let nas_path = nas_root.join(relative);
            let (_cancel_tx, cancel_rx) = oneshot::channel();
            let job = UploadJob {
                local_path: path,
                nas_path,
                cancel_rx,
            };
            let _ = job_tx.send(job);
        }
    }
}
