/// SQLite cache for macOS FileProvider change tracking.
///
/// Tracks known files and visited folders so the agent can compute deltas
/// for the FileProvider extension's `enumerateChanges` calls.
///
/// Simplified vs Windows cache: no hydration/eviction tracking (FileProvider manages that).
/// Only tracks NAS-side metadata for three-way diffing.

use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Per-domain cache database.
pub struct MacosCache {
    conn: Mutex<Connection>,
    /// The NAS mount path (e.g., /Volumes/test1) for resolving filesystem reads.
    nas_root: PathBuf,
}

impl MacosCache {
    /// Open or create the cache DB for a domain.
    /// DB lives at ~/.local/share/ufb/cache/{domain}.db
    pub fn open(domain: &str, nas_root: PathBuf) -> Result<Self, String> {
        let cache_dir = if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".local/share/ufb/cache")
        } else {
            PathBuf::from("/tmp/ufb-cache")
        };
        std::fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed to create cache dir: {}", e))?;

        let db_path = cache_dir.join(format!("{}.db", domain));
        log::info!("[macos-cache] Opening DB at {}", db_path.display());

        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open cache DB: {}", e))?;

        conn.execute_batch("
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
        ").map_err(|e| format!("Failed to set pragmas: {}", e))?;

        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS known_files (
                path TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                is_dir INTEGER NOT NULL DEFAULT 0,
                nas_size INTEGER NOT NULL,
                nas_mtime REAL NOT NULL,
                nas_created REAL NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS visited_folders (
                path TEXT PRIMARY KEY,
                folder_mtime REAL NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ").map_err(|e| format!("Failed to create schema: {}", e))?;

        Ok(Self {
            conn: Mutex::new(conn),
            nas_root,
        })
    }

    /// Record a directory listing from an enumeration.
    /// Updates known_files for all entries and marks the folder as visited.
    pub fn record_enumeration(&self, relative_path: &str, entries: &[crate::messages::DirEntry]) {
        let conn = self.conn.lock().unwrap();

        // Record or update the visited folder
        let folder_mtime = self.get_folder_mtime(relative_path);
        conn.execute(
            "INSERT OR REPLACE INTO visited_folders (path, folder_mtime) VALUES (?1, ?2)",
            params![relative_path, folder_mtime],
        ).ok();

        // Build set of current entry paths for deletion detection
        let mut current_paths: HashSet<String> = HashSet::new();

        for entry in entries {
            let entry_path = if relative_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", relative_path, entry.name)
            };
            current_paths.insert(entry_path.clone());

            conn.execute(
                "INSERT OR REPLACE INTO known_files (path, name, is_dir, nas_size, nas_mtime, nas_created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry_path,
                    entry.name,
                    entry.is_dir as i32,
                    entry.size,
                    entry.modified,
                    entry.created,
                ],
            ).ok();
        }

        // Remove known_files entries that are no longer in this folder
        let prefix = if relative_path.is_empty() {
            String::new()
        } else {
            format!("{}/", relative_path)
        };

        let mut stmt = conn.prepare(
            "SELECT path FROM known_files WHERE path LIKE ?1 AND path NOT LIKE ?2"
        ).unwrap();

        let like_pattern = if prefix.is_empty() {
            "%".to_string()
        } else {
            format!("{}%", prefix)
        };
        let exclude_pattern = if prefix.is_empty() {
            "%/%".to_string()
        } else {
            format!("{}%/%", prefix)
        };

        let db_paths: Vec<String> = stmt.query_map(params![like_pattern, exclude_pattern], |row| {
            row.get(0)
        }).unwrap().filter_map(|r| r.ok()).collect();

        for db_path in &db_paths {
            if !current_paths.contains(db_path) {
                conn.execute("DELETE FROM known_files WHERE path = ?1", params![db_path]).ok();
            }
        }
    }

    /// Get changes since a given anchor (timestamp).
    /// Walks all visited folders and diffs NAS state against DB.
    pub fn get_changes_since(&self, _since_anchor: f64) -> ChangesResult {
        let conn = self.conn.lock().unwrap();

        let mut updated: Vec<ChangedEntry> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();

        // Load visited folders
        let mut stmt = conn.prepare("SELECT path, folder_mtime FROM visited_folders").unwrap();
        let mut folders: Vec<(String, f64)> = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect();
        drop(stmt);

        // Always include root
        if !folders.iter().any(|(p, _)| p.is_empty()) {
            folders.push(("".to_string(), 0.0));
        }

        for (folder_path, _stored_mtime) in &folders {
            let nas_folder = if folder_path.is_empty() {
                self.nas_root.clone()
            } else {
                self.nas_root.join(folder_path)
            };

            // Check if folder still exists
            if !nas_folder.is_dir() {
                continue;
            }

            // Readdir NAS folder
            let nas_entries: HashMap<String, (bool, u64, f64, f64)> = match std::fs::read_dir(&nas_folder) {
                Ok(rd) => rd.flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.starts_with('.') || name.starts_with('@') || name.starts_with('#') {
                            return None;
                        }
                        let meta = entry.metadata().ok()?;
                        let mtime = meta.modified().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()).unwrap_or(0.0);
                        let ctime = meta.created().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()).unwrap_or(0.0);
                        Some((name, (meta.is_dir(), meta.len(), mtime, ctime)))
                    })
                    .collect(),
                Err(_) => continue,
            };

            // Load known files for this folder from DB
            let prefix = if folder_path.is_empty() {
                String::new()
            } else {
                format!("{}/", folder_path)
            };
            let like_pattern = if prefix.is_empty() { "%".to_string() } else { format!("{}%", prefix) };
            let exclude_pattern = if prefix.is_empty() { "%/%".to_string() } else { format!("{}%/%", prefix) };

            let mut file_stmt = conn.prepare(
                "SELECT path, name, is_dir, nas_size, nas_mtime, nas_created FROM known_files WHERE path LIKE ?1 AND path NOT LIKE ?2"
            ).unwrap();

            let db_files: HashMap<String, (String, bool, u64, f64, f64)> = file_stmt.query_map(
                params![like_pattern, exclude_pattern],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,   // path
                        (
                            row.get::<_, String>(1)?,  // name
                            row.get::<_, i32>(2)? != 0, // is_dir
                            row.get::<_, u64>(3)?,     // size
                            row.get::<_, f64>(4)?,     // mtime
                            row.get::<_, f64>(5)?,     // created
                        ),
                    ))
                },
            ).unwrap().filter_map(|r| r.ok()).collect();

            let db_names: HashSet<String> = db_files.values().map(|(name, _, _, _, _)| name.clone()).collect();
            let nas_names: HashSet<String> = nas_entries.keys().cloned().collect();

            // New on NAS (not in DB)
            for name in nas_names.difference(&db_names) {
                if let Some((is_dir, size, mtime, ctime)) = nas_entries.get(name) {
                    let rel_path = if folder_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", folder_path, name)
                    };
                    updated.push(ChangedEntry {
                        relative_path: rel_path,
                        name: name.clone(),
                        is_dir: *is_dir,
                        size: *size,
                        modified: *mtime,
                        created: *ctime,
                    });
                }
            }

            // Deleted from NAS (in DB but not on NAS)
            for name in db_names.difference(&nas_names) {
                let rel_path = if folder_path.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", folder_path, name)
                };
                deleted.push(rel_path);
            }

            // Modified (size or mtime changed)
            for name in nas_names.intersection(&db_names) {
                if let Some((is_dir, nas_size, nas_mtime, nas_ctime)) = nas_entries.get(name) {
                    let rel_path = if folder_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", folder_path, name)
                    };
                    if let Some((_, _, db_size, db_mtime, _)) = db_files.get(&rel_path) {
                        if *nas_size != *db_size || (*nas_mtime - *db_mtime).abs() > 1.0 {
                            updated.push(ChangedEntry {
                                relative_path: rel_path,
                                name: name.clone(),
                                is_dir: *is_dir,
                                size: *nas_size,
                                modified: *nas_mtime,
                                created: *nas_ctime,
                            });
                        }
                    }
                }
            }

            drop(file_stmt);
        }

        // Update known_files with changes
        for entry in &updated {
            conn.execute(
                "INSERT OR REPLACE INTO known_files (path, name, is_dir, nas_size, nas_mtime, nas_created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![entry.relative_path, entry.name, entry.is_dir as i32, entry.size, entry.modified, entry.created],
            ).ok();
        }
        for path in &deleted {
            conn.execute("DELETE FROM known_files WHERE path = ?1", params![path]).ok();
        }

        let new_anchor = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        log::info!("[macos-cache] Changes: {} updated, {} deleted", updated.len(), deleted.len());

        ChangesResult {
            updated,
            deleted,
            new_anchor,
        }
    }

    /// Remove a known file from the DB (after deletion).
    pub fn remove_known_file(&self, relative_path: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM known_files WHERE path = ?1", params![relative_path]).ok();
    }

    /// Add or update a known file in the DB (after write).
    pub fn record_known_file(&self, relative_path: &str, entry: &crate::messages::DirEntry) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO known_files (path, name, is_dir, nas_size, nas_mtime, nas_created) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![relative_path, entry.name, entry.is_dir as i32, entry.size, entry.modified, entry.created],
        ).ok();
    }

    /// Get the NAS folder mtime for a relative path.
    fn get_folder_mtime(&self, relative_path: &str) -> f64 {
        let nas_folder = if relative_path.is_empty() {
            self.nas_root.clone()
        } else {
            self.nas_root.join(relative_path)
        };
        std::fs::metadata(&nas_folder)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }
}

/// Result of a change detection query.
pub struct ChangesResult {
    pub updated: Vec<ChangedEntry>,
    pub deleted: Vec<String>,
    pub new_anchor: f64,
}

/// A changed file/folder entry with full metadata.
#[derive(Clone)]
pub struct ChangedEntry {
    pub relative_path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: f64,
    pub created: f64,
}
