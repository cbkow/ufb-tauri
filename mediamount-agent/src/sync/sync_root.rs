/// Sync root lifecycle — registration, session management, teardown.

use cloud_filter::root::{
    HydrationPolicy, HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder,
    SyncRootInfo,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::cache::CacheIndex;
use super::connectivity::NasConnectivity;
use super::filter::NasSyncFilter;
use super::watcher::NasWatcher;
use super::write_through::{EchoSuppressor, WriteThrough};

const PROVIDER_NAME: &str = "MediaMount";

/// Manages the lifecycle of a single Cloud Files sync root.
pub struct SyncRoot {
    mount_id: String,
    nas_root: PathBuf,
    client_root: PathBuf,
    /// Held to keep the CF session alive. Dropped on teardown.
    _connection: Option<cloud_filter::root::Connection<NasSyncFilter>>,
    /// Write-through pipeline (local saves → NAS upload → placeholder conversion).
    write_through: Option<WriteThrough>,
    /// NAS watcher — kept separately so we can stop it (the filter is moved into the CF session).
    nas_watcher: Option<Arc<NasWatcher>>,
    /// Shared connectivity state — read by all components, driven by orchestrator heartbeat.
    connectivity: Arc<NasConnectivity>,
    /// Cache index for LRU eviction.
    cache: Arc<CacheIndex>,
}

impl SyncRoot {
    /// Start or reconnect a sync root.
    /// Tries to reconnect to an existing registration first (preserves placeholders).
    /// Falls back to fresh registration if the root isn't registered yet.
    pub fn start(
        mount_id: &str,
        display_name: &str,
        nas_root: PathBuf,
        client_root: PathBuf,
        cache_limit_bytes: u64,
    ) -> Result<Self, String> {
        // Ensure client directory exists
        fs::create_dir_all(&client_root)
            .map_err(|e| format!("Failed to create sync root dir {:?}: {}", client_root, e))?;

        // Verify NAS is reachable
        fs::read_dir(&nas_root)
            .map_err(|e| format!("NAS unreachable at {:?}: {}", nas_root, e))?;

        // Shared state (needed before connect)
        let echo = Arc::new(EchoSuppressor::new());
        let connectivity = Arc::new(NasConnectivity::new());
        let open_handles = Arc::new(Mutex::new(HashMap::new()));

        // Cache index (SQLite, per-mount)
        let (cache_index, needs_repair) = CacheIndex::open(
            &client_root,
            mount_id,
            cache_limit_bytes,
            open_handles.clone(),
        );
        let cache = Arc::new(cache_index);

        // If DB was corrupt, rebuild with dehydrate-all
        if needs_repair {
            cache.rebuild(true);
        }

        // Create watcher
        let watcher = Arc::new(NasWatcher::new(nas_root.clone(), echo.clone()));

        // Helper to create a fresh filter instance
        let make_filter = |oh: Arc<Mutex<HashMap<PathBuf, u32>>>| {
            NasSyncFilter::new(
                nas_root.clone(),
                client_root.clone(),
                watcher.clone(),
                echo.clone(),
                connectivity.clone(),
                cache.clone(),
                oh,
            )
        };

        // Try to reconnect to existing registration first (preserves placeholders).
        // If connect fails, register fresh. Session::connect() consumes the filter,
        // so we create a new instance for the registration path.
        let (connection, is_reconnect) = {
            let filter = make_filter(open_handles.clone());
            match Session::new().connect(&client_root, filter) {
                Ok(conn) => {
                    log::info!(
                        "[sync] Reconnected to existing sync root '{}' at {:?}",
                        mount_id, client_root
                    );
                    (conn, true)
                }
                Err(e) => {
                    log::info!(
                        "[sync] No existing registration for '{}' ({}), registering fresh",
                        mount_id, e
                    );
                    let filter = make_filter(open_handles.clone());
                    let conn = Self::register_fresh(mount_id, display_name, &client_root, filter)?;
                    log::info!(
                        "[sync] Registered new sync root '{}' at {:?}",
                        mount_id, client_root
                    );
                    (conn, false)
                }
            }
        };

        // Update last_connected timestamp
        cache.update_last_connected();

        // If DB is empty and this isn't a corruption rebuild, scan for hydrated files
        if !needs_repair && cache.total_cached_bytes() == 0 && is_reconnect {
            cache.rebuild(false); // scan and index, don't dehydrate
        }

        // Start the write-through pipeline
        let write_through = WriteThrough::start(
            client_root.clone(),
            nas_root.clone(),
            echo,
            connectivity.clone(),
        );

        Ok(Self {
            mount_id: mount_id.to_string(),
            nas_root,
            client_root,
            _connection: Some(connection),
            write_through: Some(write_through),
            nas_watcher: Some(watcher),
            connectivity,
            cache,
        })
    }

    /// Full registration of a new sync root.
    fn register_fresh(
        mount_id: &str,
        display_name: &str,
        client_root: &Path,
        filter: NasSyncFilter,
    ) -> Result<cloud_filter::root::Connection<NasSyncFilter>, String> {
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(
                SecurityId::current_user()
                    .map_err(|e| format!("Failed to get current user SID: {}", e))?,
            )
            .account_name(mount_id)
            .build();

        // Clean up stale registration if any
        let _ = sync_root_id.unregister();

        let icon_path = Self::find_icon();
        sync_root_id
            .register(
                SyncRootInfo::default()
                    .with_display_name(display_name)
                    .with_hydration_type(HydrationType::Full)
                    .with_hydration_policy(
                        HydrationPolicy::StreamingAllowed
                            | HydrationPolicy::AutoDehydrationAllowed
                            | HydrationPolicy::AllowFullRestartHydration,
                    )
                    .with_population_type(PopulationType::Full)
                    .with_allow_pinning(true)
                    .with_icon(&icon_path)
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .with_path(client_root)
                    .map_err(|e| format!("Invalid sync root path: {}", e))?,
            )
            .map_err(|e| format!("Failed to register sync root: {}", e))?;

        Session::new()
            .connect(client_root, filter)
            .map_err(|e| format!("Failed to connect sync filter: {}", e))
    }

    /// Disconnect the sync root session. Does NOT unregister — placeholders survive.
    /// Call `unregister()` separately to fully remove the sync root.
    pub fn stop(&mut self) {
        log::info!("[sync] Stopping sync root '{}'", self.mount_id);

        // Update last_connected timestamp before stopping
        self.cache.update_last_connected();

        // Stop write-through first (cancels pending uploads)
        if let Some(mut wt) = self.write_through.take() {
            wt.stop();
        }

        // Stop the NAS watcher (cancels ReadDirectoryChangesW)
        if let Some(watcher) = self.nas_watcher.take() {
            watcher.stop();
        }

        // Drop the connection (disconnects CF session — but registration persists!)
        self._connection.take();

        log::info!("[sync] Sync root '{}' stopped (registration preserved)", self.mount_id);
    }

    /// Fully remove the sync root registration. Only call when user disables sync.
    pub fn unregister(mount_id: &str) {
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(SecurityId::current_user().unwrap())
            .account_name(mount_id)
            .build();

        if let Err(e) = sync_root_id.unregister() {
            log::warn!(
                "[sync] Failed to unregister sync root '{}': {}",
                mount_id, e
            );
        } else {
            log::info!("[sync] Sync root '{}' unregistered", mount_id);
        }
    }

    /// Get shared connectivity state for the orchestrator.
    pub fn connectivity(&self) -> Arc<NasConnectivity> {
        self.connectivity.clone()
    }

    /// Stop the NAS watcher (e.g., when NAS goes offline).
    pub fn stop_watcher(&self) {
        if let Some(ref w) = self.nas_watcher {
            w.stop();
        }
    }

    /// Restart the NAS watcher after reconnect.
    pub fn restart_watcher(&self) {
        if let Some(ref w) = self.nas_watcher {
            w.restart();
        }
    }

    /// Clear all cached (hydrated) data for this mount.
    pub fn clear_cache(&self) -> (u32, u64) {
        self.cache.clear_all()
    }

    /// Get the current sync activity summary for UI display.
    pub fn activity_summary(&self) -> String {
        if let Some(ref wt) = self.write_through {
            wt.activity.lock().unwrap().summary()
        } else {
            "Disabled".to_string()
        }
    }

    /// Icon for the sync root in Explorer.
    /// Uses cloud-sync.ico next to the exe, falls back to embedded exe icon.
    fn find_icon() -> String {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let cloud_ico = dir.join("cloud-sync.ico");
                if cloud_ico.exists() {
                    let path = cloud_ico.to_string_lossy().to_string();
                    log::info!("[sync] Using sync icon: {}", path);
                    return path;
                }
            }
            // Fallback: embedded icon in the exe
            let path = format!("{},0", exe.to_string_lossy());
            log::info!("[sync] Using exe icon: {}", path);
            return path;
        }
        r"%SystemRoot%\system32\shell32.dll,12".to_string()
    }
}

impl Drop for SyncRoot {
    fn drop(&mut self) {
        if self._connection.is_some() || self.write_through.is_some() || self.nas_watcher.is_some() {
            self.stop();
        }
    }
}
