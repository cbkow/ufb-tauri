use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level config file schema for mounts.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountsConfig {
    pub version: u32,
    pub mounts: Vec<MountConfig>,
}

/// Configuration for a single mount.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub display_name: String,

    /// UNC path to the NAS share, e.g. "\\\\nas\\media"
    pub nas_share_path: String,

    /// Key used to store/retrieve credentials from Windows Credential Manager
    pub credential_key: String,

    /// Drive letter for rclone VFS mount (e.g. "R")
    pub rclone_drive_letter: String,

    /// Drive letter for SMB fallback mapping (e.g. "S") — legacy, no longer used
    #[serde(default)]
    pub smb_drive_letter: String,

    /// Path to the NTFS junction that switches between rclone/SMB (e.g. "M:\\media")
    pub junction_path: String,

    /// Local cache directory for rclone VFS
    pub cache_dir_path: String,

    // ── rclone tuning ──
    #[serde(default = "default_cache_max_size")]
    pub cache_max_size: String,

    #[serde(default = "default_cache_max_age")]
    pub cache_max_age: String,

    #[serde(default = "default_vfs_write_back")]
    pub vfs_write_back: String,

    #[serde(default = "default_vfs_read_chunk_size")]
    pub vfs_read_chunk_size: String,

    #[serde(default = "default_vfs_read_chunk_streams")]
    pub vfs_read_chunk_streams: u32,

    #[serde(default = "default_vfs_read_ahead")]
    pub vfs_read_ahead: String,

    #[serde(default = "default_buffer_size")]
    pub buffer_size: String,

    // ── Health/hysteresis tuning ──
    #[serde(default = "default_probe_interval_secs")]
    pub probe_interval_secs: u64,

    #[serde(default = "default_probe_timeout_ms")]
    pub probe_timeout_ms: u64,

    #[serde(default = "default_fallback_threshold")]
    pub fallback_threshold: u32,

    #[serde(default = "default_recovery_threshold")]
    pub recovery_threshold: u32,

    #[serde(default = "default_max_rclone_start_attempts")]
    pub max_rclone_start_attempts: u32,

    #[serde(default = "default_healthcheck_file_name")]
    pub healthcheck_file_name: String,

    #[serde(default)]
    pub extra_rclone_flags: Vec<String>,
}

impl MountConfig {
    /// Build the hysteresis config from mount config values.
    pub fn hysteresis_config(&self) -> crate::state::HysteresisConfig {
        crate::state::HysteresisConfig {
            fallback_threshold: self.fallback_threshold,
            recovery_threshold: self.recovery_threshold,
            max_rclone_start_attempts: self.max_rclone_start_attempts,
        }
    }
}

// ── Default value functions ──

fn default_true() -> bool {
    true
}
fn default_cache_max_size() -> String {
    "1T".into()
}
fn default_cache_max_age() -> String {
    "72h".into()
}
fn default_vfs_write_back() -> String {
    "10s".into()
}
fn default_vfs_read_chunk_size() -> String {
    "64M".into()
}
fn default_vfs_read_chunk_streams() -> u32 {
    8
}
fn default_vfs_read_ahead() -> String {
    "2G".into()
}
fn default_buffer_size() -> String {
    "512M".into()
}
fn default_probe_interval_secs() -> u64 {
    15
}
fn default_probe_timeout_ms() -> u64 {
    3000
}
fn default_fallback_threshold() -> u32 {
    3
}
fn default_recovery_threshold() -> u32 {
    5
}
fn default_max_rclone_start_attempts() -> u32 {
    3
}
fn default_healthcheck_file_name() -> String {
    ".healthcheck".into()
}

/// Resolve the config file path: %LOCALAPPDATA%/ufb/mounts.json
pub fn config_file_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let dir = PathBuf::from(local).join("ufb");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("mounts.json"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let dir = PathBuf::from(home).join(".local/share/ufb");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("mounts.json"));
        }
    }
    None
}

