use serde::Deserialize;

/// VFS stats from rclone RC API.
#[derive(Debug, Deserialize)]
pub struct VfsStats {
    #[serde(default)]
    pub disk_cache: Option<DiskCacheStats>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskCacheStats {
    #[serde(default)]
    pub bytes_used: u64,
    #[serde(default)]
    pub uploads_in_progress: u32,
    #[serde(default)]
    pub uploads_queued: u32,
}

impl DiskCacheStats {
    /// Total dirty (in-progress + queued) file count.
    pub fn dirty_count(&self) -> u32 {
        self.uploads_in_progress + self.uploads_queued
    }
}

/// Query VFS stats from rclone's RC API.
pub async fn query_vfs_stats(rc_port: u16) -> Result<VfsStats, String> {
    let url = format!("http://127.0.0.1:{}/vfs/stats", rc_port);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .send()
        .await
        .map_err(|e| format!("VFS stats request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("VFS stats returned {}", resp.status()));
    }

    resp.json::<VfsStats>()
        .await
        .map_err(|e| format!("Failed to parse VFS stats: {}", e))
}

/// Trigger a cache flush via rclone RC API (uploads all dirty files).
pub async fn flush_cache(rc_port: u16) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/vfs/queue-set-expiry", rc_port);
    let client = reqwest::Client::new();

    // Set all upload expiry to 0 to force immediate upload
    let body = serde_json::json!({ "expiry": 0 });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Cache flush request failed: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Cache flush returned {}", resp.status()));
    }

    Ok(())
}
