use crate::config::MountConfig;
use crate::messages::{AgentToUfb, MountStateUpdateMsg};
use crate::state::{self, Effect, LogLevel, MountEvent, MountState, SyncState};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Per-mount orchestrator. Receives events, runs transitions, dispatches effects.
pub struct Orchestrator {
    pub mount_id: String,
    state: MountState,
    config: MountConfig,
    /// Global cache root for sync mounts.
    cache_root: std::path::PathBuf,
    event_tx: mpsc::Sender<MountEvent>,
    event_rx: mpsc::Receiver<MountEvent>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
    /// Servers with active SMB sessions — shared across all orchestrators.
    /// If a server is already connected, skip credential lookup and reuse the session.
    connected_servers: Arc<Mutex<HashSet<String>>>,
    /// On-demand sync root (Windows only). Held alive to keep the CF session active.
    #[cfg(windows)]
    sync_root: Option<crate::sync::SyncRoot>,
    /// Current sync sub-state, tracked independently from MountState.
    #[cfg(windows)]
    sync_state: SyncState,
    /// Shared NAS connectivity state for reconnect (Windows sync only).
    #[cfg(windows)]
    connectivity: Option<std::sync::Arc<crate::sync::NasConnectivity>>,
    /// True if symlink creation failed due to missing privileges (Windows).
    #[cfg(windows)]
    needs_elevation: bool,
    /// NAS file watcher for macOS sync mode.
    #[cfg(target_os = "macos")]
    nas_watcher: Option<crate::sync::MacosNasWatcher>,
}

impl Orchestrator {
    pub fn new(
        config: MountConfig,
        cache_root: std::path::PathBuf,
        ipc_tx: mpsc::Sender<AgentToUfb>,
        connected_servers: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(64);
        let mount_id = config.id.clone();

        Self {
            mount_id,
            state: MountState::Initializing,
            config,
            cache_root,
            event_tx,
            event_rx,
            ipc_tx,
            connected_servers,
            #[cfg(windows)]
            sync_root: None,
            #[cfg(windows)]
            sync_state: SyncState::Disabled,
            #[cfg(windows)]
            connectivity: None,
            #[cfg(windows)]
            needs_elevation: false,
            #[cfg(target_os = "macos")]
            nas_watcher: None,
        }
    }

    /// Get a sender for sending events to this orchestrator.
    pub fn event_sender(&self) -> mpsc::Sender<MountEvent> {
        self.event_tx.clone()
    }

    /// Run the orchestrator event loop. Blocks until stopped.
    pub async fn run(&mut self) {
        log::info!(
            "[{}] Orchestrator started (state: {})",
            self.mount_id,
            self.state
        );

        // Auto-start
        self.handle_event(MountEvent::Start).await;

        // If mounting succeeded (no error), transition to Mounted
        if matches!(self.state, MountState::Mounting) {
            self.handle_event(MountEvent::RequestStateUpdate).await;
        }

        // Periodic sync activity update (2s) and NAS heartbeat (30s)
        let mut sync_tick = tokio::time::interval(std::time::Duration::from_secs(2));
        let mut heartbeat_tick = tokio::time::interval(std::time::Duration::from_secs(30));
        // Non-blocking heartbeat: result arrives via channel so select! stays responsive
        let (hb_tx, mut hb_rx) = mpsc::channel::<bool>(1);

        loop {
            tokio::select! {
                event = self.event_rx.recv() => {
                    match event {
                        Some(MountEvent::ClearSyncCache) => {
                            #[cfg(windows)]
                            if let Some(ref sr) = self.sync_root {
                                let (count, bytes) = sr.clear_cache();
                                log::info!(
                                    "[{}] Cache cleared: {} files, {:.1} MB",
                                    self.mount_id, count,
                                    bytes as f64 / (1024.0 * 1024.0)
                                );
                            }
                            #[cfg(target_os = "macos")]
                            if self.config.is_sync_mode() {
                                log::info!("[{}] Clear cache requested — notifying extension", self.mount_id);
                                crate::sync::macos_watcher::post_clear_cache_notification(&self.config.share_name());
                            }
                        }
                        Some(event) => {
                            self.handle_event(event).await;

                            // Exit the event loop once fully stopped
                            if matches!(self.state, MountState::Stopped) {
                                log::info!("[{}] Orchestrator stopping", self.mount_id);
                                break;
                            }

                            // After restart or start, if we're in Mounting state and no error, transition to Mounted
                            if matches!(self.state, MountState::Mounting) {
                                self.handle_event(MountEvent::RequestStateUpdate).await;
                            }
                        }
                        None => {
                            // Channel closed — mount removed from config, exit
                            log::info!("[{}] Event channel closed, orchestrator exiting", self.mount_id);
                            break;
                        }
                    }
                }
                // Periodic sync activity update
                _ = sync_tick.tick() => {
                    #[cfg(windows)]
                    if self.sync_root.is_some() {
                        self.emit_state_update().await;
                    }
                }
                // NAS heartbeat — fire-and-forget with 10s timeout, result comes via hb_rx
                _ = heartbeat_tick.tick() => {
                    #[cfg(windows)]
                    if self.sync_root.is_some() {
                        let nas_root = self.config.nas_share_path.clone();
                        let tx = hb_tx.clone();
                        tokio::spawn(async move {
                            // Timeout the SMB metadata call — stale connections can block 60s+
                            let result = tokio::time::timeout(
                                std::time::Duration::from_secs(10),
                                tokio::task::spawn_blocking(move || {
                                    std::fs::metadata(&nas_root).is_ok()
                                }),
                            ).await;
                            let reachable = match result {
                                Ok(Ok(r)) => r,
                                _ => false, // Timeout or panic = unreachable
                            };
                            let _ = tx.send(reachable).await;
                        });
                    }
                }
                // Heartbeat result — handle disconnect/reconnect
                Some(reachable) = hb_rx.recv() => {
                    #[cfg(windows)]
                    if self.sync_root.is_some() {
                        self.handle_heartbeat_result(reachable).await;
                    }
                }
            }
        }
    }

