use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level config file schema for mounts.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountsConfig {
    pub version: u32,
    pub mounts: Vec<MountConfig>,
}

/// Configuration for a single mount.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountConfig {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub display_name: String,

    /// UNC path to the NAS share, e.g. "\\\\nas\\media"
    pub nas_share_path: String,

    /// Key used to store/retrieve credentials from the credential store
    pub credential_key: String,

    /// Drive letter that apps use to access media (e.g. "M") — Windows only
    #[serde(default)]
    pub mount_drive_letter: String,

    // ── Linux mount paths ──

    /// Path where SMB/CIFS is mounted (e.g. "/mnt/nas-smb")
    #[serde(default)]
    pub smb_mount_path: Option<String>,

    /// User-facing mount path — symlink that points to SMB mount (e.g. "/mnt/nas")
    #[serde(default)]
    pub mount_path_linux: Option<String>,

    // ── macOS mount path ──

    /// User-facing mount path on macOS — symlink in /opt/ufb/mounts/ (e.g. "/opt/ufb/mounts/nas-main")
    #[serde(default)]
    pub mount_path_macos: Option<String>,

    /// Whether subfolders of this mount are treated as subscribable jobs (default true)
    #[serde(default = "default_true")]
    pub is_jobs_folder: bool,

    // ── On-demand sync (Windows Cloud Files / macOS FileProvider) ──

    /// Enable on-demand sync for this mount. Mutually exclusive with drive-letter mount:
    /// if sync_enabled is true, the traditional drive mount is skipped.
    #[serde(default)]
    pub sync_enabled: bool,

    /// Override for the local sync root folder path.
    /// Default: %LOCALAPPDATA%\ufb\sync\{id}\ (Windows) or ~/.local/share/ufb/sync/{id}/ (Unix)
    #[serde(default)]
    pub sync_root_path: Option<String>,

    /// Maximum local cache size in bytes for hydrated files. 0 = unlimited.
    #[serde(default)]
    pub sync_cache_limit_bytes: u64,

    // ── Legacy fields — kept for backwards compat with existing config files ──

    /// Legacy: rclone drive letter (no longer used, silently ignored)
    #[serde(default)]
    pub rclone_drive_letter: String,

    /// Legacy: SMB fallback drive letter (no longer used)
    #[serde(default)]
    pub smb_drive_letter: String,

    /// Legacy: junction path (no longer used)
    #[serde(default)]
    pub junction_path: String,

    /// Legacy: rclone mount path (no longer used)
    #[serde(default)]
    pub rclone_mount_path: Option<String>,

    /// Legacy: rclone remote spec (no longer used)
    #[serde(default)]
    pub rclone_remote: Option<String>,

    /// Legacy: cache directory (no longer used)
    #[serde(default)]
    pub cache_dir_path: String,

    /// Legacy: cache tuning (no longer used)
    #[serde(default)]
    pub cache_max_size: String,
    #[serde(default)]
    pub cache_max_age: String,
    #[serde(default)]
    pub vfs_write_back: String,
    #[serde(default)]
    pub vfs_read_chunk_size: String,
    #[serde(default)]
    pub vfs_read_chunk_streams: u32,
    #[serde(default)]
    pub vfs_read_ahead: String,
    #[serde(default)]
    pub buffer_size: String,

    /// Legacy: health/hysteresis tuning (no longer used)
    #[serde(default)]
    pub probe_interval_secs: u64,
    #[serde(default)]
    pub probe_timeout_ms: u64,
    #[serde(default)]
    pub fallback_threshold: u32,
    #[serde(default)]
    pub recovery_threshold: u32,
    #[serde(default)]
    pub max_rclone_start_attempts: u32,
    #[serde(default)]
    pub healthcheck_file_name: String,
    #[serde(default)]
    pub extra_rclone_flags: Vec<String>,
}

impl MountConfig {
    /// Returns true if this mount uses on-demand sync instead of a traditional drive mount.
    pub fn is_sync_mode(&self) -> bool {
        self.sync_enabled
    }

    /// The local folder path for the sync root.
    /// Uses explicit override if set, otherwise auto-derives from the mount ID.
    pub fn sync_root_dir(&self) -> PathBuf {
        if let Some(ref p) = self.sync_root_path {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        Self::default_sync_root_base().join(&self.id)
    }

    /// Default base directory for sync roots.
    fn default_sync_root_base() -> PathBuf {
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                let dir = PathBuf::from(local).join("ufb").join("sync");
                let _ = std::fs::create_dir_all(&dir);
                return dir;
            }
            PathBuf::from(r"C:\ufb\sync")
        }
        #[cfg(not(windows))]
        {
            if let Some(home) = std::env::var_os("HOME") {
                let dir = PathBuf::from(home).join(".local/share/ufb/sync");
                let _ = std::fs::create_dir_all(&dir);
                return dir;
            }
            PathBuf::from("/tmp/ufb-sync")
        }
    }

    /// Base directory for auto-derived mount paths on Linux.
    /// Uses ~/.local/share/ufb/mnt/ which is user-writable without root.
    #[cfg(target_os = "linux")]
    fn linux_mnt_base(&self) -> std::path::PathBuf {
        let base = if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(".local/share/ufb/mnt")
        } else {
            std::path::PathBuf::from("/tmp/ufb-mnt")
        };
        let _ = std::fs::create_dir_all(&base);
        base
    }

    /// The path apps use to access the mount.
    /// If sync is enabled, returns the sync root path instead of the drive letter.
    /// Windows (drive): "M:\\"
    /// Windows (sync): "%LOCALAPPDATA%\\ufb\\sync\\{id}"
    /// macOS: mount_path_macos, or auto-derived /opt/ufb/mounts/<id>
    /// Linux: mount_path_linux, or auto-derived ~/.local/share/ufb/mnt/<id>
    pub fn mount_path(&self) -> String {
        if self.sync_enabled {
            return self.sync_root_dir().to_string_lossy().to_string();
        }
        #[cfg(windows)]
        {
            format!("{}:\\", self.mount_drive_letter)
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(ref p) = self.mount_path_macos {
                if !p.is_empty() { return p.clone(); }
            }
            format!("/opt/ufb/mounts/{}", self.id)
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(ref p) = self.mount_path_linux {
                if !p.is_empty() { return p.clone(); }
            }
            self.linux_mnt_base().join(&self.id).to_string_lossy().to_string()
        }
    }

    /// The path where SMB is mounted on Linux.
    /// Auto-derived to /media/$USER/ufb/<id>-smb if not set.
    #[cfg(target_os = "linux")]
    pub fn smb_target_path(&self) -> String {
        if let Some(ref p) = self.smb_mount_path {
            if !p.is_empty() { return p.clone(); }
        }
        self.linux_mnt_base().join(format!("{}-smb", self.id)).to_string_lossy().to_string()
    }
}

