//! NAS reachability tracking — shared between macOS NFS and Windows WinFsp.
//!
//! Background probe loop `stat()`s the share root every `PROBE_INTERVAL`.
//! After `FAILURE_THRESHOLD` consecutive failures, flips to "offline" so
//! handlers can short-circuit SMB ops instead of waiting 60 seconds for the
//! kernel timeout. Platform-neutral: no macOS or Windows-specific code here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const FAILURE_THRESHOLD: u32 = 2;

pub struct NasHealth {
    domain: String,
    nas_root: PathBuf,
    connected: AtomicBool,
    consecutive_failures: AtomicU32,
}

impl NasHealth {
    pub fn new(domain: String, nas_root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            domain,
            nas_root,
            connected: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
        })
    }

    pub fn is_online(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Spawn the background probe loop. Non-blocking. Requires a running
    /// tokio runtime (the agent always has one on macOS and Windows).
    pub fn spawn_probe_loop(self: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                self.probe_once().await;
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        });
    }

    async fn probe_once(&self) {
        let root = self.nas_root.clone();
        // Run the stat on the blocking pool so a hung SMB call doesn't
        // stall the async runtime.
        let res = tokio::task::spawn_blocking(move || std::fs::metadata(&root)).await;
        let ok = matches!(res, Ok(Ok(_)));

        if ok {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            let was_offline = !self.connected.swap(true, Ordering::Relaxed);
            if was_offline {
                log::info!(
                    "[nas-health] {} NAS reachable again — resuming writes",
                    self.domain
                );
            }
        } else {
            let fails = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
            if fails >= FAILURE_THRESHOLD {
                let was_online = self.connected.swap(false, Ordering::Relaxed);
                if was_online {
                    log::warn!(
                        "[nas-health] {} NAS unreachable (failed {} consecutive probes) — serving from cache only",
                        self.domain,
                        fails
                    );
                }
            }
        }
    }
}
