/// Sync root lifecycle — registration, session management, teardown.

use cloud_filter::root::{
    HydrationPolicy, HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder,
    SyncRootInfo,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

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
}

impl SyncRoot {
    /// Register and connect a sync root.
    pub fn start(
        mount_id: &str,
        display_name: &str,
        nas_root: PathBuf,
        client_root: PathBuf,
    ) -> Result<Self, String> {
        // Ensure client directory exists
        fs::create_dir_all(&client_root)
            .map_err(|e| format!("Failed to create sync root dir {:?}: {}", client_root, e))?;

        // Verify NAS is reachable
        fs::read_dir(&nas_root)
            .map_err(|e| format!("NAS unreachable at {:?}: {}", nas_root, e))?;

        // Build sync root ID (unique per provider + user + mount)
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(
                SecurityId::current_user()
                    .map_err(|e| format!("Failed to get current user SID: {}", e))?,
            )
            .account_name(mount_id)
            .build();

        // Clean up any stale registration
        let _ = sync_root_id.unregister();

        // Register
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
                    .with_path(&client_root)
                    .map_err(|e| format!("Invalid sync root path: {}", e))?,
            )
            .map_err(|e| format!("Failed to register sync root: {}", e))?;

        log::info!(
            "[sync] Registered sync root '{}' at {:?}",
            mount_id,
            client_root
        );

        // Shared state
        let echo = Arc::new(EchoSuppressor::new());
        let connectivity = Arc::new(NasConnectivity::new());

        // Create watcher (shared between filter and SyncRoot for clean shutdown)
        let watcher = Arc::new(NasWatcher::new(nas_root.clone(), echo.clone()));

        // Connect the filter (gets connectivity for hydration retry)
        let filter = NasSyncFilter::new(
            nas_root.clone(),
            client_root.clone(),
            watcher.clone(),
            echo.clone(),
            connectivity.clone(),
        );

        // Start the NAS watcher before connecting (so it's ready for callbacks)
        watcher.start();

        let connection = Session::new()
            .connect(&client_root, filter)
            .map_err(|e| format!("Failed to connect sync filter: {}", e))?;

        log::info!("[sync] Filter connected for '{}'", mount_id);

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
        })
    }

    /// Tear down the sync root: disconnect session and unregister.
    pub fn stop(&mut self) {
        log::info!("[sync] Stopping sync root '{}'", self.mount_id);

        // Stop write-through first (cancels pending uploads)
        if let Some(mut wt) = self.write_through.take() {
            wt.stop();
        }

        // Stop the NAS watcher (cancels ReadDirectoryChangesW)
        if let Some(watcher) = self.nas_watcher.take() {
            watcher.stop();
        }

        // Drop the connection (disconnects CF session)
        self._connection.take();

        // Unregister
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(SecurityId::current_user().unwrap())
            .account_name(&self.mount_id)
            .build();

        if let Err(e) = sync_root_id.unregister() {
            log::warn!(
                "[sync] Failed to unregister sync root '{}': {}",
                self.mount_id,
                e
            );
        }

        log::info!("[sync] Sync root '{}' stopped", self.mount_id);
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