    async fn handle_event(&mut self, event: MountEvent) {
        log::debug!(
            "[{}] Event {:?} in state {}",
            self.mount_id,
            event,
            self.state
        );

        // Update stored config if this is a ConfigChanged event
        if let MountEvent::ConfigChanged { ref new_config } = event {
            self.config = new_config.clone();
        }

        let old_state = self.state.clone();
        let (new_state, effects) = state::transition(self.state.clone(), event);

        self.state = new_state;

        if self.state != old_state {
            log::info!(
                "[{}] {} → {}",
                self.mount_id,
                old_state,
                self.state
            );
        }

        for effect in effects {
            self.dispatch_effect(effect).await;
        }
    }

    async fn dispatch_effect(&mut self, effect: Effect) {
        match effect {
            Effect::MountDrive => {
                #[cfg(windows)]
                if self.config.is_sync_mode() {
                    self.start_sync().await;
                    return;
                }
                self.mount_drive().await;
            }
            Effect::DisconnectDrive => {
                #[cfg(windows)]
                if self.sync_root.is_some() {
                    self.stop_sync().await;
                    return;
                }
                self.disconnect_drive().await;
            }
            Effect::UpdateTray => {
                // Tray updates are handled by the mount_service via state updates
            }
            Effect::LogEvent { level, message } => match level {
                LogLevel::Info => log::info!("[{}] {}", self.mount_id, message),
                LogLevel::Error => log::error!("[{}] {}", self.mount_id, message),
            },
            Effect::EmitStateUpdate => {
                self.emit_state_update().await;
            }
        }
    }

