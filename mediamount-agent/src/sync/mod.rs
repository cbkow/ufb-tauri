/// On-demand NAS sync via native cloud file APIs.
///
/// Windows: Cloud Files API via the `cloud-filter` crate.
/// macOS: FileProvider (future — hosted in MediaMountTray Swift app).
///
/// The sync module presents NAS files as cloud placeholders in the local filesystem.
/// Files appear locally but are only downloaded (hydrated) when accessed.
/// All operations are pass-through to the NAS via SMB — the local machine is a cache.

#[cfg(windows)]
mod sync_root;
#[cfg(windows)]
mod filter;
#[cfg(windows)]
mod watcher;
#[cfg(windows)]
mod placeholder;
#[cfg(windows)]
pub mod write_through;
#[cfg(windows)]
pub mod cache;
#[cfg(windows)]
pub mod connectivity;

#[cfg(windows)]
pub use sync_root::SyncRoot;
#[cfg(windows)]
pub use connectivity::{NasConnectivity, NasStatus};

#[cfg(target_os = "macos")]
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

/// Per-domain cache map shared between main and the NFS server. Keyed by
/// share name. Readers: NFS server startup, mount_service drain/stats.
/// Writers: main, on first hydration of a new mount.
#[cfg(target_os = "macos")]
pub type SharedCaches = std::sync::Arc<
    std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<MacosCache>>>,
>;
