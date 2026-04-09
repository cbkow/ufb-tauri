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
             CREATE TABLE IF NOT EXISTS cache_index (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 accessed INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_accessed ON cache_index(accessed);",
        )
    }

    fn check_integrity(conn: &Connection) -> bool {
        conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .map(|result| result == "ok")
            .unwrap_or(false)
    }

    /// Record a successful hydration. Triggers eviction check.
    pub fn record_hydration(&self, path: &Path, size: u64) {
        if size == 0 {
            return;
        }
        let path_str = path.to_string_lossy();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "INSERT OR REPLACE INTO cache_index (path, size, accessed) VALUES (?1, ?2, ?3)",
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
            "DELETE FROM cache_index WHERE path = ?1",
            params![path_str.as_ref()],
        );
    }

    /// Update last-access timestamp (LRU refresh on re-access).
    pub fn touch(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE cache_index SET accessed = ?1 WHERE path = ?2",
            params![now, path_str.as_ref()],
        );
    }

    /// Update path after a rename.
    pub fn rename_entry(&self, old_path: &Path, new_path: &Path) {
        let old_str = old_path.to_string_lossy();
        let new_str = new_path.to_string_lossy();
        let db = self.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE cache_index SET path = ?1 WHERE path = ?2",
            params![new_str.as_ref(), old_str.as_ref()],
        );
    }

    /// Total bytes of cached (hydrated) files.
    pub fn total_cached_bytes(&self) -> u64 {
        let db = self.db.lock().unwrap();
        db.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM cache_index",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as u64
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

        // Get LRU candidates
        let victims = {
            let db = self.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT path, size FROM cache_index ORDER BY accessed ASC")
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
                    "DELETE FROM cache_index WHERE path = ?1",
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
    /// Caller should emit progress events per file.
    pub fn clear_all(&self) -> (u32, u64) {
        let entries = {
            let db = self.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT path, size FROM cache_index")
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

        // Clear the entire DB
        let db = self.db.lock().unwrap();
        let _ = db.execute("DELETE FROM cache_index", []);

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
            // DB should be empty after dehydrating everything
            let db = self.db.lock().unwrap();
            let _ = db.execute("DELETE FROM cache_index", []);
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

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
                        "INSERT OR REPLACE INTO cache_index (path, size, accessed) VALUES (?1, ?2, ?3)",
                        params![path_str.as_ref(), meta.len() as i64, now],
                    );
                }
            }
        }
    }
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