    async fn mount_drive(&mut self) {
        // Retrieve credentials
        let (username, password) = self.retrieve_credentials().await;

        #[cfg(windows)]
        {
            // Step 1: Verify or create symlink at C:\Volumes\ufb\{share_name}
            let volume_path = self.config.volume_path();
            let nas_path = self.config.nas_share_path.clone();
            let link = std::path::Path::new(&volume_path);

            if link.is_symlink() {
                log::info!(
                    "[{}] Mapped {} → {} (symlink exists)",
                    self.mount_id, volume_path, nas_path
                );
            } else if link.exists() {
                log::info!(
                    "[{}] Mapped {} (directory exists)",
                    self.mount_id, volume_path
                );
            } else {
                // Symlink doesn't exist — try to create it
                use crate::platform::DriveMapping;
                let mapping = crate::platform::windows::WindowsMountMapping::new();
                match mapping.switch(&volume_path, &nas_path) {
                    Ok(()) => {
                        log::info!(
                            "[{}] Created symlink {} → {}",
                            self.mount_id, volume_path, nas_path
                        );
                    }
                    Err(ref e) if e == "NEEDS_ELEVATION" => {
                        log::warn!(
                            "[{}] Symlink requires elevation",
                            self.mount_id
                        );
                        self.needs_elevation = true;
                    }
                    Err(e) => {
                        log::error!("[{}] Symlink creation failed: {}", self.mount_id, e);
                        let _ = self
                            .event_tx
                            .send(MountEvent::MountFailed { reason: e })
                            .await;
                        return;
                    }
                }
            }

            // Step 2: Establish SMB session in background (don't block mount state)
            // Windows will use cached sessions if available; this ensures credentials
            // are set up for when the user accesses the symlink.
            {
                let share = self.config.nas_share_path.clone();
                let u = username.clone();
                let p = password.clone();
                let mid = self.mount_id.clone();
                let servers = self.connected_servers.clone();
                tokio::spawn(async move {
                    let share2 = share.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        crate::platform::windows::fallback::establish_smb_session(&share2, &u, &p)
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("SMB session task panicked: {}", e)));

                    match result {
                        Ok(()) => {
                            let host = share
                                .trim_start_matches('\\')
                                .split('\\')
                                .next()
                                .unwrap_or("")
                                .to_lowercase();
                            if !host.is_empty() {
                                servers.lock().unwrap().insert(host);
                            }
                        }
                        Err(e) => {
                            log::warn!("[{}] Background SMB session failed: {}", mid, e);
                        }
                    }
                });
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Linux: gio mount + symlink (two-step, same as before)
            use crate::platform::SmbSession;
            let smb = crate::platform::linux::LinuxSmbSession::new();
            let smb_result = smb.ensure_session(
                &self.config.nas_share_path,
                &self.config.smb_target_path(),
                &username,
                &password,
            );

            if let Err(e) = smb_result {
                log::error!("[{}] SMB session failed: {}", self.mount_id, e);
                let _ = self
                    .event_tx
                    .send(MountEvent::MountFailed { reason: e })
                    .await;
                return;
            }

            // Create symlink from user-facing path to SMB mount
            use crate::platform::DriveMapping;
            let dm = crate::platform::linux::LinuxMountMapping::new();
            let mount_point = self.config.mount_path();
            let target = self.config.smb_target_path();
            if let Err(e) = dm.switch(&mount_point, &target) {
                log::error!("[{}] Mount mapping failed: {}", self.mount_id, e);
                let _ = self
                    .event_tx
                    .send(MountEvent::MountFailed { reason: e })
                    .await;
            }
        }

