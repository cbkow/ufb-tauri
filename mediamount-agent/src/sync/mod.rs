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
