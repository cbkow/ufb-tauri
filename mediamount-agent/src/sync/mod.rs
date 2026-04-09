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
pub mod write_through;

#[cfg(windows)]
pub use sync_root::SyncRoot;