        #[cfg(target_os = "macos")]
        {
            if self.config.is_sync_mode() {
                // Sync mode: headless SMB mount + FileProvider domain + watcher
                let smb_result = crate::platform::macos::macos_smb_mount(
                    &self.config.nas_share_path,
                    &username,
                    &password,
                );
                match &smb_result {
                    Ok(volumes_path) => {
                        log::info!("[{}] Headless SMB mount at {}", self.mount_id, volumes_path);

                        let watcher = crate::sync::MacosNasWatcher::new(
                            std::path::PathBuf::from(volumes_path),
                            self.config.share_name(),
                        );
                        watcher.start();
                        self.nas_watcher = Some(watcher);
                    }
                    Err(e) => {
                        log::error!("[{}] Headless SMB mount failed: {}", self.mount_id, e);
                        let _ = self
                            .event_tx
                            .send(MountEvent::MountFailed { reason: e.clone() })
                            .await;
                        return;
                    }
                }

                use crate::platform::DriveMapping;
                let dm = crate::platform::macos::MacosMountMapping::new();
                let mount_point = self.config.mount_path();
                let fp_path = self.config.fileprovider_domain_path()
                    .to_string_lossy().to_string();
                if let Err(e) = dm.switch(&mount_point, &fp_path) {
                    log::error!("[{}] FileProvider symlink failed: {}", self.mount_id, e);
                    let _ = self
                        .event_tx
                        .send(MountEvent::MountFailed { reason: e })
                        .await;
                }
                return;
            }

            // Regular SMB mode: mount + symlink to /Volumes/
            let mount_result = crate::platform::macos::macos_smb_mount(
                &self.config.nas_share_path,
                &username,
                &password,
            );

            match mount_result {
                Ok(volumes_path) => {
                    use crate::platform::DriveMapping;
                    let dm = crate::platform::macos::MacosMountMapping::new();
                    let mount_point = self.config.mount_path();
                    if let Err(e) = dm.switch(&mount_point, &volumes_path) {
                        log::error!("[{}] Symlink failed: {}", self.mount_id, e);
                        let _ = self
                            .event_tx
                            .send(MountEvent::MountFailed { reason: e })
                            .await;
                    }
                }
                Err(e) => {
                    log::error!("[{}] Mount failed: {}", self.mount_id, e);
                    let _ = self
                        .event_tx
                        .send(MountEvent::MountFailed { reason: e })
                        .await;
                }
            }
        }
    }

    async fn disconnect_drive(&mut self) {
        #[cfg(windows)]
        {
            // Remove symlink/junction so users can't browse stale paths
            let volume_path = self.config.volume_path();
            {
                use crate::platform::DriveMapping;
                let mapping = crate::platform::windows::WindowsMountMapping::new();
                if let Err(e) = mapping.remove(&volume_path) {
                    log::debug!("[{}] Remove mount link failed (non-fatal): {}", self.mount_id, e);
                }
            }

            // Disconnect the deviceless SMB session
            let share_path = self.config.nas_share_path.clone();
            let mount_id = self.mount_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                crate::platform::windows::fallback::disconnect_smb_session(&share_path)
            })
            .await
            .unwrap_or_else(|e| Err(format!("disconnect_smb task panicked: {}", e)));
            if let Err(e) = result {
                log::warn!("[{}] SMB disconnect failed (non-fatal): {}", mount_id, e);
            }
        }

        #[cfg(target_os = "linux")]
        {
            use crate::platform::DriveMapping;
            let dm = crate::platform::linux::LinuxMountMapping::new();
            let mount_point = self.config.mount_path();
            if let Err(e) = dm.remove(&mount_point) {
                log::warn!("[{}] Remove symlink failed (non-fatal): {}", self.mount_id, e);
            }
        }

        #[cfg(target_os = "macos")]
        {
            use crate::platform::DriveMapping;
            let dm = crate::platform::macos::MacosMountMapping::new();
            let mount_point = self.config.mount_path();

            if self.config.is_sync_mode() {
                // Sync mode: stop watcher, remove symlink, unmount headless SMB
                if let Some(watcher) = self.nas_watcher.take() {
                    watcher.stop();
                }
                let _ = dm.remove(&mount_point);
                let volumes_path = format!("/Volumes/{}", self.config.share_name());
                if std::path::Path::new(&volumes_path).exists() {
                    if let Err(e) = crate::platform::macos::macos_smb_unmount(&volumes_path) {
                        log::warn!("[{}] SMB unmount failed (non-fatal): {}", self.mount_id, e);
                    }
                }
            } else {
                // Regular SMB: read symlink target, remove symlink, unmount
                if let Ok(volumes_path) = dm.read_target(&mount_point) {
                    let _ = dm.remove(&mount_point);
                    if let Err(e) = crate::platform::macos::macos_smb_unmount(&volumes_path) {
                        log::warn!("[{}] Unmount failed (non-fatal): {}", self.mount_id, e);
                    }
                } else {
                    let _ = dm.remove(&mount_point);
                }
            }
        }
    }

    /// Start on-demand sync: authenticate SMB session, register sync root, connect filter.
    #[cfg(windows)]
    async fn start_sync(&mut self) {
        self.sync_state = SyncState::Registering;

        let (username, password) = self.retrieve_credentials().await;

        // Establish deviceless SMB session for UNC path access
        let share_path = self.config.nas_share_path.clone();
        let u = username.clone();
        let p = password.clone();
        let mount_id = self.mount_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::platform::windows::fallback::establish_smb_session(&share_path, &u, &p)
        })
        .await
        .unwrap_or_else(|e| Err(format!("SMB session task panicked: {}", e)));

        if let Err(e) = result {
            log::error!("[{}] SMB session failed: {}", mount_id, e);
            self.sync_state = SyncState::Error(e.clone());
            let _ = self
                .event_tx
                .send(MountEvent::MountFailed { reason: e })
                .await;
            return;
        }

        // Mark server as connected so other mounts skip credential lookup
        let host = Self::server_host(&self.config.nas_share_path);
        if !host.is_empty() {
            self.connected_servers.lock().unwrap().insert(host);
        }

        // Register and connect sync root (blocking — CF API registration is synchronous)
        let mid = self.mount_id.clone();
        let display_name = self.config.display_name.clone();
        let nas_root = std::path::PathBuf::from(&self.config.nas_share_path);
        let client_root = self.config.sync_root_dir(&self.cache_root);

        let cache_limit = self.config.sync_cache_limit_bytes;
        let volume_path = self.config.volume_path();
        let result = tokio::task::spawn_blocking(move || {
            crate::sync::SyncRoot::start(&mid, &display_name, nas_root, client_root, cache_limit, &volume_path)
        })
        .await
        .unwrap_or_else(|e| Err(format!("SyncRoot::start task panicked: {}", e)));

        match result {
            Ok(sync_root) => {
                self.connectivity = Some(sync_root.connectivity());
                self.sync_root = Some(sync_root);
                self.sync_state = SyncState::Active;
                log::info!("[{}] Sync root active", self.mount_id);

                // Create symlink from user-facing volume path to cache dir
                let volume_path = self.config.volume_path();
                let cache_dir = self.config.sync_root_dir(&self.cache_root).to_string_lossy().to_string();
                {
                    use crate::platform::DriveMapping;
                    let mapping = crate::platform::windows::WindowsMountMapping::new();
                    match mapping.switch(&volume_path, &cache_dir) {
                        Ok(()) => {
                            log::info!(
                                "[{}] Symlinked {} → {}",
                                self.mount_id, volume_path, cache_dir
                            );
                        }
                        Err(ref e) if e == "NEEDS_ELEVATION" => {
                            log::warn!("[{}] Sync symlink requires elevation", self.mount_id);
                            self.needs_elevation = true;
                        }
                        Err(e) => {
                            // Non-fatal for sync — CF root is accessible directly
                            log::warn!("[{}] Sync symlink failed (non-fatal): {}", self.mount_id, e);
                        }
                    }
                }

                // Run startup reconciliation (DB-driven diff of visited folders)
                if let Some(ref sr) = self.sync_root {
                    let mount_id = self.mount_id.clone();
                    let (checked, changed, added, removed, updated) =
                        sr.reconcile_startup();
                    if changed > 0 {
                        log::info!(
                            "[{}] Reconciliation: {}/{} folders changed (+{} -{} ~{})",
                            mount_id, changed, checked, added, removed, updated
                        );
                    }
                }
            }
            Err(e) => {
                log::error!("[{}] Sync root failed: {}", self.mount_id, e);
                self.sync_state = SyncState::Error(e.clone());
                let _ = self
                    .event_tx
                    .send(MountEvent::MountFailed { reason: e })
                    .await;
            }
        }
    }

    /// Stop on-demand sync: disconnect session, unregister sync root, disconnect SMB session.
    #[cfg(windows)]
    async fn stop_sync(&mut self) {
        self.sync_state = SyncState::Deregistering;

        // Remove junction so users can't browse cache dir while agent is down
        let volume_path = self.config.volume_path();
        {
            use crate::platform::DriveMapping;
            let mapping = crate::platform::windows::WindowsMountMapping::new();
            if let Err(e) = mapping.remove(&volume_path) {
                log::debug!("[{}] Remove sync link failed (non-fatal): {}", self.mount_id, e);
            }
        }

        if let Some(mut sync_root) = self.sync_root.take() {
            let result = tokio::task::spawn_blocking(move || {
                sync_root.stop();
            })
            .await;
            if let Err(e) = result {
                log::warn!("[{}] SyncRoot::stop task panicked: {}", self.mount_id, e);
            }
        }

        // Disconnect the deviceless SMB session
        let share_path = self.config.nas_share_path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::platform::windows::fallback::disconnect_smb_session(&share_path)
        })
        .await;

        self.sync_state = SyncState::Disabled;
        self.connectivity = None;
        log::info!("[{}] Sync stopped", self.mount_id);
    }

    /// Handle heartbeat result — trigger disconnect/reconnect as needed.
    #[cfg(windows)]
    async fn handle_heartbeat_result(&mut self, reachable: bool) {
        match (reachable, &self.sync_state) {
            (false, SyncState::Active) => {
                // NAS just went down
                log::warn!("[{}] NAS heartbeat failed — going offline", self.mount_id);
                self.sync_state = SyncState::Offline;
                if let Some(ref conn) = self.connectivity {
                    conn.set_status(crate::sync::NasStatus::Offline);
                }
                // Stop the NAS watcher (its handle is now invalid)
                if let Some(ref sr) = self.sync_root {
                    sr.stop_watcher();
                }
                self.emit_state_update().await;
                // Immediately transition to reconnecting
                self.sync_state = SyncState::Reconnecting;
                if let Some(ref conn) = self.connectivity {
                    conn.set_status(crate::sync::NasStatus::Reconnecting);
                }
                self.emit_state_update().await;
            }
            (true, SyncState::Offline | SyncState::Reconnecting) => {
                // NAS is back
                log::info!("[{}] NAS heartbeat OK — reconnecting", self.mount_id);
                self.complete_reconnect().await;
            }
            _ => {} // No change
        }
    }

    /// Re-establish SMB session and restart the NAS watcher after a disconnect.
    #[cfg(windows)]
    async fn complete_reconnect(&mut self) {
        // Re-establish SMB session
        let share_path = self.config.nas_share_path.clone();
        let (username, password) = self.retrieve_credentials().await;
        let _ = tokio::task::spawn_blocking(move || {
            crate::platform::windows::fallback::establish_smb_session(&share_path, &username, &password)
        })
        .await;

        // Mark server as connected
        let host = Self::server_host(&self.config.nas_share_path);
        if !host.is_empty() {
            self.connected_servers.lock().unwrap().insert(host);
        }

        // Restart NAS watcher (runs full_diff to catch up)
        if let Some(ref sr) = self.sync_root {
            sr.restart_watcher();
        }

        // Set online
        self.sync_state = SyncState::Active;
        if let Some(ref conn) = self.connectivity {
            conn.set_status(crate::sync::NasStatus::Online);
        }
        self.emit_state_update().await;
        log::info!("[{}] NAS reconnected", self.mount_id);
    }

    /// Credential keys in the config are bare names (e.g., "gfx-nas").
    /// The Tauri app stores them with a "mediamount_" prefix in the OS credential store.
    const CRED_PREFIX: &'static str = "mediamount_";

    /// Extract the server hostname from a UNC path (e.g., \\192.168.40.100\share → 192.168.40.100).
    fn server_host(nas_path: &str) -> String {
        nas_path
            .trim_start_matches('\\')
            .split('\\')
            .next()
            .unwrap_or("")
            .to_lowercase()
    }

    async fn retrieve_credentials(&self) -> (String, String) {
        // If another mount already connected to this server, skip credential lookup
        // and let the OS reuse the existing session.
        let host = Self::server_host(&self.config.nas_share_path);
        if !host.is_empty() && self.connected_servers.lock().unwrap().contains(&host) {
            log::debug!(
                "[{}] Server {} already connected, reusing session",
                self.mount_id, host
            );
            return (String::new(), String::new());
        }

        let key = &self.config.credential_key;
        if key.is_empty() {
            return (String::new(), String::new());
        }
        // Match the Tauri app's prefix convention
        let prefixed = if key.starts_with(Self::CRED_PREFIX) {
            key.clone()
        } else {
            format!("{}{}", Self::CRED_PREFIX, key)
        };

        #[cfg(windows)]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::windows::WindowsCredentialStore::new();
            match cred_store.retrieve(&prefixed) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, key, e
                    );
                    (String::new(), String::new())
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::linux::LinuxCredentialStore::new();
            match cred_store.retrieve(&prefixed) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, key, e
                    );
                    (String::new(), String::new())
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::macos::MacosCredentialStore::new();
            match cred_store.retrieve(&prefixed) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, key, e
                    );
                    (String::new(), String::new())
                }
            }
        }
        #[cfg(not(any(windows, unix)))]
        { (String::new(), String::new()) }
    }

    async fn emit_state_update(&self) {
        let state_name = match &self.state {
            MountState::Initializing => "initializing",
            MountState::Mounting => "mounting",
            MountState::Mounted => "mounted",
            MountState::Error(_) => "error",
            MountState::Stopped => "stopped",
        };

        let (sync_state, sync_state_detail) = {
            #[cfg(windows)]
            if self.config.is_sync_mode() {
                let detail = if self.sync_state == SyncState::Active {
                    // Include activity summary when active
                    self.sync_root.as_ref()
                        .map(|sr| sr.activity_summary())
                        .unwrap_or_else(|| self.sync_state.to_string())
                } else {
                    self.sync_state.to_string()
                };
                (
                    Some(self.sync_state.state_name().to_string()),
                    Some(detail),
                )
            } else {
                (None, None)
            }
            #[cfg(not(windows))]
            { (None, None) }
        };

        let needs_elevation = {
            #[cfg(windows)]
            { if self.needs_elevation { Some(true) } else { None } }
            #[cfg(not(windows))]
            { None }
        };

        let msg = AgentToUfb::MountStateUpdate(MountStateUpdateMsg {
            mount_id: self.mount_id.clone(),
            state: state_name.into(),
            state_detail: self.state.to_string(),
            sync_state,
            sync_state_detail,
            needs_elevation,
        });

        if let Err(e) = self.ipc_tx.send(msg).await {
            log::debug!("[{}] Failed to send state update: {}", self.mount_id, e);
        }
    }
}
