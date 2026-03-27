use crate::config::MountConfig;
use crate::messages::{AgentToUfb, MountStateUpdateMsg};
use crate::state::{self, Effect, LogLevel, MountEvent, MountState};
use tokio::sync::mpsc;

/// Per-mount orchestrator. Receives events, runs transitions, dispatches effects.
pub struct Orchestrator {
    pub mount_id: String,
    state: MountState,
    config: MountConfig,
    event_tx: mpsc::Sender<MountEvent>,
    event_rx: mpsc::Receiver<MountEvent>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
}

impl Orchestrator {
    pub fn new(
        config: MountConfig,
        ipc_tx: mpsc::Sender<AgentToUfb>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(64);
        let mount_id = config.id.clone();

        Self {
            mount_id,
            state: MountState::Initializing,
            config,
            event_tx,
            event_rx,
            ipc_tx,
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

        loop {
            tokio::select! {
                Some(event) = self.event_rx.recv() => {
                    let is_stop = matches!(event, MountEvent::Stop);
                    self.handle_event(event).await;

                    // After restart, if we're in Mounting state and no error, transition to Mounted
                    if matches!(self.state, MountState::Mounting) {
                        self.handle_event(MountEvent::RequestStateUpdate).await;
                    }

                    if is_stop {
                        log::info!("[{}] Stop processed, orchestrator exiting", self.mount_id);
                        break;
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
                self.mount_drive().await;
            }
            Effect::DisconnectDrive => {
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
            // Single WNetAddConnection2W call: authenticate + map drive letter
            let result = crate::platform::windows::fallback::connect_drive(
                &self.config.mount_drive_letter,
                &self.config.nas_share_path,
                &username,
                &password,
            );
            if let Err(e) = result {
                log::error!("[{}] Mount failed: {}", self.mount_id, e);
                let _ = self
                    .event_tx
                    .send(MountEvent::MountFailed { reason: e })
                    .await;
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
            // macOS: open smb:// to mount, then symlink from stable path
            let mount_result = crate::platform::macos::macos_smb_mount(
                &self.config.nas_share_path,
                &username,
                &password,
            );

            match mount_result {
                Ok(volumes_path) => {
                    // Create symlink from stable path to actual /Volumes/ mount point
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
            if let Err(e) = crate::platform::windows::fallback::disconnect_drive(
                &self.config.mount_drive_letter,
            ) {
                log::warn!("[{}] Disconnect failed (non-fatal): {}", self.mount_id, e);
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

            // Read the symlink target (actual /Volumes/ path) before removing
            if let Ok(volumes_path) = dm.read_target(&mount_point) {
                // Remove the symlink first
                if let Err(e) = dm.remove(&mount_point) {
                    log::warn!("[{}] Remove symlink failed (non-fatal): {}", self.mount_id, e);
                }
                // Unmount the actual SMB mount
                if let Err(e) = crate::platform::macos::macos_smb_unmount(&volumes_path) {
                    log::warn!("[{}] Unmount failed (non-fatal): {}", self.mount_id, e);
                }
            } else {
                // No symlink — just try to remove it in case it's stale
                let _ = dm.remove(&mount_point);
            }
        }
    }

    async fn retrieve_credentials(&self) -> (String, String) {
        #[cfg(windows)]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::windows::WindowsCredentialStore::new();
            match cred_store.retrieve(&self.config.credential_key) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, self.config.credential_key, e
                    );
                    (String::new(), String::new())
                }
            }
        }
        #[cfg(target_os = "linux")]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::linux::LinuxCredentialStore::new();
            match cred_store.retrieve(&self.config.credential_key) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, self.config.credential_key, e
                    );
                    (String::new(), String::new())
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            use crate::platform::CredentialStore;
            let cred_store = crate::platform::macos::MacosCredentialStore::new();
            match cred_store.retrieve(&self.config.credential_key) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id, self.config.credential_key, e
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

        let msg = AgentToUfb::MountStateUpdate(MountStateUpdateMsg {
            mount_id: self.mount_id.clone(),
            state: state_name.into(),
            state_detail: self.state.to_string(),
        });

        if let Err(e) = self.ipc_tx.send(msg).await {
            log::debug!("[{}] Failed to send state update: {}", self.mount_id, e);
        }
    }
}
