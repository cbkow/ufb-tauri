use crate::config::MountConfig;
use crate::health::HealthMonitor;
use crate::messages::{AgentToUfb, MountStateUpdateMsg};
use crate::rclone::{cache, RcloneManager, RcloneSignal};
use crate::state::{self, Effect, HysteresisConfig, LogLevel, MountEvent, MountState};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

/// Per-mount orchestrator. Receives events, runs transitions, dispatches effects.
pub struct Orchestrator {
    pub mount_id: String,
    state: MountState,
    config: MountConfig,
    hysteresis: HysteresisConfig,
    event_tx: mpsc::Sender<MountEvent>,
    event_rx: mpsc::Receiver<MountEvent>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
    rclone: Option<RcloneManager>,
    health_monitor: Option<HealthMonitor>,
    rclone_signal_rx: Option<mpsc::Receiver<RcloneSignal>>,
    last_fallback_time: Option<u64>,
    rc_port: u16,
}

impl Orchestrator {
    pub fn new(
        config: MountConfig,
        ipc_tx: mpsc::Sender<AgentToUfb>,
        rc_port: u16,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(64);
        let hysteresis = config.hysteresis_config();
        let mount_id = config.id.clone();

        Self {
            mount_id,
            state: MountState::Initializing,
            config,
            hysteresis,
            event_tx,
            event_rx,
            ipc_tx,
            rclone: None,
            health_monitor: None,
            rclone_signal_rx: None,
            last_fallback_time: None,
            rc_port,
        }
    }

    /// Get a sender for sending events to this orchestrator.
    pub fn event_sender(&self) -> mpsc::Sender<MountEvent> {
        self.event_tx.clone()
    }

    /// Get current state.
    pub fn state(&self) -> &MountState {
        &self.state
    }

