use crate::config::{self, MountConfig, MountsConfig};
use crate::messages::{AgentToUfb, AckMsg, ErrorMsg, UfbToAgent};
use crate::orchestrator::Orchestrator;
use crate::state::MountEvent;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Top-level mount manager. Manages N mount instances.
pub struct MountService {
    mounts: HashMap<String, MountInstance>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
    /// Servers with active SMB sessions — shared across all orchestrators.
    connected_servers: Arc<Mutex<HashSet<String>>>,
    /// Global cache root for sync mounts.
    cache_root: std::path::PathBuf,
}

struct MountInstance {
    config: MountConfig,
    event_tx: mpsc::Sender<MountEvent>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl MountService {
    pub fn new(ipc_tx: mpsc::Sender<AgentToUfb>) -> Self {
        Self {
            mounts: HashMap::new(),
            ipc_tx,
            connected_servers: Arc::new(Mutex::new(HashSet::new())),
            cache_root: crate::config::MountConfig::default_cache_root(),
        }
    }

    /// Load config and start all enabled mounts.
    pub async fn start_from_config(&mut self) {
        let config = config::load_config();
        self.apply_config(config).await;
    }

    /// Reload config from disk and apply changes.
    pub async fn reload_config(&mut self) {
        log::info!("Reloading config...");
        let config = config::load_config();
        self.apply_config(config).await;
    }

    /// Apply a config, starting new mounts and stopping removed ones.
    async fn apply_config(&mut self, config: MountsConfig) {
        // Clean up stale Cloud Files sync root registrations and redirect active
        // CF nav entries to point at the junction path instead of the cache path
        #[cfg(windows)]
        {
            let active_sync_mounts: std::collections::HashMap<String, String> = config
                .mounts
                .iter()
                .filter(|m| m.enabled && m.sync_enabled)
                .map(|m| (m.id.clone(), m.volume_path()))
                .collect();
            crate::sync::SyncRoot::cleanup_stale_roots(&active_sync_mounts);
        }

        // macOS: clean up stale symlinks from old {id}-based naming or removed mounts
        #[cfg(target_os = "macos")]
        {
            let active_names: std::collections::HashSet<String> = config
                .mounts
                .iter()
                .filter(|m| m.enabled)
                .map(|m| m.share_name())
                .collect();
            let base = config::MountConfig::volumes_base();
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_symlink() {
                        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                            if !active_names.contains(name) {
                                let _ = std::fs::remove_file(&path);
                                log::info!("Removed stale symlink: {}", path.display());
                            }
                        }
                    }
                }
            }

