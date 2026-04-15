use crate::db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncStatus {
    Pending,
    Syncing,
    Synced,
    Stale,
    Error,
}

impl std::fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncStatus::Pending => write!(f, "Pending"),
            SyncStatus::Syncing => write!(f, "Syncing"),
            SyncStatus::Synced => write!(f, "Synced"),
            SyncStatus::Stale => write!(f, "Stale"),
            SyncStatus::Error => write!(f, "Error"),
        }
    }
}

impl From<String> for SyncStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "Syncing" => SyncStatus::Syncing,
            "Synced" => SyncStatus::Synced,
            "Stale" => SyncStatus::Stale,
            "Error" => SyncStatus::Error,
            _ => SyncStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub id: i64,
    pub job_path: String,
    pub job_name: String,
    pub is_active: bool,
    pub subscribed_time: i64,
    pub last_sync_time: Option<i64>,
    pub sync_status: SyncStatus,
    pub shot_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedItemRecord {
    pub item_path: String,
    pub job_path: String,
    pub job_name: String,
    pub folder_name: String,
    pub metadata_json: String,
    pub modified_time: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemMetadataRecord {
    pub item_path: String,
    pub folder_name: String,
    pub metadata_json: String,
    pub is_tracked: bool,
}

pub struct SubscriptionManager {
    db: Arc<Database>,
}

impl SubscriptionManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn subscribe_to_job(&self, job_path: &str, job_name: &str) -> Result<Subscription, String> {
        let now = chrono::Utc::now().timestamp_millis();
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO subscriptions (job_path, job_name, subscribed_time)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![job_path, job_name, now],
                )?;
                let sub = conn.query_row(
                    "SELECT id, job_path, job_name, is_active, subscribed_time,
                            last_sync_time, sync_status, shot_count
                     FROM subscriptions WHERE job_path = ?1",
                    [job_path],
                    |row| {
                        Ok(Subscription {
                            id: row.get(0)?,
                            job_path: row.get(1)?,
                            job_name: row.get(2)?,
                            is_active: row.get::<_, i64>(3)? != 0,
                            subscribed_time: row.get(4)?,
                            last_sync_time: row.get(5)?,
                            sync_status: SyncStatus::from(row.get::<_, String>(6)?),
                            shot_count: row.get(7)?,
                        })
                    },
                )?;
                Ok(sub)
            })
            .map_err(|e| e.to_string())
    }

    pub fn unsubscribe_from_job(&self, job_path: &str) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "DELETE FROM subscriptions WHERE job_path = ?1",
                    [job_path],
                )?;
                // Also clean up associated item_metadata
                conn.execute(
                    "DELETE FROM item_metadata WHERE job_path = ?1",
                    [job_path],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_all_subscriptions(&self) -> Result<Vec<Subscription>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, job_path, job_name, is_active, subscribed_time,
                            last_sync_time, sync_status, shot_count
                     FROM subscriptions ORDER BY job_name",
                )?;
                let subs = stmt
                    .query_map([], |row| {
                        Ok(Subscription {
                            id: row.get(0)?,
                            job_path: row.get(1)?,
                            job_name: row.get(2)?,
                            is_active: row.get::<_, i64>(3)? != 0,
                            subscribed_time: row.get(4)?,
                            last_sync_time: row.get(5)?,
                            sync_status: SyncStatus::from(row.get::<_, String>(6)?),
                            shot_count: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(subs)
            })
            .map_err(|e| e.to_string())
    }

    pub fn update_sync_status(
        &self,
        job_path: &str,
        status: SyncStatus,
    ) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp_millis();
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "UPDATE subscriptions SET sync_status = ?1, last_sync_time = ?2
                     WHERE job_path = ?3",
                )?;
                stmt.execute(rusqlite::params![status.to_string(), now, job_path])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn update_shot_count(&self, job_path: &str, count: i64) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare_cached("UPDATE subscriptions SET shot_count = ?1 WHERE job_path = ?2")?;
                stmt.execute(rusqlite::params![count, job_path])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    // --- Item Metadata ---

    pub fn upsert_item_metadata(
        &self,
        job_path: &str,
        item_path: &str,
        folder_name: &str,
        metadata_json: &str,
        is_tracked: bool,
    ) -> Result<(), String> {
        let now = chrono::Utc::now().timestamp_millis();
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "INSERT INTO item_metadata (item_path, job_path, folder_name, metadata_json, is_tracked, modified_time)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(item_path) DO UPDATE SET
                         metadata_json = excluded.metadata_json,
                         is_tracked = excluded.is_tracked,
                         modified_time = excluded.modified_time",
                )?;
                stmt.execute(rusqlite::params![
                    item_path, job_path, folder_name, metadata_json, is_tracked as i64, now
                ])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_item_metadata(&self, item_path: &str) -> Result<Option<String>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare_cached("SELECT metadata_json FROM item_metadata WHERE item_path = ?1")?;
                let result = stmt.query_row([item_path], |row| row.get::<_, String>(0));
                match result {
                    Ok(json) => Ok(Some(json)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_tracked_items(&self, job_path: &str) -> Result<Vec<TrackedItemRecord>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT im.item_path, im.job_path, s.job_name, im.folder_name, im.metadata_json, im.modified_time
                     FROM item_metadata im
                     JOIN subscriptions s ON im.job_path = s.job_path
                     WHERE im.job_path = ?1 AND im.is_tracked = 1",
                )?;
                let items = stmt
                    .query_map([job_path], |row| {
                        Ok(TrackedItemRecord {
                            item_path: row.get(0)?,
                            job_path: row.get(1)?,
                            job_name: row.get(2)?,
                            folder_name: row.get(3)?,
                            metadata_json: row.get(4)?,
                            modified_time: row.get(5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_all_tracked_items(&self) -> Result<Vec<TrackedItemRecord>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT im.item_path, im.job_path, s.job_name, im.folder_name, im.metadata_json, im.modified_time
                     FROM item_metadata im
                     JOIN subscriptions s ON im.job_path = s.job_path
                     WHERE im.is_tracked = 1",
                )?;
                let items = stmt
                    .query_map([], |row| {
                        Ok(TrackedItemRecord {
                            item_path: row.get(0)?,
                            job_path: row.get(1)?,
                            job_name: row.get(2)?,
                            folder_name: row.get(3)?,
                            metadata_json: row.get(4)?,
                            modified_time: row.get(5)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            })
            .map_err(|e| e.to_string())
    }

    pub fn delete_item_metadata(&self, item_path: &str) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                let mut stmt =
                    conn.prepare_cached("DELETE FROM item_metadata WHERE item_path = ?1")?;
                stmt.execute([item_path])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_all_item_metadata_for_job(
        &self,
        job_path: &str,
    ) -> Result<Vec<ItemMetadataRecord>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT item_path, folder_name, metadata_json, is_tracked
                     FROM item_metadata WHERE job_path = ?1",
                )?;
                let items = stmt
                    .query_map([job_path], |row| {
                        Ok(ItemMetadataRecord {
                            item_path: row.get(0)?,
                            folder_name: row.get(1)?,
                            metadata_json: row.get(2)?,
                            is_tracked: row.get::<_, i64>(3)? != 0,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            })
            .map_err(|e| e.to_string())
    }

    /// Get all metadata for items in a specific job+folder.
    pub fn get_folder_item_metadata(
        &self,
        job_path: &str,
        folder_name: &str,
    ) -> Result<Vec<ItemMetadataRecord>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT item_path, folder_name, metadata_json, is_tracked
                     FROM item_metadata WHERE job_path = ?1 AND folder_name = ?2",
                )?;
                let items = stmt
                    .query_map(rusqlite::params![job_path, folder_name], |row| {
                        Ok(ItemMetadataRecord {
                            item_path: row.get(0)?,
                            folder_name: row.get(1)?,
                            metadata_json: row.get(2)?,
                            is_tracked: row.get::<_, i64>(3)? != 0,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(items)
            })
            .map_err(|e| e.to_string())
    }
}
