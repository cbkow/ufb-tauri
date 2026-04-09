/// Shared NAS connectivity state — observed by all sync components.
///
/// The orchestrator drives state transitions via heartbeat.
/// Components check `is_online()` before NAS operations and call
/// `report_network_error()` when they detect a failure.

use std::sync::atomic::{AtomicU8, Ordering};

const STATUS_ONLINE: u8 = 0;
const STATUS_OFFLINE: u8 = 1;
const STATUS_RECONNECTING: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NasStatus {
    Online,
    Offline,
    Reconnecting,
}

impl From<u8> for NasStatus {
    fn from(v: u8) -> Self {
        match v {
            STATUS_ONLINE => NasStatus::Online,
            STATUS_OFFLINE => NasStatus::Offline,
            STATUS_RECONNECTING => NasStatus::Reconnecting,
            _ => NasStatus::Offline,
        }
    }
}

/// Thread-safe NAS connectivity tracker. Lock-free reads via AtomicU8.
pub struct NasConnectivity {
    status: AtomicU8,
}

impl NasConnectivity {
    pub fn new() -> Self {
        Self {
            status: AtomicU8::new(STATUS_ONLINE),
        }
    }

    pub fn status(&self) -> NasStatus {
        NasStatus::from(self.status.load(Ordering::Relaxed))
    }

    pub fn set_status(&self, s: NasStatus) {
        let v = match s {
            NasStatus::Online => STATUS_ONLINE,
            NasStatus::Offline => STATUS_OFFLINE,
            NasStatus::Reconnecting => STATUS_RECONNECTING,
        };
        self.status.store(v, Ordering::SeqCst);
    }

    pub fn is_online(&self) -> bool {
        self.status.load(Ordering::Relaxed) == STATUS_ONLINE
    }

    /// Report a network error. Only the first reporter triggers the transition
    /// from Online → Offline (CAS ensures no duplicate transitions).
    pub fn report_network_error(&self) {
        let _ = self.status.compare_exchange(
            STATUS_ONLINE,
            STATUS_OFFLINE,
            Ordering::SeqCst,
            Ordering::Relaxed,
        );
    }
}

/// Check if an I/O error is a network/SMB error (vs a local filesystem error).
pub fn is_network_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::TimedOut
            | ErrorKind::BrokenPipe
    ) || matches!(
        e.raw_os_error(),
        Some(53 | 59 | 64 | 121)
        // 53 = ERROR_BAD_NETPATH (network path not found)
        // 59 = ERROR_UNEXP_NET_ERR (unexpected network error)
        // 64 = ERROR_NETNAME_DELETED (network name no longer available)
        // 121 = ERROR_SEM_TIMEOUT (semaphore timeout — SMB stall)
    )
}
