use serde::{Deserialize, Serialize};

// ── Agent → UFB ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToUfb {
    MountStateUpdate(MountStateUpdateMsg),
    Ack(AckMsg),
    Error(ErrorMsg),
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountStateUpdateMsg {
    pub mount_id: String,
    pub state: String,
    pub state_detail: String,
    /// On-demand sync state: "disabled", "registering", "active", "error", "deregistering"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_state: Option<String>,
    /// Human-readable sync status detail
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_state_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckMsg {
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorMsg {
    pub command_id: String,
    pub message: String,
}

// ── UFB → Agent ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UfbToAgent {
    StartMount(MountIdMsg),
    StopMount(MountIdMsg),
    RestartMount(MountIdMsg),
    ReloadConfig,
    GetStates,
    Ping,
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountIdMsg {
    pub mount_id: String,
    #[serde(default)]
    pub command_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_to_ufb_serde() {
        let msg = AgentToUfb::MountStateUpdate(MountStateUpdateMsg {
            mount_id: "primary-nas".into(),
            state: "mounted".into(),
            state_detail: "Mounted".into(),
            sync_state: None,
            sync_state_detail: None,
        });

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: AgentToUfb = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AgentToUfb::MountStateUpdate(_)));

        // Verify type tag
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["type"], "mount_state_update");
    }

    #[test]
    fn test_ufb_to_agent_serde() {
        let msg = UfbToAgent::StartMount(MountIdMsg {
            mount_id: "primary-nas".into(),
            command_id: "cmd-123".into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: UfbToAgent = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, UfbToAgent::StartMount(_)));
    }

    #[test]
    fn test_ping_pong() {
        let ping = serde_json::to_string(&UfbToAgent::Ping).unwrap();
        let pong = serde_json::to_string(&AgentToUfb::Pong).unwrap();

        let parsed_ping: UfbToAgent = serde_json::from_str(&ping).unwrap();
        let parsed_pong: AgentToUfb = serde_json::from_str(&pong).unwrap();

        assert!(matches!(parsed_ping, UfbToAgent::Ping));
        assert!(matches!(parsed_pong, AgentToUfb::Pong));
    }

    #[test]
    fn test_reload_config() {
        let msg = UfbToAgent::ReloadConfig;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: UfbToAgent = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, UfbToAgent::ReloadConfig));
    }

    #[test]
    fn test_all_ufb_to_agent_variants() {
        let variants = vec![
            UfbToAgent::StartMount(MountIdMsg { mount_id: "x".into(), command_id: "1".into() }),
            UfbToAgent::StopMount(MountIdMsg { mount_id: "x".into(), command_id: "2".into() }),
            UfbToAgent::RestartMount(MountIdMsg { mount_id: "x".into(), command_id: "3".into() }),
            UfbToAgent::ReloadConfig,
            UfbToAgent::GetStates,
            UfbToAgent::Ping,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let _: UfbToAgent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn test_all_agent_to_ufb_variants() {
        let variants = vec![
            AgentToUfb::Pong,
            AgentToUfb::Ack(AckMsg { command_id: "1".into() }),
            AgentToUfb::Error(ErrorMsg { command_id: "2".into(), message: "fail".into() }),
            AgentToUfb::MountStateUpdate(MountStateUpdateMsg {
                mount_id: "x".into(),
                state: "s".into(),
                state_detail: "d".into(),
                sync_state: Some("active".into()),
                sync_state_detail: Some("Watching 3 folders".into()),
            }),
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let _: AgentToUfb = serde_json::from_str(&json).unwrap();
        }
    }
}
