/// Sync root lifecycle — registration, session management, teardown.

use cloud_filter::root::{
    HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder, SyncRootInfo,
};
use std::fs;
use std::path::PathBuf;

use super::filter::NasSyncFilter;

const PROVIDER_NAME: &str = "MediaMount";

/// Manages the lifecycle of a single Cloud Files sync root.
pub struct SyncRoot {
    mount_id: String,
    nas_root: PathBuf,
    client_root: PathBuf,
    /// Held to keep the CF session alive. Dropped on teardown.
    _connection: Option<cloud_filter::root::Connection<NasSyncFilter>>,
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
                    .with_population_type(PopulationType::Full)
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

        // Connect the filter
        let filter = NasSyncFilter::new(nas_root.clone(), client_root.clone());

        // Start the NAS watcher before connecting (so it's ready for callbacks)
        filter.watcher.start();

        let connection = Session::new()
            .connect(&client_root, filter)
            .map_err(|e| format!("Failed to connect sync filter: {}", e))?;

        log::info!("[sync] Filter connected for '{}'", mount_id);

        Ok(Self {
            mount_id: mount_id.to_string(),
            nas_root,
            client_root,
            _connection: Some(connection),
        })
    }

    /// Tear down the sync root: disconnect session and unregister.
    pub fn stop(&mut self) {
        log::info!("[sync] Stopping sync root '{}'", self.mount_id);

        // Drop the connection first (disconnects CF session)
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

    /// Find an icon for the sync root. Falls back to a system icon.
    fn find_icon() -> String {
        // Try to find the app's icon next to the executable
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let ico = dir.join("icons").join("icon.ico");
                if ico.exists() {
                    return ico.to_string_lossy().to_string();
                }
            }
        }
        // Fallback to system icon
        r"%SystemRoot%\system32\shell32.dll,0".to_string()
    }
}

impl Drop for SyncRoot {
    fn drop(&mut self) {
        if self._connection.is_some() {
            self.stop();
        }
    }
}