            // Warn about share name collisions
            for (name, ids) in config.share_name_collisions() {
                log::warn!(
                    "Share name collision: '{}' used by mounts [{}]. Set mount_path_macos to resolve.",
                    name,
                    ids.join(", ")
                );
            }
        }

        // Detect cache root change — restart all sync mounts to re-register at new path
        let new_cache_root = config.cache_root();
        if new_cache_root != self.cache_root {
            log::info!(
                "Sync cache root changed: {:?} → {:?}. Restarting all sync mounts.",
                self.cache_root, new_cache_root
            );
            // Stop all sync mounts (they'll be re-started below with the new cache root)
            let sync_ids: Vec<String> = self.mounts.iter()
                .filter(|(_, inst)| inst.config.sync_enabled)
                .map(|(id, _)| id.clone())
                .collect();
            for id in sync_ids {
                if let Some(instance) = self.mounts.remove(&id) {
                    log::info!("Stopping sync mount {} for cache migration", id);
                    let _ = instance.event_tx.send(MountEvent::Stop).await;
                }
            }
        }
        self.cache_root = new_cache_root;

        let new_ids: std::collections::HashSet<String> =
            config.mounts.iter().map(|m| m.id.clone()).collect();

        // Stop mounts that are no longer in config
        let to_remove: Vec<String> = self
            .mounts
            .keys()
            .filter(|id| !new_ids.contains(*id))
            .cloned()
            .collect();

        for id in to_remove {
            log::info!("Removing mount: {}", id);
            if let Some(instance) = self.mounts.remove(&id) {
                let _ = instance.event_tx.send(MountEvent::Stop).await;
            }
        }

        // Start/update mounts from config
        for mount_config in config.mounts {
            if !mount_config.enabled {
                // If mount exists but is now disabled, stop it
                if let Some(instance) = self.mounts.remove(&mount_config.id) {
                    log::info!("Disabling mount: {}", mount_config.id);
                    let _ = instance.event_tx.send(MountEvent::Stop).await;
                }
                continue;
            }

            if let Some(instance) = self.mounts.get(&mount_config.id) {
                // Mount exists — check if config changed
                if instance.config != mount_config {
                    log::info!("Config changed for mount: {}", mount_config.id);
                    let _ = instance
                        .event_tx
                        .send(MountEvent::ConfigChanged {
                            new_config: mount_config.clone(),
                        })
                        .await;
                    // Update stored config
                    if let Some(instance) = self.mounts.get_mut(&mount_config.id) {
                        instance.config = mount_config;
                    }
                }
                continue;
            }

            self.start_mount(mount_config).await;
        }
    }

    async fn start_mount(&mut self, config: MountConfig) {
        let mount_id = config.id.clone();

        log::info!("Starting mount: {}", mount_id);

        let mut orchestrator = Orchestrator::new(
            config.clone(),
            self.cache_root.clone(),
            self.ipc_tx.clone(),
            self.connected_servers.clone(),
        );
        let event_tx = orchestrator.event_sender();

        // Run orchestrator in background task
        let id_clone = mount_id.clone();
        let task_handle = tokio::spawn(async move {
            orchestrator.run().await;
            log::info!("[{}] Orchestrator exited", id_clone);
        });

        // Store the mount instance
        self.mounts.insert(
            mount_id,
            MountInstance {
                config,
                event_tx: event_tx.clone(),
                task_handle,
            },
        );
    }

    /// Handle a command from UFB.
    pub async fn handle_command(&mut self, cmd: UfbToAgent) {
        match cmd {
            UfbToAgent::Ping => {
                let _ = self.ipc_tx.send(AgentToUfb::Pong).await;
            }
            UfbToAgent::ReloadConfig => {
                self.reload_config().await;
            }
            UfbToAgent::GetStates => {
                // Trigger state update emission from all mounts
                for (_, instance) in &self.mounts {
                    let _ = instance.event_tx.send(MountEvent::RequestStateUpdate).await;
                }
            }
            UfbToAgent::StartMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Start, &msg.command_id)
                    .await;
            }
            UfbToAgent::StopMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Stop, &msg.command_id)
                    .await;
            }
            UfbToAgent::RestartMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Restart, &msg.command_id)
                    .await;
            }
            UfbToAgent::ClearSyncCache(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::ClearSyncCache, &msg.command_id)
                    .await;
            }
            UfbToAgent::CreateSymlinks => {
                #[cfg(windows)]
                {
                    log::info!("CreateSymlinks command received — launching elevated instance");
                    if let Err(e) = crate::platform::windows::elevation::launch_elevated_symlink_creation() {
                        log::error!("Elevation launch failed: {}", e);
                    }
                }
                #[cfg(not(windows))]
                {
                    log::debug!("CreateSymlinks ignored on this platform");
                }
            }
            UfbToAgent::Quit => {
                log::info!("Quit command received via IPC, shutting down...");
                self.shutdown().await;
                std::process::exit(0);
            }
        }
    }

    async fn send_to_mount(&self, mount_id: &str, event: MountEvent, command_id: &str) {
        match self.mounts.get(mount_id) {
            Some(instance) => {
                if instance.event_tx.send(event).await.is_ok() {
                    if !command_id.is_empty() {
                        let _ = self
                            .ipc_tx
                            .send(AgentToUfb::Ack(AckMsg {
                                command_id: command_id.into(),
                            }))
                            .await;
                    }
                }
            }
            None => {
                if !command_id.is_empty() {
                    let _ = self
                        .ipc_tx
                        .send(AgentToUfb::Error(ErrorMsg {
                            command_id: command_id.into(),
                            message: format!("unknown mount: {}", mount_id),
                        }))
                        .await;
                }
            }
        }
    }

    /// Route a MountEvent directly to a mount's orchestrator (used by tray commands).
    pub async fn route_event(&self, mount_id: &str, event: MountEvent) {
        if let Some(instance) = self.mounts.get(mount_id) {
            let _ = instance.event_tx.send(event).await;
        } else {
            log::warn!("route_event: unknown mount {}", mount_id);
        }
    }

    /// Stop all mounts gracefully and wait for orchestrators to finish.
    pub async fn shutdown(&mut self) {
        log::info!("Shutting down all mounts...");

        // Send Stop to all mounts
        let mut handles = Vec::new();
        for (id, instance) in self.mounts.drain() {
            log::info!("Stopping mount: {}", id);
            let _ = instance.event_tx.send(MountEvent::Stop).await;
            handles.push((id, instance.task_handle));
        }

        // Wait for all orchestrators to finish (with timeout)
        for (id, handle) in handles {
            match tokio::time::timeout(std::time::Duration::from_secs(15), handle).await {
                Ok(Ok(())) => log::info!("Mount {} shut down cleanly", id),
                Ok(Err(e)) => log::error!("Mount {} task panicked: {}", id, e),
                Err(_) => log::warn!("Mount {} shutdown timed out after 15s", id),
            }
        }

        log::info!("All mounts shut down");
    }
}