/// Load the mounts config from disk. Returns empty config if file doesn't exist.
pub fn load_config() -> MountsConfig {
    let path = match config_file_path() {
        Some(p) => p,
        None => {
            log::warn!("Could not determine config file path");
            return MountsConfig {
                version: 1,
                mounts: vec![],
            };
        }
    };

    if !path.exists() {
        log::info!("No config file at {}, using empty config", path.display());
        return MountsConfig {
            version: 1,
            mounts: vec![],
        };
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<MountsConfig>(&contents) {
            Ok(config) => {
                log::info!(
                    "Loaded config with {} mounts from {}",
                    config.mounts.len(),
                    path.display()
                );
                config
            }
            Err(e) => {
                log::error!("Failed to parse config at {}: {}", path.display(), e);
                MountsConfig {
                    version: 1,
                    mounts: vec![],
                }
            }
        },
        Err(e) => {
            log::error!("Failed to read config at {}: {}", path.display(), e);
            MountsConfig {
                version: 1,
                mounts: vec![],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_round_trip() {
        let config = MountsConfig {
            version: 1,
            mounts: vec![MountConfig {
                id: "primary-nas".into(),
                enabled: true,
                display_name: "Studio NAS".into(),
                nas_share_path: r"\\nas\media".into(),
                credential_key: "mediamount_primary-nas".into(),
                rclone_drive_letter: "R".into(),
                smb_drive_letter: "S".into(),
                junction_path: r"M:\media".into(),
                cache_dir_path: r"D:\rclone-cache\primary-nas".into(),
                cache_max_size: "1T".into(),
                cache_max_age: "72h".into(),
                vfs_write_back: "10s".into(),
                vfs_read_chunk_size: "64M".into(),
                vfs_read_chunk_streams: 8,
                vfs_read_ahead: "2G".into(),
                buffer_size: "512M".into(),
                probe_interval_secs: 15,
                probe_timeout_ms: 3000,
                fallback_threshold: 3,
                recovery_threshold: 5,
                max_rclone_start_attempts: 3,
                healthcheck_file_name: ".healthcheck".into(),
                extra_rclone_flags: vec![],
            }],
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: MountsConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.mounts.len(), 1);
        assert_eq!(parsed.mounts[0].id, "primary-nas");
        assert_eq!(parsed.mounts[0].rclone_drive_letter, "R");
        assert_eq!(parsed.mounts[0].cache_max_size, "1T");
    }

    #[test]
    fn test_defaults_applied() {
        let json = r#"{
            "version": 1,
            "mounts": [{
                "id": "test",
                "displayName": "Test",
                "nasSharePath": "\\\\nas\\test",
                "credentialKey": "test",
                "rcloneDriveLetter": "R",
                "smbDriveLetter": "S",
                "junctionPath": "M:\\test",
                "cacheDirPath": "D:\\cache"
            }]
        }"#;

        let config: MountsConfig = serde_json::from_str(json).unwrap();
        let m = &config.mounts[0];

        assert!(m.enabled); // default true
        assert_eq!(m.cache_max_size, "1T");
        assert_eq!(m.probe_interval_secs, 15);
        assert_eq!(m.fallback_threshold, 3);
        assert_eq!(m.recovery_threshold, 5);
        assert_eq!(m.healthcheck_file_name, ".healthcheck");
        assert!(m.extra_rclone_flags.is_empty());
    }

    #[test]
    fn test_malformed_json() {
        let json = r#"{ not valid json }"#;
        let result = serde_json::from_str::<MountsConfig>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_smb_drive_letter_defaults() {
        let json = r#"{
            "version": 1,
            "mounts": [{
                "id": "test",
                "displayName": "Test",
                "nasSharePath": "\\\\nas\\test",
                "credentialKey": "test",
                "rcloneDriveLetter": "R",
                "junctionPath": "M:\\test",
                "cacheDirPath": "D:\\cache"
            }]
        }"#;

        let config: MountsConfig = serde_json::from_str(json).unwrap();
        let m = &config.mounts[0];
        assert_eq!(m.smb_drive_letter, ""); // defaults to empty when missing
    }
}
