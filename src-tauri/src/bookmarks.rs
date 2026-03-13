use crate::db::Database;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: i64,
    pub path: String,
    pub display_name: String,
    pub created_time: i64,
    pub is_project_folder: bool,
}

pub struct BookmarkManager {
    db: Arc<Database>,
}

impl BookmarkManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn add_bookmark(
        &self,
        path: &str,
        display_name: &str,
        is_project_folder: bool,
    ) -> Result<Bookmark, String> {
        let now = chrono::Utc::now().timestamp_millis();
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO bookmarks (path, display_name, created_time, is_project_folder)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![path, display_name, now, is_project_folder as i64],
                )?;
                let bm = conn.query_row(
                    "SELECT id, path, display_name, created_time, is_project_folder
                     FROM bookmarks WHERE path = ?1",
                    [path],
                    |row| {
                        Ok(Bookmark {
                            id: row.get(0)?,
                            path: row.get(1)?,
                            display_name: row.get(2)?,
                            created_time: row.get(3)?,
                            is_project_folder: row.get::<_, i64>(4)? != 0,
                        })
                    },
                )?;
                Ok(bm)
            })
            .map_err(|e| e.to_string())
    }

    pub fn remove_bookmark(&self, path: &str) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                conn.execute("DELETE FROM bookmarks WHERE path = ?1", [path])?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }

    pub fn get_all_bookmarks(&self) -> Result<Vec<Bookmark>, String> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id, path, display_name, created_time, is_project_folder
                     FROM bookmarks ORDER BY display_name",
                )?;
                let bookmarks = stmt
                    .query_map([], |row| {
                        Ok(Bookmark {
                            id: row.get(0)?,
                            path: row.get(1)?,
                            display_name: row.get(2)?,
                            created_time: row.get(3)?,
                            is_project_folder: row.get::<_, i64>(4)? != 0,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(bookmarks)
            })
            .map_err(|e| e.to_string())
    }

    pub fn update_bookmark_name(&self, path: &str, display_name: &str) -> Result<(), String> {
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE bookmarks SET display_name = ?1 WHERE path = ?2",
                    rusqlite::params![display_name, path],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}
