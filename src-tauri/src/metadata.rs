use crate::db::Database;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Metadata JSON structure stored per item.
/// Matches the C++ Shot struct's metadata fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemMetadata {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub due_date: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub is_tracked: bool,
    /// Additional dynamic column values keyed by column name
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

pub struct MetadataManager {
    db: Arc<Database>,
    /// In-memory cache: item_path -> metadata_json
    cache: Mutex<HashMap<String, String>>,
}

impl MetadataManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Write metadata directly to the database (matching C++ synchronous behavior).
    pub fn write_immediate(
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
            .map_err(|e| e.to_string())?;

        // Update cache
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(item_path.to_string(), metadata_json.to_string());
        }

        Ok(())
    }

    /// Get metadata from cache first, then DB.
    pub fn get_metadata(&self, item_path: &str) -> Result<Option<String>, String> {
        // Check cache first
        {
            let cache = self.cache.lock().unwrap();
            if let Some(json) = cache.get(item_path) {
                return Ok(Some(json.clone()));
            }
        }

        // Fall back to DB
        self.db
            .with_conn(|conn| {
                let mut stmt = conn
                    .prepare_cached("SELECT metadata_json FROM item_metadata WHERE item_path = ?1")?;
                match stmt.query_row([item_path], |row| row.get::<_, String>(0)) {
                    Ok(json) => {
                        let mut cache = self.cache.lock().unwrap();
                        cache.insert(item_path.to_string(), json.clone());
                        Ok(Some(json))
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e),
                }
            })
            .map_err(|e| e.to_string())
    }

    /// Clear in-memory cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.clear();
    }
}
