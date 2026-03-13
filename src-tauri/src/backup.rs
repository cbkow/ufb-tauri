use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub timestamp: i64,
    pub filename: String,
    pub created_by: String,
    pub shot_count: i64,
    pub checksum: String,
    pub uncompressed_size: u64,
    pub date: String,
}

pub struct BackupManager {
    device_id: String,
}

impl BackupManager {
    pub fn new(device_id: String) -> Self {
        Self { device_id }
    }

    /// Get the backup directory for a job.
    fn backup_dir(job_path: &str) -> PathBuf {
        Path::new(job_path).join(".ufb").join("backups")
    }

    /// Create a backup of job metadata.
    pub fn create_backup(
        &self,
        job_path: &str,
        metadata_json: &str,
    ) -> Result<BackupInfo, String> {
        let backup_dir = Self::backup_dir(job_path);
        std::fs::create_dir_all(&backup_dir)
            .map_err(|e| format!("Failed to create backup dir: {}", e))?;

        let now = chrono::Utc::now();
        let timestamp = now.timestamp_millis();
        let date_str = now.format("%Y-%m-%d_%H-%M-%S").to_string();
        let filename = format!("backup_{}.json", date_str);
        let backup_path = backup_dir.join(&filename);

        std::fs::write(&backup_path, metadata_json)
            .map_err(|e| format!("Failed to write backup: {}", e))?;

        let checksum = format!("{:x}", md5_hash(metadata_json.as_bytes()));
        let info = BackupInfo {
            timestamp,
            filename,
            created_by: self.device_id.clone(),
            shot_count: 0, // Caller should set this
            checksum,
            uncompressed_size: metadata_json.len() as u64,
            date: date_str,
        };

        // Write metadata file
        let meta_path = backup_dir.join("backup_metadata.json");
        let existing_meta = if meta_path.exists() {
            std::fs::read_to_string(&meta_path).unwrap_or_else(|_| "[]".to_string())
        } else {
            "[]".to_string()
        };
        let mut entries: Vec<BackupInfo> =
            serde_json::from_str(&existing_meta).unwrap_or_default();
        entries.push(info.clone());
        let meta_json = serde_json::to_string_pretty(&entries)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
        std::fs::write(&meta_path, meta_json)
            .map_err(|e| format!("Failed to write metadata: {}", e))?;

        Ok(info)
    }

    /// List available backups for a job.
    pub fn list_backups(&self, job_path: &str) -> Result<Vec<BackupInfo>, String> {
        let meta_path = Self::backup_dir(job_path).join("backup_metadata.json");
        if !meta_path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&meta_path)
            .map_err(|e| format!("Failed to read backup metadata: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse metadata: {}", e))
    }

    /// Restore a backup by filename.
    pub fn restore_backup(&self, job_path: &str, filename: &str) -> Result<String, String> {
        let backup_path = Self::backup_dir(job_path).join(filename);
        if !backup_path.exists() {
            return Err(format!("Backup file not found: {}", filename));
        }
        std::fs::read_to_string(&backup_path)
            .map_err(|e| format!("Failed to read backup: {}", e))
    }

    /// Check if a backup should be created today (one per day policy).
    pub fn should_backup_today(&self, job_path: &str) -> bool {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        match self.list_backups(job_path) {
            Ok(backups) => !backups.iter().any(|b| b.date.starts_with(&today)),
            Err(_) => true,
        }
    }

    /// Evict old backups beyond retention count.
    pub fn evict_old_backups(&self, job_path: &str, keep_count: usize) -> Result<usize, String> {
        let mut backups = self.list_backups(job_path)?;
        if backups.len() <= keep_count {
            return Ok(0);
        }

        // Sort by timestamp, oldest first
        backups.sort_by_key(|b| b.timestamp);
        let to_remove = backups.len() - keep_count;
        let backup_dir = Self::backup_dir(job_path);

        let mut removed = 0;
        for backup in backups.iter().take(to_remove) {
            let path = backup_dir.join(&backup.filename);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }

        // Update metadata file
        let remaining: Vec<_> = backups.into_iter().skip(to_remove).collect();
        let meta_path = backup_dir.join("backup_metadata.json");
        let meta_json = serde_json::to_string_pretty(&remaining)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        std::fs::write(&meta_path, meta_json)
            .map_err(|e| format!("Failed to write metadata: {}", e))?;

        Ok(removed)
    }
}

/// Simple hash for checksums (not cryptographic).
fn md5_hash(data: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}
