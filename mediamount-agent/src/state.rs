use serde::{Deserialize, Serialize};
use std::fmt;

// ── Mount State Machine ──
// Pure transition function — no side effects. Returns new state + effects for orchestrator.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MountState {
    Initializing,
    RcloneStarting { attempt: u32 },
    RcloneHealthy { consecutive_ok: u32 },
    RcloneDegraded { consecutive_fail: u32 },
    FallingBackToSmb,
    SmbActive,
    SmbRecovering { consecutive_ok: u32 },
    RecoveringToRclone { consecutive_ok: u32 },
    ManualOverride { target: MountTarget },
    Error(MountError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MountTarget {
    Rclone,
    Smb,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MountError {
    RcloneStartFailed { attempts: u32 },
    WinfspMissing,
    DriveLetterConflict { letter: char },
    ConfigInvalid { reason: String },
    JunctionFailed { reason: String },
    SmbFailed { reason: String },
}

impl fmt::Display for MountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountError::RcloneStartFailed { attempts } => {
                write!(f, "rclone failed to start after {} attempts", attempts)
            }
            MountError::WinfspMissing => write!(f, "WinFSP not installed"),
            MountError::DriveLetterConflict { letter } => {
                write!(f, "drive letter {}:\\ already in use", letter)
            }
            MountError::ConfigInvalid { reason } => write!(f, "invalid config: {}", reason),
            MountError::JunctionFailed { reason } => write!(f, "junction failed: {}", reason),
            MountError::SmbFailed { reason } => write!(f, "SMB failed: {}", reason),
        }
    }
}

#[derive(Debug, Clone)]
pub enum MountEvent {
    // Lifecycle
    Start,
    Stop,
    ConfigReloaded,

    // rclone lifecycle
    RcloneSpawned,
    RcloneStarted,
    RcloneStartFailed,
    RcloneDied,

    // Health probes
    ProbeOk,
    ProbeFailed,

    // Manual override
    ForceSwitchToSmb,
    ForceRclone,
    CancelOverride,

    // Platform errors
    WinfspMissing,
    DriveLetterConflict { letter: char },
    JunctionSwitchFailed { reason: String },
    SmbMapFailed { reason: String },

    // State query
    RequestStateUpdate,

