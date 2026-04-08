use crate::config::MountConfig;
use serde::{Deserialize, Serialize};
use std::fmt;

// ── Mount State Machine ──
// Pure transition function — no side effects. Returns new state + effects for orchestrator.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MountState {
    Initializing,
    Mounting,
    Mounted,
    Error(MountError),
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MountError {
    ConfigInvalid { reason: String },
    MountFailed { reason: String },
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountError::ConfigInvalid { reason } => write!(f, "invalid config: {}", reason),
            MountError::MountFailed { reason } => write!(f, "mount failed: {}", reason),
        }
    }
}

#[derive(Debug, Clone)]
pub enum MountEvent {
    // Lifecycle
    Start,
    Stop,
    Restart,

    // Config changed while mount is running — orchestrator should
    // tear down the old config and apply the new one.
    ConfigChanged { new_config: MountConfig },

    // Platform errors
    MountFailed { reason: String },

    // State query
    RequestStateUpdate,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Connect and map the drive (single operation per platform).
    /// Windows: WNetAddConnection2W with drive letter.
    /// Linux: gio mount + symlink.
    MountDrive,
    /// Disconnect the drive before remounting or stopping.
    /// Windows: WNetCancelConnection2W. Linux: remove symlink.
    DisconnectDrive,
    UpdateTray,
    LogEvent { level: LogLevel, message: String },
    EmitStateUpdate,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Info,
    Error,
}

/// Pure state transition function. No side effects — returns new state + effects.
pub fn transition(
    state: MountState,
    event: MountEvent,
) -> (MountState, Vec<Effect>) {
    use MountEvent::*;
    use MountState::*;

    match (&state, event) {
        // ── Initializing ──
        (Initializing, Start) => (
            Mounting,
            vec![
                Effect::MountDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mounting SMB share".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Mounting ──
        (Mounting, RequestStateUpdate) => (
            Mounted,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mounted".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (Mounting, MountFailed { reason }) => (
            Error(MountError::MountFailed {
                reason: reason.clone(),
            }),
            vec![
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: format!("mount failed: {}", reason),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Mounted ──
        (Mounted, Restart) => (
            Mounting,
            vec![
                Effect::DisconnectDrive,
                Effect::MountDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "restart requested".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (Mounted, Stop) => (
            Stopped,
            vec![
                Effect::DisconnectDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Error ──
        (Error(_), Start) | (Error(_), Restart) => (
            Mounting,
            vec![
                Effect::DisconnectDrive,
                Effect::MountDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "retrying from error state".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Stopped ──
        (Stopped, Start) => (
            Mounting,
            vec![
                Effect::MountDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "starting from stopped state".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Global: Config changed — restart to apply new config ──
        (_, ConfigChanged { .. }) => (
            Mounting,
            vec![
                Effect::DisconnectDrive,
                Effect::MountDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "config changed, restarting".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Global: Request state update from any state (no transition) ──
        (_, RequestStateUpdate) => (
            state,
            vec![Effect::EmitStateUpdate],
        ),

        // ── Global: Stop from any state ──
        (_, Stop) => (
            Stopped,
            vec![
                Effect::DisconnectDrive,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Invalid / no-op transitions ──
        _ => {
            // No state change, no effects
            (state, vec![])
        }
    }
}

// ── Sync State Machine ──
// Parallel to mount state — tracks on-demand sync lifecycle independently.

#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    /// Sync not enabled for this mount.
    Disabled,
    /// Registering the sync root with the OS.
    Registering,
    /// Sync root active, session connected, watcher running.
    Active,
    /// Sync root encountered an error.
    Error(String),
    /// Tearing down sync root.
    Deregistering,
}

impl fmt::Display for SyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyncState::Disabled => write!(f, "Disabled"),
            SyncState::Registering => write!(f, "Registering"),
            SyncState::Active => write!(f, "Active"),
            SyncState::Error(e) => write!(f, "Error: {}", e),
            SyncState::Deregistering => write!(f, "Deregistering"),
        }
    }
}

impl SyncState {
    /// Returns the state name for IPC messages.
    pub fn state_name(&self) -> &str {
        match self {
            SyncState::Disabled => "disabled",
            SyncState::Registering => "registering",
            SyncState::Active => "active",
            SyncState::Error(_) => "error",
            SyncState::Deregistering => "deregistering",
        }
    }
}

impl fmt::Display for MountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountState::Initializing => write!(f, "Initializing"),
            MountState::Mounting => write!(f, "Mounting"),
            MountState::Mounted => write!(f, "Mounted"),
            MountState::Error(e) => write!(f, "Error: {}", e),
            MountState::Stopped => write!(f, "Stopped"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_to_mounting() {
        let (state, effects) = transition(MountState::Initializing, MountEvent::Start);
        assert!(matches!(state, MountState::Mounting));
        assert!(effects.contains(&Effect::MountDrive));
        assert!(effects.contains(&Effect::EmitStateUpdate));
    }

    #[test]
    fn test_mounting_to_mounted() {
        let (state, effects) = transition(MountState::Mounting, MountEvent::RequestStateUpdate);
        assert!(matches!(state, MountState::Mounted));
        assert!(effects.contains(&Effect::UpdateTray));
        assert!(effects.contains(&Effect::EmitStateUpdate));
    }

    #[test]
    fn test_mounting_failed() {
        let (state, _effects) = transition(
            MountState::Mounting,
            MountEvent::MountFailed { reason: "network error".into() },
        );
        assert!(matches!(state, MountState::Error(MountError::MountFailed { .. })));
    }

    #[test]
    fn test_mounted_restart() {
        let (state, effects) = transition(MountState::Mounted, MountEvent::Restart);
        assert!(matches!(state, MountState::Mounting));
        assert!(effects.contains(&Effect::DisconnectDrive));
        assert!(effects.contains(&Effect::MountDrive));
    }

    #[test]
    fn test_mounted_stop() {
        let (state, effects) = transition(MountState::Mounted, MountEvent::Stop);
        assert!(matches!(state, MountState::Stopped));
        assert!(effects.contains(&Effect::DisconnectDrive));
    }

    #[test]
    fn test_error_retry() {
        let (state, _) = transition(
            MountState::Error(MountError::MountFailed { reason: "test".into() }),
            MountEvent::Start,
        );
        assert!(matches!(state, MountState::Mounting));
    }

    #[test]
    fn test_stopped_start() {
        let (state, _) = transition(MountState::Stopped, MountEvent::Start);
        assert!(matches!(state, MountState::Mounting));
    }

    #[test]
    fn test_global_stop() {
        let states = vec![
            MountState::Mounting,
            MountState::Mounted,
        ];
        for s in states {
            let (new_state, effects) = transition(s, MountEvent::Stop);
            assert!(matches!(new_state, MountState::Stopped));
            assert!(effects.contains(&Effect::EmitStateUpdate));
        }
    }

    #[test]
    fn test_invalid_event_is_noop() {
        let (state, effects) = transition(MountState::Initializing, MountEvent::Restart);
        assert!(matches!(state, MountState::Initializing));
        assert!(effects.is_empty());
    }
}
