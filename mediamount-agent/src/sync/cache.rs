/// Per-mount cache index backed by SQLite.
///
/// Tracks hydrated files (path, size, last-access timestamp) for LRU eviction.
/// DB location: {sync_root}/.cache_index.db
///
/// Eviction: after each hydration, check budget → evict LRU to 80% of limit.
/// Dehydration: cloud_filter::ext::file::FileExt::dehydrate(..) → CfDehydratePlaceholder.

use cloud_filter::ext::FileExt;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const EVICTION_TARGET_PERCENT: f64 = 0.80;

pub struct CacheIndex {
    db: Mutex<Connection>,
    cache_limit: u64,
    client_root: PathBuf,
    /// Shared with NasSyncFilter — files with refcount > 0 can't be evicted.
    open_handles: Arc<Mutex<HashMap<PathBuf, u32>>>,
}

impl CacheIndex {
    /// Open or create the cache index DB. If corrupt, delete and recreate.
    pub fn open(
        client_root: &Path,
        mount_id: &str,
        cache_limit: u64,
        open_handles: Arc<Mutex<HashMap<PathBuf, u32>>>,
    ) -> (Self, bool) {
        // Store DB outside the sync root — CF API intercepts file ops inside it.
        // %LOCALAPPDATA%\ufb\cache\{mount_id}.db
        let cache_dir = if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local).join("ufb").join("cache")
        } else {
            PathBuf::from(r"C:\ufb\cache")
        };
        let _ = std::fs::create_dir_all(&cache_dir);
        let db_path = cache_dir.join(format!("{}.db", mount_id));
        let (conn, needs_repair) = Self::open_or_repair(&db_path);

        let index = Self {
            db: Mutex::new(conn),
            cache_limit,
            client_root: client_root.to_path_buf(),
            open_handles,
        };
        (index, needs_repair)
    }

    fn open_or_repair(db_path: &Path) -> (Connection, bool) {
        // Try to open existing DB
        match Connection::open(db_path) {
            Ok(conn) => {
                // Check integrity
                if Self::init_schema(&conn).is_ok() && Self::check_integrity(&conn) {
                    return (conn, false);
                }
                // Corrupt — drop and recreate
                drop(conn);
                log::warn!("[cache] DB corrupt, recreating: {:?}", db_path);
                let _ = std::fs::remove_file(db_path);
            }
            Err(e) => {
                log::warn!("[cache] DB open failed ({}), recreating: {:?}", e, db_path);
                let _ = std::fs::remove_file(db_path);
            }
        }

        // Create fresh
        let conn = Connection::open(db_path).expect("Failed to create cache DB");
        Self::init_schema(&conn).expect("Failed to init cache schema");
        (conn, true)
    }

    fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;

             -- Known files: all placeholders we've seen, with NAS metadata + hydration state
             CREATE TABLE IF NOT EXISTS known_files (
                 path TEXT PRIMARY KEY,
                 nas_size INTEGER NOT NULL,
                 nas_mtime INTEGER NOT NULL,
                 is_hydrated INTEGER NOT NULL DEFAULT 0,
                 hydrated_size INTEGER DEFAULT 0,
                 last_accessed INTEGER DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_hydrated ON known_files(is_hydrated);
             CREATE INDEX IF NOT EXISTS idx_accessed ON known_files(last_accessed);

             -- Visited folders: folders the user has browsed (registered during FETCH_PLACEHOLDERS)
             CREATE TABLE IF NOT EXISTS visited_folders (
                 nas_path TEXT PRIMARY KEY,
                 client_path TEXT NOT NULL,
                 folder_mtime INTEGER NOT NULL DEFAULT 0
             );

             -- Metadata: key-value store for global state
             CREATE TABLE IF NOT EXISTS metadata (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );

             DROP TABLE IF EXISTS cache_index;",
        )?;

        // Migrate from old cache_index if it still exists (separate step —
        // referencing a non-existent table in execute_batch fails even with
        // a WHERE EXISTS guard because SQLite compiles the statement first).
        let has_old_table: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='cache_index')",
            [],
            |row| row.get(0),
        )?;
        if has_old_table {
            conn.execute_batch(
                "INSERT OR IGNORE INTO known_files (path, nas_size, nas_mtime, is_hydrated, hydrated_size, last_accessed)
                     SELECT path, size, 0, 1, size, accessed FROM cache_index;
                 DROP TABLE cache_index;",
            )?;
        }
        Ok(())
    }

    fn check_integrity(conn: &Connection) -> bool {
        conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .map(|result| result == "ok")
            .unwrap_or(false)
    }

    // ── Hydration tracking ──

    /// Record a successful hydration. Triggers eviction check.
    pub fn record_hydration(&self, path: &Path, size: u64) {
        if size == 0 {
            return;
        }
        let path_str = path.to_string_lossy();
        let now = Self::unix_now();

        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT INTO known_files (path, nas_size, nas_mtime, is_hydrated, hydrated_size, last_accessed)
             VALUES (?1, ?2, 0, 1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET is_hydrated=1, hydrated_size=?2, last_accessed=?3",
            params![path_str.as_ref(), size as i64, now],
        );
        drop(db);

        self.evict_if_over_budget();
    }

    /// Record a dehydration (OS-initiated or programmatic).
    pub fn record_dehydration(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE known_files SET is_hydrated=0, hydrated_size=0 WHERE path = ?1",
            params![path_str.as_ref()],
        );
    }

    /// Update last-access timestamp (LRU refresh on re-access).
    pub fn touch(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let now = Self::unix_now();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE known_files SET last_accessed = ?1 WHERE path = ?2",
            params![now, path_str.as_ref()],
        );
    }

    /// Update path after a rename.
    pub fn rename_entry(&self, old_path: &Path, new_path: &Path) {
        let old_str = old_path.to_string_lossy();
        let new_str = new_path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE known_files SET path = ?1 WHERE path = ?2",
            params![new_str.as_ref(), old_str.as_ref()],
        );
    }

    /// Total bytes of cached (hydrated) files.
    pub fn total_cached_bytes(&self) -> u64 {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT COALESCE(SUM(hydrated_size), 0) FROM known_files WHERE is_hydrated = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as u64
    }

    // ── Known files (placeholder tracking for reconciliation) ──

    /// Record a known file (placeholder created during FETCH_PLACEHOLDERS or watcher).
    pub fn record_known_file(&self, path: &Path, nas_size: u64, nas_mtime: i64) {
        let path_str = path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT INTO known_files (path, nas_size, nas_mtime) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET nas_size=?2, nas_mtime=?3",
            params![path_str.as_ref(), nas_size as i64, nas_mtime],
        );
    }

    /// Remove a known file (placeholder removed).
    pub fn remove_known_file(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "DELETE FROM known_files WHERE path = ?1",
            params![path_str.as_ref()],
        );
    }

    /// Get all known files for a folder (for reconciliation diff).
    pub fn known_files_in_folder(&self, folder_path: &Path) -> Vec<(String, i64, i64)> {
        let prefix = format!("{}\\", folder_path.to_string_lossy());
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT path, nas_size, nas_mtime FROM known_files WHERE path LIKE ?1 AND path NOT LIKE ?2")
            .unwrap();
        // Match direct children only (path starts with prefix, no additional backslash)
        stmt.query_map(params![format!("{}%", prefix), format!("{}%\\\\%", prefix)], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    // ── Visited folders ──

    /// Record a visited folder (called during FETCH_PLACEHOLDERS / watcher.register).
    pub fn record_visited_folder(&self, nas_path: &Path, client_path: &Path, folder_mtime: i64) {
        let nas_str = nas_path.to_string_lossy();
        let client_str = client_path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT OR REPLACE INTO visited_folders (nas_path, client_path, folder_mtime) VALUES (?1, ?2, ?3)",
            params![nas_str.as_ref(), client_str.as_ref(), folder_mtime],
        );
    }

    /// Ensure a visited folder exists in the DB. If already present, keeps existing mtime.
    /// If new, inserts with mtime=0 (forces reconciliation on next startup).
    pub fn ensure_visited_folder(&self, nas_path: &Path, client_path: &Path) {
        let nas_str = nas_path.to_string_lossy();
        let client_str = client_path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT OR IGNORE INTO visited_folders (nas_path, client_path, folder_mtime) VALUES (?1, ?2, 0)",
            params![nas_str.as_ref(), client_str.as_ref()],
        );
    }

    /// Get all visited folders for startup reconciliation.
    pub fn visited_folders(&self) -> Vec<(PathBuf, PathBuf, i64)> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT nas_path, client_path, folder_mtime FROM visited_folders")
            .unwrap();
        stmt.query_map([], |row| {
            let nas: String = row.get(0)?;
            let client: String = row.get(1)?;
            let mtime: i64 = row.get(2)?;
            Ok((PathBuf::from(nas), PathBuf::from(client), mtime))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    /// Update folder mtime after reconciliation.
    pub fn update_folder_mtime(&self, nas_path: &Path, folder_mtime: i64) {
        let nas_str = nas_path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE visited_folders SET folder_mtime = ?1 WHERE nas_path = ?2",
            params![folder_mtime, nas_str.as_ref()],
        );
    }

    // ── Metadata ──

    /// Get last_connected_at timestamp.
    pub fn last_connected_at(&self) -> Option<i64> {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT value FROM metadata WHERE key = 'last_connected_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
    }

    /// Update last_connected_at to now.
    pub fn update_last_connected(&self) {
        let now = Self::unix_now();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('last_connected_at', ?1)",
            params![now.to_string()],
        );
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    /// Evict LRU files until total <= 80% of cache_limit.
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
        let mut evicted_count = 0u32;
        let mut evicted_bytes = 0u64;

        // Get LRU candidates (hydrated files, oldest first)
        let victims = {
            let db = self.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT path, hydrated_size FROM known_files WHERE is_hydrated = 1 ORDER BY last_accessed ASC")
                .unwrap();
            let rows: Vec<(String, i64)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let open = self.open_handles.lock().unwrap();

        for (path_str, size) in &victims {
            if remaining <= target {
                break;
            }

            let path = PathBuf::from(path_str);

            // Skip files that are currently open
            if open.contains_key(&path) {
                continue;
            }

            if self.dehydrate_file(&path) {
                let db = self.db.lock().unwrap();
                let _ = db.execute(
                    "UPDATE known_files SET is_hydrated=0, hydrated_size=0 WHERE path = ?1",
                    params![path_str],
                );
                remaining -= *size as u64;
                evicted_count += 1;
                evicted_bytes += *size as u64;
            }
        }

        if evicted_count > 0 {
            log::info!(
                "[cache] Evicted {} files ({:.1} MB) — cache {:.1}/{:.1} MB",
                evicted_count,
                evicted_bytes as f64 / (1024.0 * 1024.0),
                remaining as f64 / (1024.0 * 1024.0),
                self.cache_limit as f64 / (1024.0 * 1024.0),
            );
        }
    }

    /// Clear ALL cached data for this mount. Returns (files_cleared, bytes_cleared).
    pub fn clear_all(&self) -> (u32, u64) {
        let entries = {
            let db = self.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT path, hydrated_size FROM known_files WHERE is_hydrated = 1")
                .unwrap();
            let rows: Vec<(String, i64)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let total = entries.len() as u32;
        let mut cleared_count = 0u32;
        let mut cleared_bytes = 0u64;

        for (path_str, size) in &entries {
            let path = PathBuf::from(path_str);
            if self.dehydrate_file(&path) {
                cleared_count += 1;
                cleared_bytes += *size as u64;
            }
        }

        // Mark all as dehydrated (don't delete — we still know about these files)
        let db = self.db.lock().unwrap();
        let _ = db.execute("UPDATE known_files SET is_hydrated=0, hydrated_size=0", []);

        log::info!(
            "[cache] Cleared {} of {} files ({:.1} MB)",
            cleared_count,
            total,
            cleared_bytes as f64 / (1024.0 * 1024.0),
        );

        (cleared_count, cleared_bytes)
    }

    /// Dehydrate a single file. Returns true on success.
    fn dehydrate_file(&self, path: &Path) -> bool {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return false,
        };
        match file.dehydrate(..) {
            Ok(()) => true,
            Err(e) => {
                log::debug!("[cache] Dehydrate skipped {:?}: {}", path, e);
                false
            }
        }
    }

    /// Rebuild the cache index from filesystem state.
    /// Walks the sync root, checks file attributes, populates DB for hydrated files.
    /// Optionally dehydrates all files first (for corruption recovery).
    pub fn rebuild(&self, dehydrate_all: bool) {
        log::info!(
            "[cache] Rebuilding cache index from {:?} (dehydrate_all={})",
            self.client_root,
            dehydrate_all
        );

        if dehydrate_all {
            self.dehydrate_tree(&self.client_root);
            // Clear all tracking — fresh start
            let db = self.db.lock().unwrap();
            let _ = db.execute("DELETE FROM known_files", []);
            let _ = db.execute("DELETE FROM visited_folders", []);
            let _ = db.execute("DELETE FROM metadata", []);
            return;
        }

        // Scan and index hydrated files
        self.scan_and_index(&self.client_root);
    }

    fn dehydrate_tree(&self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.dehydrate_tree(&path);
            } else if is_hydrated(&path) {
                self.dehydrate_file(&path);
            }
        }
    }

    fn scan_and_index(&self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        let now = Self::unix_now();

        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            // Skip hidden/internal files
            if name.starts_with('.') || name.starts_with('#') || name.starts_with('@') {
                continue;
            }

            if path.is_dir() {
                self.scan_and_index(&path);
            } else if is_hydrated(&path) {
                if let Ok(meta) = std::fs::metadata(&path) {
                    let path_str = path.to_string_lossy();
                    let db = self.db.lock().unwrap();
                    let _ = db.execute(
                        "INSERT INTO known_files (path, nas_size, nas_mtime, is_hydrated, hydrated_size, last_accessed)
                         VALUES (?1, ?2, 0, 1, ?2, ?3)
                         ON CONFLICT(path) DO UPDATE SET is_hydrated=1, hydrated_size=?2, last_accessed=?3",
                        params![path_str.as_ref(), meta.len() as i64, now],
                    );
                }
            }
        }
    }
}

/// Check if a file is a Cloud Files placeholder (has the reparse point attribute).
/// Returns false for regular user files. Cheap — no file open needed.
pub(crate) fn is_cf_placeholder(path: &Path) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetFileAttributesW;

    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
    attrs != u32::MAX && (attrs & 0x400) != 0 // FILE_ATTRIBUTE_REPARSE_POINT
}

/// Check if a file is a hydrated placeholder (data present locally).
/// Uses file attributes — cheap, no file open needed.
fn is_hydrated(path: &Path) -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetFileAttributesW;

    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
    if attrs == u32::MAX {
        return false;
    }
    let is_placeholder = (attrs & 0x400) != 0; // FILE_ATTRIBUTE_REPARSE_POINT
    let needs_recall = (attrs & 0x00400000) != 0; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    // Hydrated = is a placeholder AND data is locally present (no recall needed)
    is_placeholder && !needs_recall
}