// ── Default value functions ──

fn default_true() -> bool {
    true
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
                mount_drive_letter: "M".into(),
                smb_mount_path: None,
                mount_path_linux: None,
                mount_path_macos: None,
                is_jobs_folder: true,
                sync_enabled: false,
                sync_root_path: None,
                sync_cache_limit_bytes: 0,
                rclone_drive_letter: String::new(),
                smb_drive_letter: String::new(),
                junction_path: String::new(),
                rclone_mount_path: None,
                rclone_remote: None,
                cache_dir_path: String::new(),
                cache_max_size: String::new(),
                cache_max_age: String::new(),
                vfs_write_back: String::new(),
                vfs_read_chunk_size: String::new(),
                vfs_read_chunk_streams: 0,
                vfs_read_ahead: String::new(),
                buffer_size: String::new(),
                probe_interval_secs: 0,
                probe_timeout_ms: 0,
                fallback_threshold: 0,
                recovery_threshold: 0,
                max_rclone_start_attempts: 0,
                healthcheck_file_name: String::new(),
                extra_rclone_flags: vec![],
            }],
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        let parsed: MountsConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.mounts.len(), 1);
        assert_eq!(parsed.mounts[0].id, "primary-nas");
        assert_eq!(parsed.mounts[0].mount_drive_letter, "M");
    }

    #[test]
    fn test_mount_path() {
        let config = MountsConfig {
            version: 1,
            mounts: vec![MountConfig {
                id: "test".into(),
                enabled: true,
                display_name: "Test".into(),
                nas_share_path: r"\\nas\test".into(),
                credential_key: "test".into(),
                mount_drive_letter: "M".into(),
                smb_mount_path: None,
                mount_path_linux: None,
                mount_path_macos: None,
                is_jobs_folder: true,
                sync_enabled: false,
                sync_root_path: None,
                sync_cache_limit_bytes: 0,
                rclone_drive_letter: String::new(),
                smb_drive_letter: String::new(),
                junction_path: String::new(),
                rclone_mount_path: None,
                rclone_remote: None,
                cache_dir_path: String::new(),
                cache_max_size: String::new(),
                cache_max_age: String::new(),
                vfs_write_back: String::new(),
                vfs_read_chunk_size: String::new(),
                vfs_read_chunk_streams: 0,
                vfs_read_ahead: String::new(),
                buffer_size: String::new(),
                probe_interval_secs: 0,
                probe_timeout_ms: 0,
                fallback_threshold: 0,
                recovery_threshold: 0,
                max_rclone_start_attempts: 0,
                healthcheck_file_name: String::new(),
                extra_rclone_flags: vec![],
            }],
        };
        assert_eq!(config.mounts[0].mount_path(), r"M:\");
    }

    #[test]
    fn test_defaults_applied() {
        // Minimal config — only required fields
        let json = r#"{
            "version": 1,
            "mounts": [{
                "id": "test",
                "displayName": "Test",
                "nasSharePath": "\\\\nas\\test",
                "credentialKey": "test",
                "mountDriveLetter": "M"
            }]
        }"#;

        let config: MountsConfig = serde_json::from_str(json).unwrap();
        let m = &config.mounts[0];

        assert!(m.enabled); // default true
        assert_eq!(m.mount_drive_letter, "M");
    }

    #[test]
    fn test_backwards_compat_old_config() {
        // Old config with rclone fields should still parse
        let json = r#"{
            "version": 1,
            "mounts": [{
                "id": "test",
                "displayName": "Test",
                "nasSharePath": "\\\\nas\\test",
                "credentialKey": "test",
                "rcloneDriveLetter": "R",
                "mountDriveLetter": "M",
                "cacheDirPath": "D:\\cache",
                "cacheMaxSize": "1T",
                "probeIntervalSecs": 15,
                "fallbackThreshold": 3,
                "healthcheckFileName": ".healthcheck"
            }]
        }"#;

        let config: MountsConfig = serde_json::from_str(json).unwrap();
        let m = &config.mounts[0];
        assert_eq!(m.id, "test");
        assert_eq!(m.mount_drive_letter, "M");
        // Legacy fields are parsed but ignored
        assert_eq!(m.rclone_drive_letter, "R");
    }

    #[test]
    fn test_malformed_json() {
        let json = r#"{ not valid json }"#;
        let result = serde_json::from_str::<MountsConfig>(json);
        assert!(result.is_err());
    }
}
