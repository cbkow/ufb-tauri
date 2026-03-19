use crate::config::{self, MountConfig, MountsConfig};
use crate::messages::{AgentToUfb, AckMsg, ErrorMsg, UfbToAgent};
use crate::orchestrator::Orchestrator;
use crate::state::MountEvent;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Top-level mount manager. Manages N mount instances.
pub struct MountService {
    mounts: HashMap<String, MountInstance>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
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

            if self.mounts.contains_key(&mount_config.id) {
                continue;
            }

            self.start_mount(mount_config).await;
        }
    }

    async fn start_mount(&mut self, config: MountConfig) {
        let mount_id = config.id.clone();

        log::info!("Starting mount: {}", mount_id);

        let mut orchestrator = Orchestrator::new(config.clone(), self.ipc_tx.clone());
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