    /// Run the orchestrator event loop. Blocks until stopped.
    pub async fn run(&mut self) {
        log::info!(
            "[{}] Orchestrator started (state: {})",
            self.mount_id,
            self.state
        );

        // Crash recovery: remove any stale junction so the normal startup flow
        // can recreate it cleanly (map SMB first, then junction → SMB, then rclone).
        #[cfg(windows)]
        {
            let junction = std::path::Path::new(&self.config.junction_path);
            if junction.exists() || junction.read_link().is_ok() {
                log::info!(
                    "[{}] Removing stale junction at {} for clean startup",
                    self.mount_id,
                    self.config.junction_path
                );
                let _ = std::fs::remove_dir(junction);
            }
        }

        // Check prerequisites before starting
        if !RcloneManager::check_winfsp() {
            self.handle_event(MountEvent::WinfspMissing).await;
            return;
        }

        // Kill any orphaned rclone processes using our drive letter (crash recovery)
        #[cfg(windows)]
        RcloneManager::kill_orphaned_rclone(&self.config.rclone_drive_letter);

        // Check drive letter conflicts (after orphan cleanup)
        let rclone_letter = self.config.rclone_drive_letter.chars().next().unwrap_or('R');
        if crate::platform::is_drive_in_use(&self.config.rclone_drive_letter) {
            self.handle_event(MountEvent::DriveLetterConflict {
                letter: rclone_letter,
            })
            .await;
            return;
        }

        // Auto-start
        self.handle_event(MountEvent::Start).await;

        loop {
            tokio::select! {
                // Events from external sources (IPC commands, health monitor, etc.)
                Some(event) = self.event_rx.recv() => {
                    let is_stop = matches!(event, MountEvent::Stop);
                    self.handle_event(event).await;

                    // Exit loop after Stop has been fully processed
                    if is_stop {
                        log::info!("[{}] Stop processed, orchestrator exiting", self.mount_id);
                        break;
                    }
                }

                // rclone process signals
                signal = async {
                    if let Some(ref mut rx) = self.rclone_signal_rx {
                        rx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(signal) = signal {
                        let event = match signal {
                            RcloneSignal::Started => MountEvent::RcloneStarted,
                            RcloneSignal::Fatal(msg) => {
                                log::error!("[{}] rclone fatal: {}", self.mount_id, msg);
                                MountEvent::RcloneStartFailed
                            }
                            RcloneSignal::Exited { code } => {
                                log::warn!("[{}] rclone exited with code {:?}", self.mount_id, code);
                                MountEvent::RcloneDied
                            }
                        };
                        self.handle_event(event).await;
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
        let (new_state, effects) =
            state::transition(self.state.clone(), event, &self.hysteresis);

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
            Effect::SpawnRclone => {
                self.spawn_rclone().await;
            }
            Effect::KillRclone => {
                self.kill_rclone().await;
            }
            Effect::SwitchJunctionToRclone => {
                let target = format!("{}:\\", self.config.rclone_drive_letter);
                self.switch_junction(&target).await;
            }
            Effect::SwitchJunctionToSmb => {
                let target = format!("{}:\\", self.config.smb_drive_letter);
                self.switch_junction(&target).await;
            }
            Effect::MapSmb => {
                self.map_smb().await;
            }
            Effect::UnmapSmb => {
                self.unmap_smb().await;
            }
            Effect::StartProbeLoop => {
                self.start_probe_loop();
            }
            Effect::StopProbeLoop => {
                self.stop_probe_loop();
            }
            Effect::UpdateTray => {
                // Tray updates are handled by the mount_service via state updates
            }
            Effect::LogEvent { level, message } => match level {
                LogLevel::Info => log::info!("[{}] {}", self.mount_id, message),
                LogLevel::Warn => log::warn!("[{}] {}", self.mount_id, message),
                LogLevel::Error => log::error!("[{}] {}", self.mount_id, message),
            },
            Effect::EmitStateUpdate => {
                self.emit_state_update().await;
            }
            Effect::DeleteCacheDir => {
                let cache_dir = std::path::Path::new(&self.config.cache_dir_path);
                if cache_dir.exists() {
                    log::info!(
                        "[{}] Deleting VFS cache at {}",
                        self.mount_id,
                        self.config.cache_dir_path
                    );
                    match std::fs::remove_dir_all(cache_dir) {
                        Ok(()) => log::info!("[{}] Cache directory deleted", self.mount_id),
                        Err(e) => log::error!(
                            "[{}] Failed to delete cache directory: {}",
                            self.mount_id,
                            e
                        ),
                    }
                }
            }
        }
    }

    async fn spawn_rclone(&mut self) {
        match RcloneManager::spawn(&self.config, self.rc_port).await {
            Ok((manager, signal_rx)) => {
                self.rclone = Some(manager);
                self.rclone_signal_rx = Some(signal_rx);
                log::info!("[{}] rclone spawned on port {}", self.mount_id, self.rc_port);
            }
            Err(e) => {
                log::error!("[{}] Failed to spawn rclone: {}", self.mount_id, e);
                let _ = self.event_tx.send(MountEvent::RcloneStartFailed).await;
            }
        }
    }

    async fn kill_rclone(&mut self) {
        if let Some(ref mut rclone) = self.rclone {
            // Before killing, check for dirty files and wait for write-back
            if rclone.is_running() {
                match cache::query_vfs_stats(self.rc_port).await {
                    Ok(stats) => {
                        let dirty = stats
                            .disk_cache
                            .as_ref()
                            .map(|c| c.dirty_count())
                            .unwrap_or(0);
                        if dirty > 0 {
                            log::info!(
                                "[{}] Waiting for {} dirty files to flush before shutdown...",
                                self.mount_id,
                                dirty
                            );
                            // Wait up to 30 seconds for dirty files to flush
                            let deadline = tokio::time::Instant::now()
                                + std::time::Duration::from_secs(30);
                            loop {
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                if tokio::time::Instant::now() >= deadline {
                                    log::warn!(
                                        "[{}] Flush timeout — proceeding with shutdown",
                                        self.mount_id
                                    );
                                    break;
                                }
                                match cache::query_vfs_stats(self.rc_port).await {
                                    Ok(s) => {
                                        let remaining = s
                                            .disk_cache
                                            .as_ref()
                                            .map(|c| c.dirty_count())
                                            .unwrap_or(0);
                                        if remaining == 0 {
                                            log::info!(
                                                "[{}] All dirty files flushed",
                                                self.mount_id
                                            );
                                            break;
                                        }
                                        log::debug!(
                                            "[{}] Still {} dirty files...",
                                            self.mount_id,
                                            remaining
                                        );
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                    }
                    Err(_) => {} // rclone RC not responding, just kill
                }
            }
            rclone.kill().await;
        }
        self.rclone = None;
        self.rclone_signal_rx = None;
    }

    async fn switch_junction(&mut self, target: &str) {
        #[cfg(windows)]
        {
            use crate::platform::MountPoint;
            let mp = crate::platform::windows::WindowsMountPoint::new();
            if let Err(e) = mp.switch(&self.config.junction_path, target) {
                log::error!("[{}] Junction switch failed: {}", self.mount_id, e);
                let _ = self
                    .event_tx
                    .send(MountEvent::JunctionSwitchFailed { reason: e })
                    .await;
            }
        }
    }

    async fn map_smb(&mut self) {
        #[cfg(windows)]
        {
            use crate::platform::{CredentialStore, FallbackMount};
            let cred_store = crate::platform::windows::WindowsCredentialStore::new();

            let (username, password) = match cred_store.retrieve(&self.config.credential_key) {
                Ok(creds) => creds,
                Err(e) => {
                    log::warn!(
                        "[{}] No credentials found for {}: {}, trying without",
                        self.mount_id,
                        self.config.credential_key,
                        e
                    );
                    (String::new(), String::new())
                }
            };

            let smb = crate::platform::windows::WindowsFallbackMount::new();
            if let Err(e) = smb.map(
                &self.config.nas_share_path,
                &self.config.smb_drive_letter,
                &username,
                &password,
            ) {
                log::error!("[{}] SMB map failed: {}", self.mount_id, e);
                let _ = self
                    .event_tx
                    .send(MountEvent::SmbMapFailed { reason: e })
                    .await;
            } else {
                self.last_fallback_time = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                );

                // Ensure .healthcheck file exists on the share
                let healthcheck_path = std::path::Path::new(&format!(
                    "{}:\\",
                    self.config.smb_drive_letter
                ))
                .join(&self.config.healthcheck_file_name);
                if !healthcheck_path.exists() {
                    match std::fs::write(&healthcheck_path, "ok") {
                        Ok(_) => log::info!(
                            "[{}] Created healthcheck file at {}",
                            self.mount_id,
                            healthcheck_path.display()
                        ),
                        Err(e) => log::warn!(
                            "[{}] Failed to create healthcheck file at {}: {}",
                            self.mount_id,
                            healthcheck_path.display(),
                            e
                        ),
                    }
                }
            }
        }
    }

    async fn unmap_smb(&mut self) {
        #[cfg(windows)]
        {
            use crate::platform::FallbackMount;
            let smb = crate::platform::windows::WindowsFallbackMount::new();
            if let Err(e) = smb.unmap(&self.config.smb_drive_letter) {
                log::warn!("[{}] SMB unmap failed: {}", self.mount_id, e);
            }
        }
    }

    fn start_probe_loop(&mut self) {
        self.stop_probe_loop();

        let mount_path = PathBuf::from(&self.config.junction_path);
        let healthcheck_file = self.config.healthcheck_file_name.clone();
        let interval = Duration::from_secs(self.config.probe_interval_secs);
        let timeout = Duration::from_millis(self.config.probe_timeout_ms);

        self.health_monitor = Some(HealthMonitor::start(
            mount_path,
            healthcheck_file,
            interval,
            timeout,
            self.event_tx.clone(),
        ));
    }

    fn stop_probe_loop(&mut self) {
        if let Some(ref mut monitor) = self.health_monitor {
            monitor.stop();
        }
        self.health_monitor = None;
    }

    async fn emit_state_update(&self) {
        // Query cache stats if rclone is running
        let (cache_used, dirty_files) = if let Some(ref rclone) = self.rclone {
            if rclone.is_running() {
                match cache::query_vfs_stats(self.rc_port).await {
                    Ok(stats) => {
                        let used = stats
                            .disk_cache
                            .as_ref()
                            .map(|c| c.bytes_used)
                            .unwrap_or(0);
                        let dirty = stats
                            .disk_cache
                            .as_ref()
                            .map(|c| c.dirty_count())
                            .unwrap_or(0);
                        (used, dirty)
                    }
                    Err(_) => (0, 0),
                }
            } else {
                (0, 0)
            }
        } else {
            (0, 0)
        };

        // Parse cache_max_size string to bytes (approximate)
        let cache_max = parse_size_to_bytes(&self.config.cache_max_size);

        let is_rclone_active = matches!(
            self.state,
            MountState::RcloneHealthy { .. } | MountState::RcloneDegraded { .. }
        );
        let is_smb_active = matches!(
            self.state,
            MountState::SmbActive
                | MountState::SmbRecovering { .. }
                | MountState::FallingBackToSmb
        );

        let state_name = match &self.state {
            MountState::Initializing => "initializing",
            MountState::RcloneStarting { .. } => "rclone_starting",
            MountState::RcloneHealthy { .. } => "rclone_healthy",
            MountState::RcloneDegraded { .. } => "rclone_degraded",
            MountState::FallingBackToSmb => "falling_back_to_smb",
            MountState::SmbActive => "smb_active",
            MountState::SmbRecovering { .. } => "smb_recovering",
            MountState::RecoveringToRclone { .. } => "recovering_to_rclone",
            MountState::ManualOverride { .. } => "manual_override",
            MountState::Error(_) => "error",
        };

        let msg = AgentToUfb::MountStateUpdate(MountStateUpdateMsg {
            mount_id: self.mount_id.clone(),
            state: state_name.into(),
            state_detail: self.state.to_string(),
            cache_used_bytes: cache_used,
            cache_max_bytes: cache_max,
            dirty_files,
            last_fallback_time: self.last_fallback_time,
            is_rclone_active,
            is_smb_active,
        });

        if let Err(e) = self.ipc_tx.send(msg).await {
            log::debug!("[{}] Failed to send state update: {}", self.mount_id, e);
        }
    }
}

/// Parse a human-readable size string (e.g. "1T", "512M", "2G") to bytes.
fn parse_size_to_bytes(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }

    let (num_str, suffix) = if s.ends_with(|c: char| c.is_ascii_alphabetic()) {
        let idx = s.len() - 1;
        (&s[..idx], &s[idx..])
    } else {
        (s, "")
    };

    let num: f64 = num_str.parse().unwrap_or(0.0);
    let multiplier: u64 = match suffix.to_uppercase().as_str() {
        "K" => 1024,
        "M" => 1024 * 1024,
        "G" => 1024 * 1024 * 1024,
        "T" => 1024 * 1024 * 1024 * 1024,
        _ => 1,
    };

    (num * multiplier as f64) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size_to_bytes("1T"), 1024 * 1024 * 1024 * 1024);
        assert_eq!(parse_size_to_bytes("512M"), 512 * 1024 * 1024);
        assert_eq!(parse_size_to_bytes("2G"), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size_to_bytes("64K"), 64 * 1024);
        assert_eq!(parse_size_to_bytes(""), 0);
        assert_eq!(parse_size_to_bytes("1024"), 1024);
    }
}
