pub mod probe;

use crate::state::MountEvent;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

/// Per-mount health monitor. Runs a probe loop that sends MountEvents.
pub struct HealthMonitor {
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HealthMonitor {
    /// Start a new health monitor for the given mount path.
    pub fn start(
        mount_path: PathBuf,
        healthcheck_file: String,
        interval: Duration,
        timeout: Duration,
        event_tx: mpsc::Sender<MountEvent>,
    ) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            log::info!(
                "Health monitor started for {} (interval={:?}, timeout={:?})",
                mount_path.display(),
                interval,
                timeout,
            );

            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // Skip immediate first tick

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let result = probe::run_probe(&mount_path, &healthcheck_file, timeout).await;
                        let event = if result {
                            MountEvent::ProbeOk
                        } else {
                            MountEvent::ProbeFailed
                        };
                        if event_tx.send(event).await.is_err() {
                            log::warn!("Health monitor: event channel closed");
                            break;
                        }
                    }
                    _ = &mut cancel_rx => {
                        log::info!("Health monitor stopped for {}", mount_path.display());
                        break;
                    }
                }
            }
        });

        Self {
            cancel_tx: Some(cancel_tx),
        }
    }

    /// Stop the health monitor.
    pub fn stop(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for HealthMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}
