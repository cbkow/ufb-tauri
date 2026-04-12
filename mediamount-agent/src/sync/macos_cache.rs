/// SQLite cache for macOS FileProvider change tracking + LRU eviction.
///
/// Tracks known files, visited folders, and hydration state so the agent can:
/// - Compute deltas for the extension's `enumerateChanges` calls
/// - Enforce cache limits via LRU eviction (extension calls `evictItem`)

use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const EVICTION_TARGET_PERCENT: f64 = 0.8;

/// Per-domain cache database.
pub struct MacosCache {
    conn: Mutex<Connection>,
    nas_root: PathBuf,
    cache_limit: u64,
    /// Paths pending eviction — consumed by getChanges response.
    pending_evictions: Mutex<Vec<String>>,
}

impl MacosCache {
    /// Open or create the cache DB for a domain.
    pub fn open(domain: &str, nas_root: PathBuf, cache_limit: u64) -> Result<Self, String> {
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

        // Create tables (without hydration columns — migration adds them)
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

        // Migrate: add hydration columns if missing
        let has_hydrated: bool = conn.prepare("SELECT is_hydrated FROM known_files LIMIT 0")
            .is_ok();
        if !has_hydrated {
            log::info!("[macos-cache] Migrating: adding hydration columns");
            let _ = conn.execute_batch("
                ALTER TABLE known_files ADD COLUMN is_hydrated INTEGER NOT NULL DEFAULT 0;
                ALTER TABLE known_files ADD COLUMN hydrated_size INTEGER DEFAULT 0;
                ALTER TABLE known_files ADD COLUMN last_accessed REAL DEFAULT 0;
            ");
        }

        // Create indexes (after migration ensures columns exist)
        let _ = conn.execute_batch("
            CREATE INDEX IF NOT EXISTS idx_hydrated ON known_files(is_hydrated);
            CREATE INDEX IF NOT EXISTS idx_accessed ON known_files(last_accessed);
        ");

        Ok(Self {
            conn: Mutex::new(conn),
            nas_root,
            cache_limit,
            pending_evictions: Mutex::new(Vec::new()),
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

    // ── Hydration tracking + LRU eviction ──

    /// Record that a file was downloaded (materialized). Triggers eviction check.
    pub fn record_hydration(&self, relative_path: &str, size: u64) {
        if size == 0 {
            return;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE known_files SET is_hydrated=1, hydrated_size=?1, last_accessed=?2 WHERE path=?3",
            params![size as i64, now, relative_path],
        ).ok();
        drop(conn);

        self.evict_if_over_budget();
    }

    /// Update last_accessed time (called on each file read).
    pub fn touch(&self, relative_path: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE known_files SET last_accessed=?1 WHERE path=?2",
            params![now, relative_path],
        ).ok();
    }

    /// Total bytes of hydrated (locally cached) files.
    pub fn total_cached_bytes(&self) -> u64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COALESCE(SUM(hydrated_size), 0) FROM known_files WHERE is_hydrated=1",
            [],
            |row| row.get::<_, i64>(0),
        ).unwrap_or(0) as u64
    }

    /// Check if over budget and compute eviction candidates.
    fn evict_if_over_budget(&self) {
        if self.cache_limit == 0 {
            return;
        }

        let total = self.total_cached_bytes();
        if total <= self.cache_limit {
            return;
        }

        let target = (self.cache_limit as f64 * EVICTION_TARGET_PERCENT) as u64;
        let mut remaining = total;

        // Get LRU candidates (hydrated files, oldest accessed first)
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, hydrated_size FROM known_files WHERE is_hydrated=1 AND is_dir=0 ORDER BY last_accessed ASC"
        ).unwrap();
        let victims: Vec<(String, i64)> = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect();
        drop(stmt);

        let mut evict_paths = Vec::new();
        for (path, size) in &victims {
            if remaining <= target {
                break;
            }
            evict_paths.push(path.clone());
            // Mark as not hydrated in DB
            conn.execute(
                "UPDATE known_files SET is_hydrated=0, hydrated_size=0 WHERE path=?1",
                params![path],
            ).ok();
            remaining -= *size as u64;
        }
        drop(conn);

        if !evict_paths.is_empty() {
            let evicted_bytes = total - remaining;
            log::info!(
                "[macos-cache] Eviction: {} files ({:.1} MB) — cache {:.1}/{:.1} MB",
                evict_paths.len(),
                evicted_bytes as f64 / 1_048_576.0,
                remaining as f64 / 1_048_576.0,
                self.cache_limit as f64 / 1_048_576.0,
            );
            self.pending_evictions.lock().unwrap().extend(evict_paths);
        }
    }

    /// Mark ALL hydrated files for eviction. Used by "Clear Cache" button.
    pub fn clear_all_hydrated(&self) -> (u32, u64) {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, hydrated_size FROM known_files WHERE is_hydrated=1 AND is_dir=0"
        ).unwrap();
        let files: Vec<(String, i64)> = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }).unwrap().filter_map(|r| r.ok()).collect();
        drop(stmt);

        let mut count = 0u32;
        let mut bytes = 0u64;
        let mut evict_paths = Vec::new();

        for (path, size) in &files {
            conn.execute(
                "UPDATE known_files SET is_hydrated=0, hydrated_size=0 WHERE path=?1",
                params![path],
            ).ok();
            evict_paths.push(path.clone());
            count += 1;
            bytes += *size as u64;
        }
        drop(conn);

        if !evict_paths.is_empty() {
            log::info!("[macos-cache] Clear cache: {} files, {:.1} MB", count, bytes as f64 / 1_048_576.0);
            self.pending_evictions.lock().unwrap().extend(evict_paths);
        }

        (count, bytes)
    }

    /// Drain pending eviction candidates (consumed by getChanges response).
    pub fn drain_pending_evictions(&self) -> Vec<String> {
        std::mem::take(&mut *self.pending_evictions.lock().unwrap())
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
