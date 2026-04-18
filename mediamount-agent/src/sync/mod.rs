/// On-demand NAS sync via native OS user-mode filesystem drivers.
///
/// Windows: WinFsp via the `winfsp` crate.
/// macOS: NFS3 loopback server via the `nfsserve` crate.
///
/// The sync module presents NAS files as virtual entries in the local filesystem.
/// Each user-space callback (open, read, readdir, write) is answered on the fly —
/// serve from local cache when warm, pass through to SMB otherwise. No persistent
/// placeholder state; no reconciliation database after offline periods.

pub mod cache_core;

#[cfg(any(windows, target_os = "macos"))]
pub mod nas_health;

#[cfg(windows)]
pub mod windows_cache;
#[cfg(windows)]
pub mod connectivity;
#[cfg(windows)]
pub mod winfsp_server;

#[cfg(any(windows, target_os = "macos"))]
pub mod conflict;
#[cfg(target_os = "macos")]
pub mod macos_watcher;
#[cfg(target_os = "macos")]
pub mod macos_cache;
#[cfg(target_os = "macos")]
pub mod nfs_server;
#[cfg(target_os = "macos")]
pub use macos_watcher::MacosNasWatcher;
#[cfg(target_os = "macos")]
pub use macos_cache::MacosCache;
#[cfg(windows)]
pub use windows_cache::CacheIndex;
#[cfg(windows)]
pub use connectivity::{NasConnectivity, NasStatus};

/// Per-domain cache map shared between main and the NFS server. Keyed by
/// share name. Readers: NFS server startup, mount_service drain/stats.
/// Writers: main, on first hydration of a new mount.
#[cfg(target_os = "macos")]
pub type SharedCaches = std::sync::Arc<
    std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<MacosCache>>>,
>;

/// Per-domain cache map for Windows WinFsp mounts. Same pattern as macOS.
#[cfg(windows)]
pub type SharedCaches = std::sync::Arc<
    std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<CacheIndex>>>,
>;

/// Handle returned from `nfs_server::start` / `winfsp_server::start` for a
/// single per-domain sync server. Carries the shutdown primitive and the
/// task/thread join handle so callers can tear down the server cleanly
/// (e.g. on cache-root change or agent quit).
///
/// Dropping a `SyncServerHandle` without calling `shutdown_and_wait` does
/// NOT shut down the server — the shutdown signal must be sent explicitly.
/// This is intentional: the default process lifetime behavior (park
/// forever) should not regress just because someone forgot to hold the
/// handle somewhere.
#[cfg(any(target_os = "macos", windows))]
pub struct SyncServerHandle {
    pub domain: String,
    #[cfg(target_os = "macos")]
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    #[cfg(target_os = "macos")]
    task_handle: tokio::task::JoinHandle<()>,
    #[cfg(windows)]
    shutdown_tx: std::sync::mpsc::Sender<()>,
    #[cfg(windows)]
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

/// Shared registry of live sync servers, keyed by domain/share. Written by
/// `main.rs` as servers come up; drained by `MountService` on cache-root
/// change (each handle gets `shutdown_and_wait` then the map is cleared).
/// `tokio::Mutex` because teardown happens inside an async context.
#[cfg(any(target_os = "macos", windows))]
pub type SyncServersRegistry = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<String, SyncServerHandle>>,
>;

#[cfg(any(target_os = "macos", windows))]
impl SyncServerHandle {
    #[cfg(target_os = "macos")]
    pub fn new_macos(
        domain: String,
        shutdown_tx: tokio::sync::oneshot::Sender<()>,
        task_handle: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self { domain, shutdown_tx, task_handle }
    }

    #[cfg(windows)]
    pub fn new_windows(
        domain: String,
        shutdown_tx: std::sync::mpsc::Sender<()>,
        thread_handle: std::thread::JoinHandle<()>,
    ) -> Self {
        Self { domain, shutdown_tx, thread_handle: Some(thread_handle) }
    }

    /// Signal shutdown and wait for the server to exit. Safe to call
    /// exactly once.
    pub async fn shutdown_and_wait(mut self) {
        #[cfg(target_os = "macos")]
        {
            let _ = self.shutdown_tx.send(());
            let _ = self.task_handle.await;
        }
        #[cfg(windows)]
        {
            let _ = self.shutdown_tx.send(());
            if let Some(h) = self.thread_handle.take() {
                // Join the OS thread off the tokio runtime so we don't
                // block an executor worker while WinFsp unmounts.
                let _ = tokio::task::spawn_blocking(move || h.join()).await;
            }
        }
    }
}