    // Restart
    Restart,
    FlushAndRestart,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    SpawnRclone,
    KillRclone,
    SwitchJunctionToRclone,
    SwitchJunctionToSmb,
    EnsureSmbSession,
    StartProbeLoop,
    StopProbeLoop,
    UpdateTray,
    LogEvent { level: LogLevel, message: String },
    EmitStateUpdate,
    DeleteCacheDir,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Hysteresis configuration for state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HysteresisConfig {
    pub fallback_threshold: u32,
    pub recovery_threshold: u32,
    pub max_rclone_start_attempts: u32,
}

impl Default for HysteresisConfig {
    fn default() -> Self {
        Self {
            fallback_threshold: 3,
            recovery_threshold: 5,
            max_rclone_start_attempts: 3,
        }
    }
}

/// Pure state transition function. No side effects — returns new state + effects.
pub fn transition(
    state: MountState,
    event: MountEvent,
    config: &HysteresisConfig,
) -> (MountState, Vec<Effect>) {
    use MountEvent::*;
    use MountState::*;

    match (&state, event) {
        // ── Initializing ──
        // Map SMB first so apps have a working path while rclone starts
        (Initializing, Start) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "starting rclone (attempt 1)".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (Initializing, WinfspMissing) => (
            Error(MountError::WinfspMissing),
            vec![
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: "WinFSP not installed".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (Initializing, DriveLetterConflict { letter }) => (
            Error(MountError::DriveLetterConflict { letter }),
            vec![
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: format!("drive letter {}:\\ conflict", letter),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── RcloneStarting ──
        (RcloneStarting { .. }, RcloneStarted) => (
            RcloneHealthy { consecutive_ok: 0 },
            vec![
                Effect::SwitchJunctionToRclone,
                Effect::StartProbeLoop,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "rclone started, junction switched".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneStarting { attempt }, RcloneStartFailed) => {
            let attempt = *attempt;
            if attempt >= config.max_rclone_start_attempts {
                (
                    FallingBackToSmb,
                    vec![
                        Effect::EnsureSmbSession,
                        Effect::SwitchJunctionToSmb,
                        Effect::LogEvent {
                            level: LogLevel::Error,
                            message: format!(
                                "rclone failed after {} attempts, falling back to SMB",
                                attempt
                            ),
                        },
                        Effect::UpdateTray,
                        Effect::EmitStateUpdate,
                    ],
                )
            } else {
                (
                    RcloneStarting {
                        attempt: attempt + 1,
                    },
                    vec![
                        Effect::SpawnRclone,
                        Effect::LogEvent {
                            level: LogLevel::Warn,
                            message: format!(
                                "rclone start failed, retrying (attempt {})",
                                attempt + 1
                            ),
                        },
                        Effect::EmitStateUpdate,
                    ],
                )
            }
        }

        // ── RcloneHealthy ──
        (RcloneHealthy { .. }, ProbeOk) => {
            let consecutive_ok = match &state {
                RcloneHealthy { consecutive_ok } => consecutive_ok + 1,
                _ => 1,
            };
            (
                RcloneHealthy { consecutive_ok },
                vec![Effect::EmitStateUpdate],
            )
        }
        (RcloneHealthy { .. }, ProbeFailed) => (
            RcloneDegraded { consecutive_fail: 1 },
            vec![
                Effect::LogEvent {
                    level: LogLevel::Warn,
                    message: "probe failed, entering degraded state".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneHealthy { .. }, RcloneDied) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: "rclone process died, restarting".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneHealthy { .. }, ForceSwitchToSmb) => (
            ManualOverride {
                target: MountTarget::Smb,
            },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual switch to SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneHealthy { .. }, Restart) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "restart requested".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneHealthy { .. }, FlushAndRestart) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::DeleteCacheDir,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "flush and restart requested — cache wiped".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneHealthy { .. }, Stop) => (
            Initializing,
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped, junction switched to SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── RcloneDegraded ──
        (RcloneDegraded { consecutive_fail }, ProbeFailed) => {
            let consecutive_fail = *consecutive_fail + 1;
            if consecutive_fail >= config.fallback_threshold {
                (
                    FallingBackToSmb,
                    vec![
                        Effect::StopProbeLoop,
                        Effect::KillRclone,
                        Effect::EnsureSmbSession,
                        Effect::SwitchJunctionToSmb,
                        Effect::StartProbeLoop,
                        Effect::LogEvent {
                            level: LogLevel::Error,
                            message: format!(
                                "probe failed {} times, falling back to SMB",
                                consecutive_fail
                            ),
                        },
                        Effect::UpdateTray,
                        Effect::EmitStateUpdate,
                    ],
                )
            } else {
                (
                    RcloneDegraded { consecutive_fail },
                    vec![
                        Effect::LogEvent {
                            level: LogLevel::Warn,
                            message: format!(
                                "probe failed ({}/{})",
                                consecutive_fail, config.fallback_threshold
                            ),
                        },
                        Effect::EmitStateUpdate,
                    ],
                )
            }
        }
        (RcloneDegraded { .. }, ProbeOk) => (
            RcloneHealthy { consecutive_ok: 1 },
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "probe recovered, back to healthy".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneDegraded { .. }, RcloneDied) => (
            FallingBackToSmb,
            vec![
                Effect::StopProbeLoop,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::StartProbeLoop,
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: "rclone died while degraded, falling back to SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneDegraded { .. }, ForceSwitchToSmb) => (
            ManualOverride {
                target: MountTarget::Smb,
            },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual switch to SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneDegraded { .. }, ForceRclone) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual force rclone restart from degraded".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RcloneDegraded { .. }, Stop) => (
            Initializing,
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── FallingBackToSmb ──
        // This is a transient state; once SMB is mapped and junction switched, we go to SmbActive.
        // The orchestrator transitions to SmbActive after effects complete.
        (FallingBackToSmb, ProbeOk) => (
            SmbActive,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "SMB fallback active".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (FallingBackToSmb, ProbeFailed) => (
            SmbActive,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Warn,
                    message: "SMB fallback active (probe still failing)".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (FallingBackToSmb, ForceRclone) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual force rclone from SMB fallback".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (FallingBackToSmb, SmbMapFailed { reason }) => (
            Error(MountError::SmbFailed {
                reason: reason.clone(),
            }),
            vec![
                Effect::LogEvent {
                    level: LogLevel::Error,
                    message: format!("SMB fallback failed: {}", reason),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── SmbActive ──
        (SmbActive, ProbeOk) => (
            SmbRecovering { consecutive_ok: 1 },
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "rclone probe OK while on SMB, starting recovery".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (SmbActive, ProbeFailed) => {
            // Stay on SMB, probe keeps checking
            (SmbActive, vec![])
        }
        (SmbActive, ForceRclone) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,

                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual force rclone from SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (SmbActive, Stop) => (
            Initializing,
            vec![
                Effect::StopProbeLoop,

                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── SmbRecovering ──
        (SmbRecovering { consecutive_ok }, ProbeOk) => {
            let consecutive_ok = *consecutive_ok + 1;
            if consecutive_ok >= config.recovery_threshold {
                (
                    RecoveringToRclone { consecutive_ok: 0 },
                    vec![
                        Effect::SpawnRclone,
                        Effect::LogEvent {
                            level: LogLevel::Info,
                            message: format!(
                                "recovery threshold reached ({} probes OK), restarting rclone",
                                consecutive_ok
                            ),
                        },
                        Effect::EmitStateUpdate,
                    ],
                )
            } else {
                (
                    SmbRecovering { consecutive_ok },
                    vec![
                        Effect::LogEvent {
                            level: LogLevel::Info,
                            message: format!(
                                "recovery probe {}/{}",
                                consecutive_ok, config.recovery_threshold
                            ),
                        },
                        Effect::EmitStateUpdate,
                    ],
                )
            }
        }
        (SmbRecovering { .. }, ProbeFailed) => (
            SmbActive,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Warn,
                    message: "recovery probe failed, back to SMB".into(),
                },
                Effect::EmitStateUpdate,
            ],
        ),
        (SmbRecovering { .. }, ForceRclone) => (
            RcloneStarting { attempt: 1 },
            vec![
                Effect::StopProbeLoop,
                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual force rclone from SMB recovery".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── RecoveringToRclone ──
        (RecoveringToRclone { .. }, RcloneStarted) => (
            RcloneHealthy { consecutive_ok: 0 },
            vec![

                Effect::SwitchJunctionToRclone,
                Effect::StartProbeLoop,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "recovered to rclone, junction switched back".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (RecoveringToRclone { .. }, RcloneStartFailed) => (
            SmbActive,
            vec![
                Effect::StartProbeLoop,
                Effect::LogEvent {
                    level: LogLevel::Warn,
                    message: "rclone restart failed during recovery, staying on SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── ManualOverride ──
        (ManualOverride { target: MountTarget::Smb }, ForceRclone) => (
            RcloneStarting { attempt: 1 },
            vec![

                Effect::SpawnRclone,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual switch back to rclone".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (ManualOverride { target: MountTarget::Rclone }, ForceSwitchToSmb) => (
            ManualOverride {
                target: MountTarget::Smb,
            },
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual switch to SMB".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (ManualOverride { .. }, CancelOverride) => (
            Initializing,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "manual override cancelled, re-initializing".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),
        (ManualOverride { .. }, Stop) => (
            Initializing,
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,

                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped from manual override".into(),
                },
                Effect::UpdateTray,
                Effect::EmitStateUpdate,
            ],
        ),

        // ── Error ──
        (Error(_), Start) => (
            Initializing,
            vec![
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "retrying from error state".into(),
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
        // Kill rclone but leave SMB mapped and junction pointing at SMB
        // so artists keep a working path after shutdown.
        (_, Stop) => (
            Initializing,
            vec![
                Effect::StopProbeLoop,
                Effect::KillRclone,
                Effect::EnsureSmbSession,
                Effect::SwitchJunctionToSmb,
                Effect::LogEvent {
                    level: LogLevel::Info,
                    message: "mount stopped, junction switched to SMB".into(),
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

impl fmt::Display for MountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MountState::Initializing => write!(f, "Initializing"),
            MountState::RcloneStarting { attempt } => {
                write!(f, "Starting rclone (attempt {})", attempt)
            }
            MountState::RcloneHealthy { consecutive_ok } => {
                write!(f, "Healthy ({})", consecutive_ok)
            }
            MountState::RcloneDegraded { consecutive_fail } => {
                write!(f, "Degraded ({} failures)", consecutive_fail)
            }
            MountState::FallingBackToSmb => write!(f, "Falling back to SMB"),
            MountState::SmbActive => write!(f, "SMB active"),
            MountState::SmbRecovering { consecutive_ok } => {
                write!(f, "SMB recovering ({} OK)", consecutive_ok)
            }
            MountState::RecoveringToRclone { .. } => write!(f, "Recovering to rclone"),
            MountState::ManualOverride { target } => {
                write!(f, "Manual override ({:?})", target)
            }
            MountState::Error(e) => write!(f, "Error: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> HysteresisConfig {
        HysteresisConfig::default()
    }

    #[test]
    fn test_init_to_starting() {
        let (state, effects) =
            transition(MountState::Initializing, MountEvent::Start, &default_config());
        assert!(matches!(state, MountState::RcloneStarting { attempt: 1 }));
        assert!(effects.contains(&Effect::EnsureSmbSession));
        assert!(effects.contains(&Effect::SwitchJunctionToSmb));
        assert!(effects.contains(&Effect::SpawnRclone));
        assert!(effects.contains(&Effect::EmitStateUpdate));
    }

    #[test]
    fn test_starting_to_healthy() {
        let (state, effects) = transition(
            MountState::RcloneStarting { attempt: 1 },
            MountEvent::RcloneStarted,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::RcloneHealthy { consecutive_ok: 0 }
        ));
        assert!(effects.contains(&Effect::SwitchJunctionToRclone));
        assert!(effects.contains(&Effect::StartProbeLoop));
    }

    #[test]
    fn test_starting_retry_then_fallback() {
        let config = default_config();

        // Attempt 1 fails
        let (state, effects) = transition(
            MountState::RcloneStarting { attempt: 1 },
            MountEvent::RcloneStartFailed,
            &config,
        );
        assert!(matches!(state, MountState::RcloneStarting { attempt: 2 }));
        assert!(effects.contains(&Effect::SpawnRclone));

        // Attempt 2 fails
        let (state, _) = transition(state, MountEvent::RcloneStartFailed, &config);
        assert!(matches!(state, MountState::RcloneStarting { attempt: 3 }));

        // Attempt 3 fails → fallback
        let (state, effects) = transition(state, MountEvent::RcloneStartFailed, &config);
        assert!(matches!(state, MountState::FallingBackToSmb));
        assert!(effects.contains(&Effect::EnsureSmbSession));
        assert!(effects.contains(&Effect::SwitchJunctionToSmb));
    }

    #[test]
    fn test_healthy_probe_ok_increments() {
        let (state, _) = transition(
            MountState::RcloneHealthy { consecutive_ok: 5 },
            MountEvent::ProbeOk,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::RcloneHealthy { consecutive_ok: 6 }
        ));
    }

    #[test]
    fn test_healthy_to_degraded_on_probe_fail() {
        let (state, _) = transition(
            MountState::RcloneHealthy { consecutive_ok: 10 },
            MountEvent::ProbeFailed,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::RcloneDegraded {
                consecutive_fail: 1
            }
        ));
    }

    #[test]
    fn test_degraded_accumulates_then_fallback() {
        let config = default_config();

        let (state, _) = transition(
            MountState::RcloneDegraded {
                consecutive_fail: 1,
            },
            MountEvent::ProbeFailed,
            &config,
        );
        assert!(matches!(
            state,
            MountState::RcloneDegraded {
                consecutive_fail: 2
            }
        ));

        let (state, effects) = transition(state, MountEvent::ProbeFailed, &config);
        assert!(matches!(state, MountState::FallingBackToSmb));
        assert!(effects.contains(&Effect::KillRclone));
        assert!(effects.contains(&Effect::EnsureSmbSession));
        assert!(effects.contains(&Effect::SwitchJunctionToSmb));
    }

    #[test]
    fn test_degraded_recovers_on_probe_ok() {
        let (state, _) = transition(
            MountState::RcloneDegraded {
                consecutive_fail: 2,
            },
            MountEvent::ProbeOk,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::RcloneHealthy { consecutive_ok: 1 }
        ));
    }

    #[test]
    fn test_smb_active_stays_on_probe_fail() {
        let (state, effects) =
            transition(MountState::SmbActive, MountEvent::ProbeFailed, &default_config());
        assert!(matches!(state, MountState::SmbActive));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_smb_recovery_threshold() {
        let config = default_config();

        let mut state = MountState::SmbActive;
        // First probe OK → SmbRecovering
        let (s, _) = transition(state, MountEvent::ProbeOk, &config);
        state = s;
        assert!(matches!(
            state,
            MountState::SmbRecovering { consecutive_ok: 1 }
        ));

        // Accumulate probes
        for i in 2..config.recovery_threshold {
            let (s, _) = transition(state, MountEvent::ProbeOk, &config);
            state = s;
            assert!(matches!(
                state,
                MountState::SmbRecovering { consecutive_ok } if consecutive_ok == i
            ));
        }

        // Threshold reached → RecoveringToRclone
        let (state, effects) = transition(state, MountEvent::ProbeOk, &config);
        assert!(matches!(state, MountState::RecoveringToRclone { .. }));
        assert!(effects.contains(&Effect::SpawnRclone));
    }

    #[test]
    fn test_smb_recovery_resets_on_failure() {
        let (state, _) = transition(
            MountState::SmbRecovering { consecutive_ok: 3 },
            MountEvent::ProbeFailed,
            &default_config(),
        );
        assert!(matches!(state, MountState::SmbActive));
    }

    #[test]
    fn test_recovering_to_rclone_success() {
        let (state, effects) = transition(
            MountState::RecoveringToRclone { consecutive_ok: 0 },
            MountEvent::RcloneStarted,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::RcloneHealthy { consecutive_ok: 0 }
        ));
        assert!(effects.contains(&Effect::SwitchJunctionToRclone));
    }

    #[test]
    fn test_recovering_to_rclone_failure() {
        let (state, _) = transition(
            MountState::RecoveringToRclone { consecutive_ok: 0 },
            MountEvent::RcloneStartFailed,
            &default_config(),
        );
        assert!(matches!(state, MountState::SmbActive));
    }

    #[test]
    fn test_manual_override_smb() {
        let (state, effects) = transition(
            MountState::RcloneHealthy { consecutive_ok: 10 },
            MountEvent::ForceSwitchToSmb,
            &default_config(),
        );
        assert!(matches!(
            state,
            MountState::ManualOverride {
                target: MountTarget::Smb
            }
        ));
        assert!(effects.contains(&Effect::KillRclone));
        assert!(effects.contains(&Effect::EnsureSmbSession));
    }

    #[test]
    fn test_manual_override_back_to_rclone() {
        let (state, effects) = transition(
            MountState::ManualOverride {
                target: MountTarget::Smb,
            },
            MountEvent::ForceRclone,
            &default_config(),
        );
        assert!(matches!(state, MountState::RcloneStarting { attempt: 1 }));
        assert!(effects.contains(&Effect::SpawnRclone));
    }

    #[test]
    fn test_cancel_override() {
        let (state, _) = transition(
            MountState::ManualOverride {
                target: MountTarget::Smb,
            },
            MountEvent::CancelOverride,
            &default_config(),
        );
        assert!(matches!(state, MountState::Initializing));
    }

    #[test]
    fn test_error_retry() {
        let (state, _) = transition(
            MountState::Error(MountError::RcloneStartFailed { attempts: 3 }),
            MountEvent::Start,
            &default_config(),
        );
        assert!(matches!(state, MountState::Initializing));
    }

    #[test]
    fn test_winfsp_missing() {
        let (state, _) = transition(
            MountState::Initializing,
            MountEvent::WinfspMissing,
            &default_config(),
        );
        assert!(matches!(state, MountState::Error(MountError::WinfspMissing)));
    }

    #[test]
    fn test_healthy_rclone_died() {
        let (state, effects) = transition(
            MountState::RcloneHealthy { consecutive_ok: 50 },
            MountEvent::RcloneDied,
            &default_config(),
        );
        assert!(matches!(state, MountState::RcloneStarting { attempt: 1 }));
        assert!(effects.contains(&Effect::StopProbeLoop));
        assert!(effects.contains(&Effect::SpawnRclone));
    }

    #[test]
    fn test_restart() {
        let (state, effects) = transition(
            MountState::RcloneHealthy { consecutive_ok: 10 },
            MountEvent::Restart,
            &default_config(),
        );
        assert!(matches!(state, MountState::RcloneStarting { attempt: 1 }));
        assert!(effects.contains(&Effect::KillRclone));
        assert!(effects.contains(&Effect::SpawnRclone));
        assert!(!effects.contains(&Effect::DeleteCacheDir));
    }

    #[test]
    fn test_flush_and_restart() {
        let (state, effects) = transition(
            MountState::RcloneHealthy { consecutive_ok: 10 },
            MountEvent::FlushAndRestart,
            &default_config(),
        );
        assert!(matches!(state, MountState::RcloneStarting { attempt: 1 }));
        assert!(effects.contains(&Effect::DeleteCacheDir));
        assert!(effects.contains(&Effect::KillRclone));
        assert!(effects.contains(&Effect::SpawnRclone));
    }

    #[test]
    fn test_invalid_event_is_noop() {
        // ProbeOk in Initializing state — no-op
        let (state, effects) = transition(
            MountState::Initializing,
            MountEvent::ProbeOk,
            &default_config(),
        );
        assert!(matches!(state, MountState::Initializing));
        assert!(effects.is_empty());
    }

    #[test]
    fn test_global_stop() {
        // Stop from any state should return Initializing
        let states = vec![
            MountState::RcloneHealthy { consecutive_ok: 5 },
            MountState::RcloneDegraded {
                consecutive_fail: 2,
            },
            MountState::SmbActive,
            MountState::SmbRecovering { consecutive_ok: 3 },
        ];
        for s in states {
            let (new_state, effects) = transition(s, MountEvent::Stop, &default_config());
            assert!(matches!(new_state, MountState::Initializing));
            assert!(effects.contains(&Effect::EmitStateUpdate));
        }
    }

    #[test]
    fn test_degraded_rclone_died_goes_to_smb() {
        let (state, effects) = transition(
            MountState::RcloneDegraded {
                consecutive_fail: 2,
            },
            MountEvent::RcloneDied,
            &default_config(),
        );
        assert!(matches!(state, MountState::FallingBackToSmb));
        assert!(effects.contains(&Effect::EnsureSmbSession));
        assert!(effects.contains(&Effect::SwitchJunctionToSmb));
    }
}
