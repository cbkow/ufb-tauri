use crate::db::Database;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnOption {
    pub id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDefinition {
    pub id: Option<i64>,
    pub job_path: String,
    pub folder_name: String,
    pub column_name: String,
    pub column_type: String,
    pub column_order: i32,
    pub column_width: f64,
    pub is_visible: bool,
    pub default_value: Option<String>,
    pub options: Vec<ColumnOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPreset {
    pub id: Option<i64>,
    pub preset_name: String,
    pub columns_json: String,
    pub created_time: i64,
    pub modified_time: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetColumnDef {
    pub column_name: String,
    pub column_type: String,
    pub column_order: i32,
    pub column_width: f64,
    pub is_visible: bool,
    pub default_value: Option<String>,
    pub options: Vec<ColumnOption>,
}

/// Cache key: (job_path, folder_name)
type CacheKey = (String, String);

pub struct ColumnConfigManager {
    db: Arc<Database>,
    cache: Mutex<HashMap<CacheKey, Vec<ColumnDefinition>>>,
}

impl ColumnConfigManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn get_column_defs(
        &self,
        job_path: &str,
        folder_name: &str,
    ) -> Result<Vec<ColumnDefinition>, String> {
        // Check cache first
        let key = (job_path.to_string(), folder_name.to_string());
        {
            let cache = self.cache.lock().unwrap();
            if let Some(defs) = cache.get(&key) {
                return Ok(defs.clone());
            }
        }

        let defs = self
            .db
            .with_conn(|conn| {
                // Try folder-specific first, fall back to job-level ("*")
                let mut stmt = conn.prepare(
                    "SELECT id, job_path, folder_name, column_name, column_type,
                            column_order, column_width, is_visible, default_value
                     FROM column_definitions
                     WHERE job_path = ?1 AND (folder_name = ?2 OR folder_name = '*')
                     ORDER BY
                         CASE WHEN folder_name = ?2 THEN 0 ELSE 1 END,
                         column_order",
                )?;
                let mut defs: Vec<ColumnDefinition> = stmt
                    .query_map(rusqlite::params![job_path, folder_name], |row| {
                        Ok(ColumnDefinition {
                            id: Some(row.get(0)?),
                            job_path: row.get(1)?,
                            folder_name: row.get(2)?,
                            column_name: row.get(3)?,
                            column_type: row.get(4)?,
                            column_order: row.get(5)?,
                            column_width: row.get(6)?,
                            is_visible: row.get::<_, i64>(7)? != 0,
                            default_value: row.get(8)?,
                            options: vec![],
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;

                // Load options for each column
                for def in &mut defs {
                    if let Some(col_id) = def.id {
                        let mut opt_stmt = conn.prepare(
                            "SELECT id, option_name, option_color
                             FROM column_options WHERE column_id = ?1",
                        )?;
                        def.options = opt_stmt
                            .query_map([col_id], |row| {
                                Ok(ColumnOption {
                                    id: Some(row.get(0)?),
                                    name: row.get(1)?,
                                    color: row.get(2)?,
                                })
                            })?
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                }

                Ok(defs)
            })
            .map_err(|e| e.to_string())?;

        // Cache the result
        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(key, defs.clone());
        }

        Ok(defs)
    }

    pub fn add_column(&self, def: &ColumnDefinition) -> Result<i64, String> {
        let id = self
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO column_definitions
                     (job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        def.job_path,
                        def.folder_name,
                        def.column_name,
                        def.column_type,
                        def.column_order,
                        def.column_width,
                        def.is_visible as i64,
                        def.default_value,
                    ],
                )?;
                let col_id = conn.last_insert_rowid();

                // Insert options
                for opt in &def.options {
                    conn.execute(
                        "INSERT INTO column_options (column_id, option_name, option_color) VALUES (?1, ?2, ?3)",
                        rusqlite::params![col_id, opt.name, opt.color],
                    )?;
                }

                Ok(col_id)
            })
            .map_err(|e| e.to_string())?;

        self.invalidate_cache(&def.job_path, &def.folder_name);
        Ok(id)
    }

    pub fn update_column(&self, def: &ColumnDefinition) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                let col_id = def.id.ok_or(rusqlite::Error::InvalidParameterName(
                    "missing column id".to_string(),
                ))?;
                conn.execute(
                    "UPDATE column_definitions SET
                         column_name = ?1, column_type = ?2, column_order = ?3,
                         column_width = ?4, is_visible = ?5, default_value = ?6
                     WHERE id = ?7",
                    rusqlite::params![
                        def.column_name,
                        def.column_type,
                        def.column_order,
                        def.column_width,
                        def.is_visible as i64,
                        def.default_value,
                        col_id,
                    ],
                )?;

                // Replace options
                conn.execute(
                    "DELETE FROM column_options WHERE column_id = ?1",
                    [col_id],
                )?;
                for opt in &def.options {
                    conn.execute(
                        "INSERT INTO column_options (column_id, option_name, option_color) VALUES (?1, ?2, ?3)",
                        rusqlite::params![col_id, opt.name, opt.color],
                    )?;
                }

                Ok(())
            })
            .map_err(|e| e.to_string())?;

        self.invalidate_cache(&def.job_path, &def.folder_name);
        Ok(())
    }

    pub fn delete_column(&self, id: i64) -> Result<(), String> {
        // Get job_path and folder_name before deleting for cache invalidation
        let key = self
            .db
            .with_conn(|conn| {
                let key = conn.query_row(
                    "SELECT job_path, folder_name FROM column_definitions WHERE id = ?1",
                    [id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )?;
                conn.execute("DELETE FROM column_definitions WHERE id = ?1", [id])?;
                Ok(key)
            })
            .map_err(|e| e.to_string())?;

        self.invalidate_cache(&key.0, &key.1);
        Ok(())
    }

    fn invalidate_cache(&self, job_path: &str, folder_name: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.remove(&(job_path.to_string(), folder_name.to_string()));
    }

    pub fn invalidate_all_caches(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.clear();
    }

    // ── Column Presets ──

    pub fn get_column_presets(&self) -> Result<Vec<ColumnPreset>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, preset_name, columns_json, created_time, modified_time
                     FROM column_presets ORDER BY preset_name",
                )?;
                let presets = stmt
                    .query_map([], |row| {
                        Ok(ColumnPreset {
                            id: Some(row.get(0)?),
                            preset_name: row.get(1)?,
                            columns_json: row.get(2)?,
                            created_time: row.get(3)?,
                            modified_time: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(presets)
            })
            .map_err(|e| e.to_string())
    }

    pub fn save_column_preset(
        &self,
        name: &str,
        column: &ColumnDefinition,
    ) -> Result<i64, String> {
        // Strip identity fields, keep only the column definition data
        let preset_def = PresetColumnDef {
            column_name: column.column_name.clone(),
            column_type: column.column_type.clone(),
            column_order: column.column_order,
            column_width: column.column_width,
            is_visible: column.is_visible,
            default_value: column.default_value.clone(),
            options: column.options.clone(),
        };

        let json = serde_json::to_string(&preset_def)
            .map_err(|e| format!("Failed to serialize preset: {}", e))?;

        let now = crate::utils::current_time_ms();

        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO column_presets (preset_name, columns_json, created_time, modified_time)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(preset_name) DO UPDATE SET
                         columns_json = excluded.columns_json,
                         modified_time = excluded.modified_time",
                    rusqlite::params![name, json, now, now],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .map_err(|e| e.to_string())
    }

    pub fn delete_column_preset(&self, id: i64) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                conn.execute("DELETE FROM column_presets WHERE id = ?1", [id])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn add_preset_column(
        &self,
        preset_id: i64,
        job_path: &str,
        folder_name: &str,
    ) -> Result<i64, String> {
        // Read preset (single column definition)
        let columns_json: String = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT columns_json FROM column_presets WHERE id = ?1",
                    [preset_id],
                    |row| row.get(0),
                )
            })
            .map_err(|e| format!("Preset not found: {}", e))?;

        let preset_def: PresetColumnDef = serde_json::from_str(&columns_json)
            .map_err(|e| format!("Failed to parse preset JSON: {}", e))?;

        // Find the next column_order for this folder
        let next_order: i32 = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COALESCE(MAX(column_order), -1) + 1 FROM column_definitions
                     WHERE job_path = ?1 AND folder_name = ?2",
                    rusqlite::params![job_path, folder_name],
                    |row| row.get(0),
                )
            })
            .map_err(|e| e.to_string())?;

        let col_id = self
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO column_definitions
                     (job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        job_path,
                        folder_name,
                        preset_def.column_name,
                        preset_def.column_type,
                        next_order,
                        preset_def.column_width,
                        preset_def.is_visible as i64,
                        preset_def.default_value,
                    ],
                )?;
                let col_id = conn.last_insert_rowid();

                for opt in &preset_def.options {
                    conn.execute(
                        "INSERT INTO column_options (column_id, option_name, option_color) VALUES (?1, ?2, ?3)",
                        rusqlite::params![col_id, opt.name, opt.color],
                    )?;
                }

                Ok(col_id)
            })
            .map_err(|e| e.to_string())?;

        self.invalidate_cache(job_path, folder_name);
        Ok(col_id)
    }
}
