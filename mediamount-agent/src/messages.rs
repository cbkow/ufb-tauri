use serde::{Deserialize, Serialize};

// ── Agent → UFB ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToUfb {
    MountStateUpdate(MountStateUpdateMsg),
    Ack(AckMsg),
    Error(ErrorMsg),
    Pong,
    ConflictDetected(ConflictDetectedMsg),
    /// Response to `UfbToAgent::GetCacheStats`. Zero values are emitted
    /// for mounts that have no cache (plain SMB on macOS, Windows without
    /// sync) so the frontend can treat the message as authoritative.
    CacheStats(CacheStatsMsg),
    /// Hydration state changed for a file. Consumed by the FinderSync
    /// extension to paint overlay badges in Finder. Broadcast to every
    /// connected client; non-FinderSync clients can ignore.
    BadgeUpdate(BadgeUpdateMsg),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeKind {
    /// Fully hydrated — all bytes cached locally.
    Hydrated,
    /// Partial — some chunks cached (chunk_bitmap has bits set).
    Partial,
    /// No local cache — reads will proxy to SMB. FinderSync should drop
    /// any existing badge for this path.
    Uncached,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BadgeUpdateMsg {
    pub domain: String,
    /// Path relative to the share root.
    pub relpath: String,
    pub badge: BadgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsMsg {
    pub mount_id: String,
    /// Total bytes of hydrated (locally cached) file content for this share.
    pub hydrated_bytes: u64,
    /// Number of files currently hydrated.
    pub hydrated_count: u64,
    /// Command ID to correlate with the triggering GetCacheStats request.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDetectedMsg {
    /// Domain or share name where the conflict occurred.
    pub domain: String,
    /// Path the user was writing to (canonical relative path inside the share).
    pub original_path: String,
    /// Path where the conflicting write was preserved (sidecar file name).
    pub conflict_path: String,
    /// Hostname of this machine — included in the sidecar name for traceability.
    pub host: String,
    /// Unix epoch seconds when the conflict was detected.
    pub detected_at: u64,
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
    /// True if symlink creation requires elevation (Windows)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_elevation: Option<bool>,
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
    ClearSyncCache(MountIdMsg),
    /// Ask the agent how much content is currently cached for a share.
    /// Agent replies with `AgentToUfb::CacheStats`. Cheap (one indexed
    /// SUM query); safe to poll on dialog open.
    GetCacheStats(MountIdMsg),
    CreateSymlinks,
    ReloadConfig,
    GetStates,
    Ping,
    Quit,
    /// Tell the agent that something user-facing happened (window focus,
    /// refresh button, tab switch). Agent routes the signal to the platform's
    /// freshness mechanism — Darwin notification on macOS (extension picks it
    /// up and signals .workingSet), watcher hint on Windows.
    FreshnessSweep(FreshnessSweepMsg),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FreshnessSweepMsg {
    /// Optional domain / share name to scope the sweep. `None` = all enabled mounts.
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub command_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountIdMsg {
    pub mount_id: String,
    #[serde(default)]
    pub command_id: String,
}

// ── FileProvider Extension → Agent (file operations) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileOpsRequest {
    ListDir(ListDirReq),
    Stat(StatReq),
    ReadFile(ReadFileReq),
    WriteFile(WriteFileReq),
    DeleteItem(DeleteItemReq),
    RenameItem(RenameItemReq),
    RecordEnumeration(RecordEnumerationReq),
    GetChanges(GetChangesReq),
    ClearCache(ClearCacheReq),
    /// List currently-hydrated relative paths for a domain. Cheap DB query
    /// (no NAS I/O) — used by the extension's clear-cache flow to drive
    /// `evictItem` calls without going through `ListDir`.
    EvictAll(EvictAllReq),
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListDirReq {
    pub request_id: String,
    /// The FileProvider domain / share name (e.g., "test1")
    pub domain: String,
    /// Path relative to the share root (e.g., "project/assets"). Empty string = root.
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatReq {
    pub request_id: String,
    pub domain: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileReq {
    pub request_id: String,
    pub domain: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFileReq {
    pub request_id: String,
    pub domain: String,
    /// Destination path relative to share root (e.g., "project/new_file.txt")
    pub relative_path: String,
    /// Path to the source file in the app group container (written by the extension)
    pub source_path: String,
    /// True if this is a directory creation (no source file)
    #[serde(default)]
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteItemReq {
    pub request_id: String,
    pub domain: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameItemReq {
    pub request_id: String,
    pub domain: String,
    pub old_path: String,
    pub new_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearCacheReq {
    pub request_id: String,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvictAllReq {
    pub request_id: String,
    pub domain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordEnumerationReq {
    pub request_id: String,
    pub domain: String,
    pub relative_path: String,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChangesReq {
    pub request_id: String,
    pub domain: String,
    pub since_anchor: String,
}

// ── Agent → FileProvider Extension (file operation responses) ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileOpsResponse {
    DirListing(DirListingResp),
    FileStat(FileStatResp),
    FileReady(FileReadyResp),
    WriteOk(WriteOkResp),
    DeleteOk(DeleteOkResp),
    RenameOk(RenameOkResp),
    RecordOk(RecordOkResp),
    Changes(ChangesResp),
    EvictList(EvictListResp),
    Error(FileOpsErrorResp),
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvictListResp {
    pub request_id: String,
    /// Relative paths of currently-hydrated files in the domain.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListingResp {
    pub request_id: String,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since Unix epoch
    pub modified: f64,
    /// Seconds since Unix epoch
    pub created: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatResp {
    pub request_id: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: f64,
    pub created: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileReadyResp {
    pub request_id: String,
    /// Path to the temp file in the app group container
    pub temp_path: String,
    pub size: u64,
    pub modified: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOkResp {
    pub request_id: String,
    pub size: u64,
    pub modified: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOkResp {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOpsErrorResp {
    pub request_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameOkResp {
    pub request_id: String,
    pub new_path: String,
    pub size: u64,
    pub modified: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOkResp {
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesResp {
    pub request_id: String,
    pub updated: Vec<ChangedEntry>,
    pub deleted: Vec<String>,
    #[serde(default)]
    pub evict: Vec<String>,
    pub new_anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedEntry {
    pub relative_path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: f64,
    pub created: f64,
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
            needs_elevation: None,
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
                needs_elevation: None,
            }),
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let _: AgentToUfb = serde_json::from_str(&json).unwrap();
        }
    }
}
