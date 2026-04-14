use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level config file schema for mounts.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountsConfig {
    pub version: u32,
    pub mounts: Vec<MountConfig>,
    /// Global cache root for all sync mounts.
    /// Default: %LOCALAPPDATA%\ufb\sync (Windows) or ~/.local/share/ufb/sync (Unix).
    /// Sync roots live at {sync_cache_root}\{share_name}\.
    #[serde(default)]
    pub sync_cache_root: Option<String>,
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

    /// User-facing mount path on macOS — symlink in ~/ufb/mounts/ (e.g. "~/ufb/mounts/nas-main")
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

impl MountsConfig {
    /// Resolve the effective cache root for sync mounts.
    pub fn cache_root(&self) -> PathBuf {
        if let Some(ref p) = self.sync_cache_root {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
        MountConfig::default_cache_root()
    }

    /// Detect enabled mounts with duplicate share names.
    /// Returns pairs of (share_name, [mount_ids]) for any collisions.
    pub fn share_name_collisions(&self) -> Vec<(String, Vec<String>)> {
        use std::collections::HashMap;
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        for m in &self.mounts {
            if m.enabled {
                by_name
                    .entry(m.share_name())
                    .or_default()
                    .push(m.id.clone());
            }
        }
        by_name
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .collect()
    }
}

impl MountConfig {
    /// Returns true if this mount uses on-demand sync instead of a traditional drive mount.
    pub fn is_sync_mode(&self) -> bool {
        self.sync_enabled
    }

    /// The local folder path for the sync root (where CF API / FileProvider operates).
    /// Windows: internal cache path ({cache_root}\{shareName}).
    /// macOS: FileProvider domain path (~/Library/CloudStorage/...) — cache_root ignored.
    /// Uses per-mount override if set (Windows/Linux only).
    pub fn sync_root_dir(&self, cache_root: &std::path::Path) -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            let _ = cache_root; // FileProvider controls cache location on macOS
            return self.fileprovider_domain_path();
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(ref p) = self.sync_root_path {
                if !p.is_empty() {
                    return PathBuf::from(p);
                }
            }
            cache_root.join(self.share_name())
        }
    }

    /// The path where macOS FileProvider materializes this domain.
    /// ~/Library/CloudStorage/{BundleId}-{share_name}/
    #[cfg(target_os = "macos")]
    pub fn fileprovider_domain_path(&self) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home)
            .join("Library/CloudStorage")
            .join(format!(
                "com.unionfiles.mediamount-tray.FileProvider-{}",
                self.share_name()
            ))
    }

    /// Extract the folder name from the NAS path (last path component).
    /// e.g., \\192.168.40.100\test1 → test1
    ///       \\nas\Jobs_Live → Jobs_Live
    ///       \\nas\FlameServer\Flame\FLAME_JOBS → FLAME_JOBS
    pub fn share_name(&self) -> String {
        self.nas_share_path
            .trim_end_matches('\\')
            .split('\\')
            .last()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.id)
            .to_string()
    }

    /// The user-facing volume path: C:\Volumes\ufb\{share_name} on Windows.
    /// This is a symlink pointing to either the UNC path (traditional) or cache dir (sync).
    pub fn volume_path(&self) -> String {
        Self::volumes_base()
            .join(self.share_name())
            .to_string_lossy()
            .to_string()
    }

    /// Base directory for user-facing volume symlinks.
    pub fn volumes_base() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(r"C:\Volumes\ufb")
        }
        #[cfg(target_os = "macos")]
        {
            // User-owned path — no admin required. ~/ufb/mounts/
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join("ufb/mounts");
            }
            PathBuf::from("/tmp/ufb-mounts")
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(".local/share/ufb/mnt");
            }
            PathBuf::from("/tmp/ufb-mnt")
        }
    }

    /// Private base directory where SMB shares are actually mounted on macOS
    /// (via `mount_smbfs`). User-facing symlinks in `volumes_base()` point
    /// here for non-sync mounts. Not a user-facing path.
    #[cfg(target_os = "macos")]
    pub fn smb_mount_base() -> PathBuf {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/share/ufb/smb-mounts");
        }
        PathBuf::from("/tmp/ufb-smb-mounts")
    }

    /// Default cache root for sync data.
    pub fn default_cache_root() -> PathBuf {
        #[cfg(windows)]
        {
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                return PathBuf::from(local).join("ufb").join("sync");
            }
            PathBuf::from(r"C:\ufb\sync")
        }
        #[cfg(not(windows))]
        {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(".local/share/ufb/sync");
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

    /// The user-facing path for this mount (where the user navigates).
    /// Windows: C:\Volumes\ufb\{shareName} (symlink)
    /// macOS: ~/ufb/mounts/{shareName} or explicit override
    /// Linux: ~/.local/share/ufb/mnt/{id} or explicit override
    pub fn mount_path(&self) -> String {
        #[cfg(windows)]
        {
            self.volume_path()
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(ref p) = self.mount_path_macos {
                if !p.is_empty() { return p.clone(); }
            }
            self.volume_path()
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
                sync_cache_root: None,
            };
        }
    };

    if !path.exists() {
        log::info!("No config file at {}, using empty config", path.display());
        return MountsConfig {
            version: 1,
            mounts: vec![],
            sync_cache_root: None,
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
                    sync_cache_root: None,
                }
            }
        },
        Err(e) => {
            log::error!("Failed to read config at {}: {}", path.display(), e);
            MountsConfig {
                version: 1,
                mounts: vec![],
                sync_cache_root: None,
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
            sync_cache_root: None,
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
    #[cfg(windows)]
    fn test_mount_path() {
        let m = test_mount("test", r"\\nas\test");
        assert_eq!(m.mount_path(), r"C:\Volumes\ufb\test");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_mount_path() {
        let m = test_mount("test", r"\\nas\test");
        let path = m.mount_path();
        assert!(path.ends_with("ufb/mounts/test"), "expected path ending with ufb/mounts/test, got: {}", path);
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

    /// Helper to create a minimal MountConfig for tests.
    fn test_mount(id: &str, nas_share_path: &str) -> MountConfig {
        MountConfig {
            id: id.into(),
            enabled: true,
            display_name: id.into(),
            nas_share_path: nas_share_path.into(),
            credential_key: id.into(),
            mount_drive_letter: String::new(),
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
        }
    }

    #[test]
    fn test_share_name() {
        let m = test_mount("primary-nas", r"\\192.168.40.100\Jobs_Live");
        assert_eq!(m.share_name(), "Jobs_Live");

        let m = test_mount("deep", r"\\nas\FlameServer\Flame\FLAME_JOBS");
        assert_eq!(m.share_name(), "FLAME_JOBS");

        // Trailing backslash
        let m = test_mount("trailing", r"\\nas\test\");
        assert_eq!(m.share_name(), "test");

        // Empty NAS path falls back to id
        let m = test_mount("fallback-id", "");
        assert_eq!(m.share_name(), "fallback-id");
    }

    #[test]
    fn test_volume_path_uses_share_name() {
        let m = test_mount("primary-nas", r"\\192.168.40.100\Jobs_Live");
        let vp = m.volume_path();
        // Last component should be share_name, not id
        assert!(vp.ends_with("Jobs_Live"), "volume_path should end with share_name, got: {}", vp);
        assert!(!vp.contains("primary-nas"), "volume_path should not contain id");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_mount_path_macos() {
        let m = test_mount("primary-nas", r"\\192.168.40.100\Jobs_Live");
        let path = m.mount_path();
        assert!(path.ends_with("ufb/mounts/Jobs_Live"), "expected path ending with ufb/mounts/Jobs_Live, got: {}", path);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_mount_path_macos_override() {
        let mut m = test_mount("primary-nas", r"\\nas\Jobs_Live");
        m.mount_path_macos = Some("/custom/path".into());
        assert_eq!(m.mount_path(), "/custom/path");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_fileprovider_domain_path() {
        let m = test_mount("primary-nas", r"\\nas\Jobs_Live");
        let fp = m.fileprovider_domain_path();
        let fp_str = fp.to_string_lossy();
        assert!(
            fp_str.contains("Library/CloudStorage/com.unionfiles.mediamount-tray.FileProvider-Jobs_Live"),
            "unexpected fileprovider path: {}", fp_str
        );
    }

    #[test]
    fn test_share_name_collisions() {
        let config = MountsConfig {
            version: 1,
            sync_cache_root: None,
            mounts: vec![
                test_mount("nas-a", r"\\server-a\Jobs_Live"),
                test_mount("nas-b", r"\\server-b\Jobs_Live"),
                test_mount("nas-c", r"\\server-c\Archive"),
            ],
        };
        let collisions = config.share_name_collisions();
        assert_eq!(collisions.len(), 1);
        let (name, ids) = &collisions[0];
        assert_eq!(name, "Jobs_Live");
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_no_collisions_when_disabled() {
        let mut m = test_mount("nas-b", r"\\server-b\Jobs_Live");
        m.enabled = false;
        let config = MountsConfig {
            version: 1,
            sync_cache_root: None,
            mounts: vec![
                test_mount("nas-a", r"\\server-a\Jobs_Live"),
                m,
            ],
        };
        assert!(config.share_name_collisions().is_empty());
    }
}
