/// SQLite cache for macOS FileProvider change tracking + LRU eviction.
///
/// Tracks known files, visited folders, and hydration state so the agent can:
/// - Compute deltas for the extension's `enumerateChanges` calls
/// - Enforce cache limits via LRU eviction (extension calls `evictItem`)

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const EVICTION_TARGET_PERCENT: f64 = 0.8;
const POOL_SIZE: u32 = 6;

/// Content cache chunk size — matches the NFS `rsize` mount option (1 MiB)
/// so each NFS READ RPC maps cleanly to one chunk.
pub const CHUNK_SIZE: u64 = 1024 * 1024;

/// How many chunks cover a file of `size` bytes.
#[inline]
pub fn num_chunks(size: u64) -> u64 {
    (size + CHUNK_SIZE - 1) / CHUNK_SIZE
}

/// Bit operations on the chunk bitmap. Bits are packed LSB-first within each
/// byte: chunk `i` lives in byte `i/8`, bit `i%8`.
#[inline]
pub fn bit_is_set(bitmap: &[u8], chunk: u64) -> bool {
    let byte = (chunk / 8) as usize;
    let mask = 1u8 << ((chunk % 8) as u8);
    bitmap.get(byte).map(|b| b & mask != 0).unwrap_or(false)
}

#[inline]
pub fn set_bit(bitmap: &mut Vec<u8>, chunk: u64) {
    let byte = (chunk / 8) as usize;
    let mask = 1u8 << ((chunk % 8) as u8);
    if bitmap.len() <= byte {
        bitmap.resize(byte + 1, 0);
    }
    bitmap[byte] |= mask;
}

/// Check if every bit in `[0, total_chunks)` is set.
#[inline]
pub fn bitmap_is_complete(bitmap: &[u8], total_chunks: u64) -> bool {
    if total_chunks == 0 {
        return true;
    }
    let full_bytes = (total_chunks / 8) as usize;
    // Every full byte must be 0xFF.
    for i in 0..full_bytes {
        if bitmap.get(i).copied().unwrap_or(0) != 0xFF {
            return false;
        }
    }
    // Partial trailing byte: only the low `remainder` bits must be set.
    let remainder = total_chunks % 8;
    if remainder > 0 {
        let mask = (1u8 << remainder) - 1;
        if bitmap.get(full_bytes).copied().unwrap_or(0) & mask != mask {
            return false;
        }
    }
    true
}

/// Parent directory of a relative path. "a/b/c.txt" → "a/b". "foo.txt" → "".
/// Used to populate + query the indexed `parent_path` column on `known_files`.
#[inline]
pub fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

type SqlitePool = Pool<SqliteConnectionManager>;
type SqliteConn = PooledConnection<SqliteConnectionManager>;

/// Subset of `known_files` fields needed to build an NFS `fattr3`. Returned
/// from the cache-serving accessors so callers don't have to know the schema.
#[derive(Debug, Clone)]
pub struct CachedAttr {
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since Unix epoch (NAS mtime).
    pub mtime: f64,
    /// Seconds since Unix epoch (NAS ctime).
    pub created: f64,
    pub is_hydrated: bool,
    pub hydrated_size: u64,
}

/// Per-domain cache database.
pub struct MacosCache {
    pool: SqlitePool,
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

        // One-time setup: enable WAL, create tables, apply migrations, create indexes.
        // Must happen on a single serial connection before the pool opens, because
        // ALTER TABLE concurrency is not safe to race.
        {
            let mut conn = Connection::open(&db_path)
                .map_err(|e| format!("Failed to open cache DB: {}", e))?;

            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;",
            )
            .map_err(|e| format!("Failed to set pragmas: {}", e))?;

            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS known_files (
                    fh INTEGER PRIMARY KEY AUTOINCREMENT,
                    path TEXT NOT NULL UNIQUE,
                    name TEXT NOT NULL,
                    is_dir INTEGER NOT NULL DEFAULT 0,
                    nas_size INTEGER NOT NULL,
                    nas_mtime REAL NOT NULL,
                    nas_created REAL NOT NULL DEFAULT 0,
                    is_hydrated INTEGER NOT NULL DEFAULT 0,
                    hydrated_size INTEGER DEFAULT 0,
                    last_accessed REAL DEFAULT 0,
                    last_verified_at REAL DEFAULT 0,
                    parent_path TEXT NOT NULL DEFAULT '',
                    chunk_bitmap BLOB DEFAULT NULL
                );

                CREATE TABLE IF NOT EXISTS visited_folders (
                    path TEXT PRIMARY KEY,
                    folder_mtime REAL NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS metadata (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )
            .map_err(|e| format!("Failed to create schema: {}", e))?;

            // Migrate: add hydration columns if missing
            let has_hydrated: bool = conn
                .prepare("SELECT is_hydrated FROM known_files LIMIT 0")
                .is_ok();
            if !has_hydrated {
                log::info!("[macos-cache] Migrating: adding hydration columns");
                let _ = conn.execute_batch(
                    "ALTER TABLE known_files ADD COLUMN is_hydrated INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE known_files ADD COLUMN hydrated_size INTEGER DEFAULT 0;
                     ALTER TABLE known_files ADD COLUMN last_accessed REAL DEFAULT 0;",
                );
            }

            let has_verified: bool = conn
                .prepare("SELECT last_verified_at FROM known_files LIMIT 0")
                .is_ok();
            if !has_verified {
                log::info!("[macos-cache] Migrating: adding last_verified_at column");
                let _ = conn.execute_batch(
                    "ALTER TABLE known_files ADD COLUMN last_verified_at REAL DEFAULT 0;",
                );
            }

            // Wave 3.2: add parent_path column (the directory containing the entry)
            // so orphan / enumeration queries can use an indexed equality lookup
            // instead of a full-table LIKE scan. Backfill existing rows from path.
            let has_parent: bool = conn
                .prepare("SELECT parent_path FROM known_files LIMIT 0")
                .is_ok();
            if !has_parent {
                log::info!("[macos-cache] Migrating: adding parent_path column + backfilling");
                let _ = conn.execute_batch(
                    "ALTER TABLE known_files ADD COLUMN parent_path TEXT NOT NULL DEFAULT '';",
                );
                // Backfill parent_path from path in application code — simpler + safer
                // than nested substr/instr SQL.
                let rows: Vec<(i64, String)> = conn
                    .prepare("SELECT rowid, path FROM known_files")
                    .and_then(|mut stmt| {
                        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .unwrap_or_default();

                if let Ok(tx) = conn.transaction() {
                    if let Ok(mut upd) = tx
                        .prepare("UPDATE known_files SET parent_path = ?1 WHERE rowid = ?2")
                    {
                        for (rowid, path) in rows {
                            let _ = upd.execute(params![parent_of(&path), rowid]);
                        }
                    }
                    let _ = tx.commit();
                }
            }

            let _ = conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_hydrated ON known_files(is_hydrated);
                 CREATE INDEX IF NOT EXISTS idx_accessed ON known_files(last_accessed);
                 CREATE INDEX IF NOT EXISTS idx_parent_path ON known_files(parent_path);",
            );

            // Phase 3 slice 2.5: fh lives directly on known_files instead of a
            // separate nfs_handles table. Reasons:
            //   - Two tables meant two places where rows could go out of sync;
            //     repeatedly observed nfs_handles rows vanishing despite no
            //     explicit DELETE in the code (fireworks from INSERT OR
            //     REPLACE + trigger interactions).
            //   - Switching the main upserts from INSERT OR REPLACE (delete +
            //     re-insert) to INSERT ... ON CONFLICT(path) DO UPDATE (update
            //     in place) keeps the rowid/fh stable under re-enumeration.
            //   - One source of truth, simpler to reason about.
            //
            // Migration detects the old schema (path TEXT PRIMARY KEY with no
            // fh column) and rewrites the table in one transaction. Fh values
            // are re-assigned by AUTOINCREMENT — client-cached handles from
            // before the migration become STALE (one final umount/remount).
            let has_fh: bool = conn
                .prepare("SELECT fh FROM known_files LIMIT 0")
                .is_ok();
            if !has_fh {
                log::info!("[macos-cache] Migrating: known_files fh INTEGER PRIMARY KEY + drop nfs_handles");
                let tx = conn
                    .transaction()
                    .map_err(|e| format!("migration tx failed: {}", e))?;
                tx.execute_batch(
                    "DROP TRIGGER IF EXISTS nfs_handles_insert;
                     ALTER TABLE known_files RENAME TO known_files_old;
                     CREATE TABLE known_files (
                         fh INTEGER PRIMARY KEY AUTOINCREMENT,
                         path TEXT NOT NULL UNIQUE,
                         name TEXT NOT NULL,
                         is_dir INTEGER NOT NULL DEFAULT 0,
                         nas_size INTEGER NOT NULL,
                         nas_mtime REAL NOT NULL,
                         nas_created REAL NOT NULL DEFAULT 0,
                         is_hydrated INTEGER NOT NULL DEFAULT 0,
                         hydrated_size INTEGER DEFAULT 0,
                         last_accessed REAL DEFAULT 0,
                         last_verified_at REAL DEFAULT 0,
                         parent_path TEXT NOT NULL DEFAULT '',
                         chunk_bitmap BLOB DEFAULT NULL
                     );
                     -- Reserve fh=1 for the share root.
                     INSERT INTO known_files (fh, path, name, is_dir, nas_size, nas_mtime, parent_path)
                     VALUES (1, '', '', 1, 0, 0, '');
                     INSERT INTO known_files
                         (path, name, is_dir, nas_size, nas_mtime, nas_created,
                          is_hydrated, hydrated_size, last_accessed, last_verified_at,
                          parent_path)
                     SELECT path, name, is_dir, nas_size, nas_mtime, nas_created,
                            COALESCE(is_hydrated, 0),
                            COALESCE(hydrated_size, 0),
                            COALESCE(last_accessed, 0),
                            COALESCE(last_verified_at, 0),
                            COALESCE(parent_path, '')
                     FROM known_files_old
                     WHERE path != '';
                     DROP TABLE known_files_old;
                     DROP TABLE IF EXISTS nfs_handles;
                     CREATE INDEX IF NOT EXISTS idx_hydrated ON known_files(is_hydrated);
                     CREATE INDEX IF NOT EXISTS idx_accessed ON known_files(last_accessed);
                     CREATE INDEX IF NOT EXISTS idx_parent_path ON known_files(parent_path);",
                )
                .map_err(|e| format!("migration schema rewrite failed: {}", e))?;
                tx.commit()
                    .map_err(|e| format!("migration commit failed: {}", e))?;
            } else {
                // Already-new schema on a fresh DB: make sure root row exists.
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO known_files (fh, path, name, is_dir, nas_size, nas_mtime, parent_path)
                     VALUES (1, '', '', 1, 0, 0, '')",
                    [],
                );
                // And tidy up any leftover nfs_handles from a partial upgrade.
                let _ = conn.execute_batch(
                    "DROP TRIGGER IF EXISTS nfs_handles_insert;
                     DROP TABLE IF EXISTS nfs_handles;",
                );
            }

            // Block-level content cache bitmap (Phase 2).
            // One bit per 1 MiB chunk, NULL until any chunk is cached.
            // Fully-hydrated files skip the bitmap via is_hydrated=1.
            let has_bitmap: bool = conn
                .prepare("SELECT chunk_bitmap FROM known_files LIMIT 0")
                .is_ok();
            if !has_bitmap {
                log::info!("[macos-cache] Migrating: adding chunk_bitmap column");
                let _ = conn.execute_batch(
                    "ALTER TABLE known_files ADD COLUMN chunk_bitmap BLOB DEFAULT NULL;",
                );
            }
        }

        // Build the pool. Each connection applies its own per-connection PRAGMAs on init.
        // WAL mode is persistent on the DB file, so we only need synchronous=NORMAL +
        // busy_timeout here — readers won't block each other, writers serialize at SQLite.
        let manager = SqliteConnectionManager::file(&db_path).with_init(|c| {
            c.execute_batch(
                "PRAGMA synchronous=NORMAL;
                 PRAGMA busy_timeout=5000;
                 PRAGMA foreign_keys=ON;",
            )
        });
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .build(manager)
            .map_err(|e| format!("Failed to build SQLite pool: {}", e))?;

        Ok(Self {
            pool,
            nas_root,
            cache_limit,
            pending_evictions: Mutex::new(Vec::new()),
        })
    }

    /// Get a pooled connection. Short-lived; returned to pool on drop.
    #[inline]
    fn conn(&self) -> SqliteConn {
        self.pool.get().expect("SQLite pool exhausted")
    }

    /// Record a directory listing from an enumeration.
    /// Updates known_files for all entries and marks the folder as visited.
    ///
    /// Drift detection: any entry whose cached (nas_size, nas_mtime) differs
    /// from the enumerated values is queued for eviction. The extension drains
    /// this queue via `getChanges` and calls `evictItem`, dropping cached bytes
    /// so the next open triggers a fresh `fetchContents`.
    ///
    /// Performance: all DB work for a single enumeration happens in ONE
    /// transaction (not N autocommits) using prepared-cached statements.
    /// NAS I/O (folder mtime stat) happens BEFORE acquiring the connection
    /// mutex so other cache operations aren't blocked on SMB latency.
    /// pending_evictions is populated AFTER releasing the connection mutex.
    pub fn record_enumeration(&self, relative_path: &str, entries: &[crate::messages::DirEntry]) {
        // Stat the NAS folder BEFORE taking the DB mutex — this is SMB I/O.
        let folder_mtime = self.get_folder_mtime(relative_path);

        let mut drifted: Vec<String> = Vec::new();

        {
            let mut conn_guard = self.conn();
            let tx = match conn_guard.transaction() {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("[macos-cache] record_enumeration tx begin failed: {}", e);
                    return;
                }
            };

            // Upsert visited folder.
            if let Ok(mut stmt) = tx.prepare_cached(
                "INSERT OR REPLACE INTO visited_folders (path, folder_mtime) VALUES (?1, ?2)",
            ) {
                let _ = stmt.execute(params![relative_path, folder_mtime]);
            }

            // Build set of current entry paths for deletion detection.
            let mut current_paths: HashSet<String> = HashSet::new();

            // Iterate entries: drift-check existing rows, upsert metadata.
            {
                let mut stmt_select = tx
                    .prepare_cached(
                        "SELECT nas_size, nas_mtime, is_hydrated FROM known_files WHERE path = ?1",
                    )
                    .expect("prepare stmt_select");
                let mut stmt_upsert = tx
                    .prepare_cached(
                        "INSERT INTO known_files
                             (path, name, is_dir, nas_size, nas_mtime, nas_created, parent_path)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(path) DO UPDATE SET
                             name = excluded.name,
                             is_dir = excluded.is_dir,
                             nas_size = excluded.nas_size,
                             nas_mtime = excluded.nas_mtime,
                             nas_created = excluded.nas_created,
                             parent_path = excluded.parent_path",
                    )
                    .expect("prepare stmt_upsert");

                for entry in entries {
                    let entry_path = if relative_path.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{}/{}", relative_path, entry.name)
                    };
                    current_paths.insert(entry_path.clone());

                    if !entry.is_dir {
                        let existing: Option<(i64, f64, i64)> = stmt_select
                            .query_row(params![entry_path], |row| {
                                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                            })
                            .ok();
                        if let Some((cached_size, cached_mtime, is_hydrated)) = existing {
                            let drift = entry.size != cached_size as u64
                                || (entry.modified - cached_mtime).abs() > 0.001;
                            if drift && is_hydrated != 0 {
                                drifted.push(entry_path.clone());
                            }
                        }
                    }

                    let _ = stmt_upsert.execute(params![
                        entry_path,
                        entry.name,
                        entry.is_dir as i32,
                        entry.size,
                        entry.modified,
                        entry.created,
                        relative_path, // parent_path
                    ]);
                }
            }

            // Orphan detection + deletion — indexed equality lookup on
            // parent_path. The `path != parent_path` filter excludes the
            // self-reference case: the share root has path='' AND
            // parent_path='' (it's its own parent semantically), which
            // would otherwise flag root as an orphan and delete it
            // whenever we enumerate the share root.
            let orphans: Vec<String> = {
                let mut stmt_scan = tx
                    .prepare_cached(
                        "SELECT path FROM known_files
                         WHERE parent_path = ?1 AND path != parent_path",
                    )
                    .expect("prepare stmt_scan");
                stmt_scan
                    .query_map(params![relative_path], |row| row.get(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    .unwrap_or_default()
            };

            if !orphans.is_empty() {
                let mut stmt_delete = tx
                    .prepare_cached("DELETE FROM known_files WHERE path = ?1")
                    .expect("prepare stmt_delete");
                for path in &orphans {
                    if !current_paths.contains(path) {
                        let _ = stmt_delete.execute(params![path]);
                    }
                }
            }

            if let Err(e) = tx.commit() {
                log::warn!("[macos-cache] record_enumeration tx commit failed: {}", e);
            }
        }
        // Conn lock released here.

        log::debug!(
            "[macos-cache] record_enumeration parent={:?} entries={} → nfs_handles rows={}",
            relative_path,
            entries.len(),
            self.nfs_handles_count()
        );

        if !drifted.is_empty() {
            log::info!(
                "[macos-cache] Enumeration drift: {} entries under {:?} — queued for eviction",
                drifted.len(),
                relative_path
            );
            self.pending_evictions.lock().unwrap().extend(drifted);
        }
    }

    /// Get changes since a given anchor (timestamp).
    /// Walks all visited folders and diffs NAS state against DB.
    ///
    /// Two-phase: snapshot visited folders under a short-held lock, release it,
    /// then do all NAS read_dirs outside the mutex so other cache writers don't block.
    /// Reacquire the lock for the diff + write-back.
    pub fn get_changes_since(&self, _since_anchor: f64) -> ChangesResult {
        // ── Phase A: snapshot visited folders (short-held lock) ──
        let folders: Vec<String> = {
            let conn = self.conn();
            let mut list: Vec<String> = conn
                .prepare_cached("SELECT path FROM visited_folders")
                .ok()
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(0))
                        .ok()
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default();
            if !list.iter().any(|p| p.is_empty()) {
                list.push(String::new());
            }
            list
        };

        // ── Phase B: do all NAS read_dirs WITHOUT the conn lock ──
        type NasEntries = HashMap<String, (bool, u64, f64, f64)>;
        let mut snapshot: Vec<(String, NasEntries)> = Vec::with_capacity(folders.len());

        for folder_path in &folders {
            let nas_folder = if folder_path.is_empty() {
                self.nas_root.clone()
            } else {
                self.nas_root.join(folder_path)
            };

            if !nas_folder.is_dir() {
                continue;
            }

            let nas_entries: NasEntries = match std::fs::read_dir(&nas_folder) {
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

            snapshot.push((folder_path.clone(), nas_entries));
        }

        // ── Phase C: reacquire lock, diff DB against snapshot, write updates in one tx ──
        let mut updated: Vec<ChangedEntry> = Vec::new();
        let mut deleted: Vec<String> = Vec::new();

        {
            let mut conn = self.conn();
            let tx = match conn.transaction() {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("[macos-cache] get_changes_since: failed to begin tx: {}", e);
                    return ChangesResult {
                        updated,
                        deleted,
                        new_anchor: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64(),
                    };
                }
            };

            for (folder_path, nas_entries) in &snapshot {
                let db_files: HashMap<String, (String, bool, u64, f64, f64)> = tx
                    .prepare_cached(
                        "SELECT path, name, is_dir, nas_size, nas_mtime, nas_created
                         FROM known_files WHERE parent_path = ?1",
                    )
                    .ok()
                    .and_then(|mut stmt| {
                        stmt.query_map(params![folder_path], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                (
                                    row.get::<_, String>(1)?,
                                    row.get::<_, i32>(2)? != 0,
                                    row.get::<_, u64>(3)?,
                                    row.get::<_, f64>(4)?,
                                    row.get::<_, f64>(5)?,
                                ),
                            ))
                        })
                        .ok()
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                    })
                    .unwrap_or_default();

                let db_names: HashSet<String> = db_files.values().map(|(name, _, _, _, _)| name.clone()).collect();
                let nas_names: HashSet<String> = nas_entries.keys().cloned().collect();

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

                for name in db_names.difference(&nas_names) {
                    let rel_path = if folder_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", folder_path, name)
                    };
                    deleted.push(rel_path);
                }

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
            }

            // Apply updates + deletes inside the same transaction.
            {
                let mut upsert = tx
                    .prepare_cached(
                        "INSERT INTO known_files
                             (path, name, is_dir, nas_size, nas_mtime, nas_created, parent_path)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                         ON CONFLICT(path) DO UPDATE SET
                             name = excluded.name,
                             is_dir = excluded.is_dir,
                             nas_size = excluded.nas_size,
                             nas_mtime = excluded.nas_mtime,
                             nas_created = excluded.nas_created,
                             parent_path = excluded.parent_path",
                    )
                    .ok();
                if let Some(stmt) = upsert.as_mut() {
                    for entry in &updated {
                        let _ = stmt.execute(params![
                            entry.relative_path,
                            entry.name,
                            entry.is_dir as i32,
                            entry.size,
                            entry.modified,
                            entry.created,
                            parent_of(&entry.relative_path),
                        ]);
                    }
                }
            }
            {
                let mut del = tx.prepare_cached("DELETE FROM known_files WHERE path = ?1").ok();
                if let Some(stmt) = del.as_mut() {
                    for path in &deleted {
                        let _ = stmt.execute(params![path]);
                    }
                }
            }

            if let Err(e) = tx.commit() {
                log::warn!("[macos-cache] get_changes_since: commit failed: {}", e);
            }
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
        let conn = self.conn();
        let _ = conn
            .prepare_cached("DELETE FROM known_files WHERE path = ?1")
            .map(|mut stmt| stmt.execute(params![relative_path]));
    }

    /// Add or update a known file in the DB (after write).
    pub fn record_known_file(&self, relative_path: &str, entry: &crate::messages::DirEntry) {
        let conn = self.conn();
        let parent = parent_of(relative_path).to_string();
        let _ = conn
            .prepare_cached(
                "INSERT INTO known_files
                     (path, name, is_dir, nas_size, nas_mtime, nas_created, parent_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET
                     name = excluded.name,
                     is_dir = excluded.is_dir,
                     nas_size = excluded.nas_size,
                     nas_mtime = excluded.nas_mtime,
                     nas_created = excluded.nas_created,
                     parent_path = excluded.parent_path"
            )
            .map(|mut stmt| {
                stmt.execute(params![
                    relative_path, entry.name, entry.is_dir as i32,
                    entry.size, entry.modified, entry.created, parent,
                ])
            });
    }

    // ── Hydration tracking + LRU eviction ──

    /// Record that a file was downloaded (materialized). Triggers eviction check.
    pub fn record_hydration(&self, relative_path: &str, size: u64) {
        if size == 0 {
            return;
        }
        let now = unix_now_f64();

        {
            let conn = self.conn();
            let _ = conn
                .prepare_cached(
                    "UPDATE known_files SET is_hydrated=1, hydrated_size=?1, last_accessed=?2 WHERE path=?3",
                )
                .map(|mut stmt| stmt.execute(params![size as i64, now, relative_path]));
        }

        self.evict_if_over_budget();
    }

    /// Update last_accessed time (called on each file read).
    pub fn touch(&self, relative_path: &str) {
        let now = unix_now_f64();
        let conn = self.conn();
        let _ = conn
            .prepare_cached("UPDATE known_files SET last_accessed=?1 WHERE path=?2")
            .map(|mut stmt| stmt.execute(params![now, relative_path]));
    }

    /// Stamp last_verified_at = now. Called after a successful NAS stat confirms
    /// cached metadata matches reality.
    pub fn record_verification(&self, relative_path: &str) {
        let now = unix_now_f64();
        let conn = self.conn();
        let _ = conn
            .prepare_cached("UPDATE known_files SET last_verified_at=?1 WHERE path=?2")
            .map(|mut stmt| stmt.execute(params![now, relative_path]));
    }

    /// Update NAS metadata to fresh stat values and stamp last_verified_at.
    /// Preserves hydration state and last_accessed via targeted UPDATE.
    pub fn update_nas_metadata(&self, relative_path: &str, nas_size: u64, nas_mtime: f64) {
        let now = unix_now_f64();
        let conn = self.conn();
        let _ = conn
            .prepare_cached(
                "UPDATE known_files SET nas_size=?1, nas_mtime=?2, last_verified_at=?3 WHERE path=?4",
            )
            .map(|mut stmt| stmt.execute(params![nas_size as i64, nas_mtime, now, relative_path]));
    }

    /// Returns cached `(nas_size, nas_mtime)` if the entry warrants re-verification,
    /// or `None` if we should skip the NAS stat.
    pub fn needs_verification(&self, relative_path: &str, ttl_secs: f64) -> Option<(u64, f64)> {
        let now = unix_now_f64();
        let conn = self.conn();
        let row: Option<(i64, f64, f64)> = conn
            .prepare_cached(
                "SELECT nas_size, nas_mtime, last_verified_at FROM known_files WHERE path=?1",
            )
            .ok()
            .and_then(|mut stmt| {
                stmt.query_row(params![relative_path], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?))
                })
                .ok()
            });
        row.and_then(|(size, mtime, verified)| {
            if now - verified < ttl_secs {
                None
            } else {
                Some((size as u64, mtime))
            }
        })
    }

    /// Compare provided NAS metadata against the cached row.
    pub fn compare_nas_metadata(
        &self,
        relative_path: &str,
        nas_size: u64,
        nas_mtime: f64,
    ) -> Option<bool> {
        let conn = self.conn();
        let cached: Option<(i64, f64)> = conn
            .prepare_cached("SELECT nas_size, nas_mtime FROM known_files WHERE path=?1")
            .ok()
            .and_then(|mut stmt| {
                stmt.query_row(params![relative_path], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
                })
                .ok()
            });
        cached.map(|(cached_size, cached_mtime)| {
            nas_size != cached_size as u64 || (nas_mtime - cached_mtime).abs() > 0.001
        })
    }

    /// Check if a path is tracked in known_files (has any row).
    pub fn is_known(&self, relative_path: &str) -> bool {
        let conn = self.conn();
        conn.prepare_cached("SELECT 1 FROM known_files WHERE path=?1")
            .ok()
            .and_then(|mut stmt| stmt.query_row(params![relative_path], |_| Ok(true)).ok())
            .unwrap_or(false)
    }

    /// Total bytes of hydrated (locally cached) files.
    pub fn total_cached_bytes(&self) -> u64 {
        let conn = self.conn();
        conn.prepare_cached(
            "SELECT COALESCE(SUM(hydrated_size), 0) FROM known_files WHERE is_hydrated=1",
        )
        .ok()
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, i64>(0)).ok())
        .unwrap_or(0) as u64
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
        let conn = self.conn();
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

    // ── Content cache (Phase 2) ──

    /// Read the current chunk bitmap for a file. Returns an empty Vec for
    /// uncached files (no chunks yet). Cheap — single indexed read.
    pub fn get_chunk_bitmap(&self, path: &str) -> Vec<u8> {
        let conn = self.conn();
        let Ok(mut stmt) =
            conn.prepare_cached("SELECT chunk_bitmap FROM known_files WHERE path = ?1")
        else {
            return Vec::new();
        };
        stmt.query_row(params![path], |row| {
            Ok(row.get::<_, Option<Vec<u8>>>(0)?.unwrap_or_default())
        })
        .unwrap_or_default()
    }

    /// Persist an updated chunk bitmap for a file. Also advances
    /// `last_accessed` (the read path is the only caller right now, so this
    /// keeps eviction LRU honest).
    pub fn update_chunk_bitmap(&self, path: &str, bitmap: &[u8]) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let conn = self.conn();
        let _ = conn
            .prepare_cached(
                "UPDATE known_files
                 SET chunk_bitmap = ?1, last_accessed = ?2
                 WHERE path = ?3",
            )
            .map(|mut stmt| stmt.execute(params![bitmap, now, path]));
    }

    /// Insert (or update) a single entry in `known_files`. On first insert
    /// AUTOINCREMENT assigns an `fh`; on conflict we UPDATE in place so the
    /// `fh` stays stable (critical for NFS — client-cached handles for this
    /// path keep working). Used by NFS CREATE / MKDIR to register a
    /// freshly-created file or folder without running `record_enumeration`'s
    /// orphan-scan against the parent (which would erase sibling rows).
    pub fn record_new_entry(
        &self,
        relative_path: &str,
        name: &str,
        is_dir: bool,
        size: u64,
        mtime: f64,
        created: f64,
    ) {
        let parent = parent_of(relative_path).to_string();
        let conn = self.conn();
        let _ = conn
            .prepare_cached(
                "INSERT INTO known_files
                     (path, name, is_dir, nas_size, nas_mtime, nas_created, parent_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET
                     name = excluded.name,
                     is_dir = excluded.is_dir,
                     nas_size = excluded.nas_size,
                     nas_mtime = excluded.nas_mtime,
                     nas_created = excluded.nas_created,
                     parent_path = excluded.parent_path",
            )
            .map(|mut stmt| {
                stmt.execute(params![
                    relative_path,
                    name,
                    is_dir as i32,
                    size as i64,
                    mtime,
                    created,
                    parent,
                ])
            });
    }

    /// Reflect an NFS RENAME in the cache. Preserves the source row's `fh`
    /// so NFS clients' cached handles keep resolving after the rename —
    /// critical for editor "save as" patterns that rename over the file.
    ///
    /// Caller must have performed `std::fs::rename(from_abs, to_abs)` first.
    /// This function does the DB-side bookkeeping:
    ///   1. Delete any stale rows at the target (the disk rename already
    ///      clobbered them).
    ///   2. UPDATE the source row's path/parent_path/name in place so `fh`
    ///      survives.
    ///   3. Fix up every descendant row (when renaming a directory) —
    ///      their path prefix changes, their `fh` stays.
    pub fn rename_path(&self, from: &str, to: &str) -> Result<(), String> {
        let mut conn = self.conn();
        let tx = conn
            .transaction()
            .map_err(|e| format!("rename tx begin: {}", e))?;

        // Clear anything under the target path. The disk rename moved the
        // actual files to `to`, so whatever was previously there is gone.
        tx.execute(
            "DELETE FROM known_files WHERE path = ?1 OR path LIKE ?1 || '/%'",
            params![to],
        )
        .map_err(|e| format!("rename clear target: {}", e))?;

        let new_parent = parent_of(to).to_string();
        let new_name = to.rsplit('/').next().unwrap_or(to).to_string();
        tx.execute(
            "UPDATE known_files SET path = ?1, parent_path = ?2, name = ?3
             WHERE path = ?4",
            params![to, new_parent, new_name, from],
        )
        .map_err(|e| format!("rename update source: {}", e))?;

        // Descendant fixup for directory renames. Collect first (borrow
        // ends with the prepare), then apply UPDATEs.
        let from_prefix = format!("{}/", from);
        let to_prefix = format!("{}/", to);
        let descendants: Vec<(i64, String)> = {
            let mut stmt = tx
                .prepare("SELECT fh, path FROM known_files WHERE path LIKE ?1 || '%'")
                .map_err(|e| format!("rename prep desc: {}", e))?;
            let rows = stmt
                .query_map(params![from_prefix], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("rename query desc: {}", e))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r.map_err(|e| format!("rename row desc: {}", e))?);
            }
            out
        };

        for (fh, old_desc_path) in descendants {
            let new_desc_path =
                format!("{}{}", to_prefix, &old_desc_path[from_prefix.len()..]);
            let new_desc_parent = parent_of(&new_desc_path).to_string();
            tx.execute(
                "UPDATE known_files SET path = ?1, parent_path = ?2 WHERE fh = ?3",
                params![new_desc_path, new_desc_parent, fh],
            )
            .map_err(|e| format!("rename update desc: {}", e))?;
        }

        tx.commit()
            .map_err(|e| format!("rename commit: {}", e))?;
        Ok(())
    }

    /// Remove a path from the cache. Deletes its row from `known_files`
    /// (its fh becomes permanently `NFS3ERR_STALE` — AUTOINCREMENT never
    /// reuses it) and deletes the on-disk cache file if present. Used by
    /// NFS REMOVE / RMDIR after the NAS-side delete succeeds.
    pub fn forget_path(&self, path: &str, fh: u64) {
        {
            let conn = self.conn();
            let _ = conn
                .prepare_cached("DELETE FROM known_files WHERE path = ?1")
                .map(|mut stmt| stmt.execute(params![path]));
        }
        let cache_path = self.cache_file_path(fh);
        let _ = std::fs::remove_file(&cache_path);
    }

    /// Invalidate the content cache for a file — clears `is_hydrated`, drops
    /// the chunk bitmap, and deletes the on-disk cache file. Metadata rows
    /// (size, mtime, fh) are left untouched; callers should follow up with
    /// `update_nas_metadata` after the authoritative NAS state is known.
    /// Called on every write path so subsequent reads re-hydrate from SMB.
    pub fn invalidate_cache(&self, path: &str, fh: u64) {
        {
            let conn = self.conn();
            let _ = conn
                .prepare_cached(
                    "UPDATE known_files
                     SET is_hydrated = 0, hydrated_size = 0, chunk_bitmap = NULL
                     WHERE path = ?1",
                )
                .map(|mut stmt| stmt.execute(params![path]));
        }
        let cache_path = self.cache_file_path(fh);
        let _ = std::fs::remove_file(&cache_path);
    }

    /// Mark a file as fully hydrated (all chunks cached). Nulls the bitmap
    /// since `is_hydrated=1` is the fast-path shortcut.
    pub fn mark_fully_hydrated(&self, path: &str, size: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let conn = self.conn();
        let _ = conn
            .prepare_cached(
                "UPDATE known_files
                 SET is_hydrated = 1,
                     hydrated_size = ?1,
                     chunk_bitmap = NULL,
                     last_accessed = ?2
                 WHERE path = ?3",
            )
            .map(|mut stmt| stmt.execute(params![size as i64, now, path]));
    }

    /// Cache-file path for a given NFS handle. Directory is created on first
    /// call; callers can assume the parent exists.
    pub fn cache_file_path(&self, fh: u64) -> PathBuf {
        let dir = Self::cache_file_dir();
        let _ = std::fs::create_dir_all(&dir);
        dir.join(format!("{:016x}", fh))
    }

    /// Root of the content-cache filesystem layout (shared by all domains).
    fn cache_file_dir() -> PathBuf {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".local/share/ufb/cache/by_handle")
        } else {
            PathBuf::from("/tmp/ufb-cache/by_handle")
        }
    }

    // ── NFS handle / metadata serving (Phase 1) ──

    /// Look up the NFS file handle for a relative path. `""` is the root.
    /// Returns `None` if the path has never been indexed.
    pub fn fh_for_path(&self, path: &str) -> Option<u64> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached("SELECT fh FROM known_files WHERE path = ?1").ok()?;
        stmt.query_row(params![path], |row| row.get::<_, i64>(0))
            .ok()
            .map(|v| v as u64)
    }

    /// Diagnostic: total number of known_files rows (one per cached path).
    pub fn nfs_handles_count(&self) -> i64 {
        let conn = self.conn();
        conn.prepare_cached("SELECT COUNT(*) FROM known_files")
            .ok()
            .and_then(|mut stmt| stmt.query_row([], |row| row.get(0)).ok())
            .unwrap_or(-1)
    }

    /// Reverse lookup: path for an NFS file handle.
    pub fn path_for_fh(&self, fh: u64) -> Option<String> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached("SELECT path FROM known_files WHERE fh = ?1").ok()?;
        stmt.query_row(params![fh as i64], |row| row.get::<_, String>(0)).ok()
    }

    /// Ensure a handle exists for `path`, returning its fh. On first insert
    /// AUTOINCREMENT assigns an `fh`; on conflict the row is left untouched.
    /// Used for the share root at server startup (so fh=1 is always reserved).
    pub fn ensure_fh(&self, path: &str) -> Option<u64> {
        {
            let conn = self.conn();
            let parent = parent_of(path).to_string();
            let mut stmt = conn
                .prepare_cached(
                    "INSERT INTO known_files
                         (path, name, is_dir, nas_size, nas_mtime, parent_path)
                     VALUES (?1, ?2, 1, 0, 0, ?3)
                     ON CONFLICT(path) DO NOTHING",
                )
                .ok()?;
            let _ = stmt.execute(params![path, path, parent]);
        }
        self.fh_for_path(path)
    }

    /// Cached metadata for a given path — fields an NFS GETATTR needs. Returns
    /// `None` if the path isn't in `known_files` (cold — caller must populate).
    pub fn cached_attr(&self, path: &str) -> Option<CachedAttr> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare_cached(
                "SELECT is_dir, nas_size, nas_mtime, nas_created, is_hydrated, hydrated_size
                 FROM known_files WHERE path = ?1",
            )
            .ok()?;
        stmt.query_row(params![path], |row| {
            Ok(CachedAttr {
                is_dir: row.get::<_, i32>(0)? != 0,
                size: row.get::<_, i64>(1)? as u64,
                mtime: row.get::<_, f64>(2)?,
                created: row.get::<_, f64>(3)?,
                is_hydrated: row.get::<_, i32>(4)? != 0,
                hydrated_size: row.get::<_, i64>(5)? as u64,
            })
        })
        .ok()
    }

    /// Direct children of a folder — (fh, name, CachedAttr) tuples, joined
    /// across `known_files` and `nfs_handles`. Returns an empty Vec for cold
    /// folders (caller falls back to live enumeration then recalls us).
    pub fn cached_children(&self, parent_path: &str) -> Vec<(u64, String, CachedAttr)> {
        let conn = self.conn();
        conn.prepare_cached(
            "SELECT fh, name, is_dir, nas_size, nas_mtime, nas_created,
                    is_hydrated, hydrated_size
             FROM known_files
             WHERE parent_path = ?1 AND path != ''",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map(params![parent_path], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, String>(1)?,
                    CachedAttr {
                        is_dir: row.get::<_, i32>(2)? != 0,
                        size: row.get::<_, i64>(3)? as u64,
                        mtime: row.get::<_, f64>(4)?,
                        created: row.get::<_, f64>(5)?,
                        is_hydrated: row.get::<_, i32>(6)? != 0,
                        hydrated_size: row.get::<_, i64>(7)? as u64,
                    },
                ))
            })
            .ok()
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    }

    /// Has this folder already been enumerated? Used to distinguish
    /// cold-path (fall through to NAS) vs empty-but-warm folders.
    pub fn folder_is_enumerated(&self, path: &str) -> bool {
        let conn = self.conn();
        conn.prepare_cached("SELECT 1 FROM visited_folders WHERE path = ?1")
            .ok()
            .and_then(|mut stmt| stmt.query_row(params![path], |_| Ok(true)).ok())
            .unwrap_or(false)
    }

    /// Return relative paths of currently-hydrated files. Cheap DB-only query,
    /// no NAS I/O. Used by the extension's clear-cache flow to drive per-item
    /// `evictItem` calls without a `list_dir` round-trip.
    pub fn hydrated_paths(&self) -> Vec<String> {
        let conn = self.conn();
        conn.prepare_cached(
            "SELECT path FROM known_files WHERE is_hydrated=1 AND is_dir=0",
        )
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(0))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    }

    /// Mark ALL hydrated files for eviction. Used by "Clear Cache" button.
    pub fn clear_all_hydrated(&self) -> (u32, u64) {
        let conn = self.conn();
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

    /// If this path is currently materialized (hydrated), queue it for eviction.
    /// No-op otherwise. Used by stat-drift detection so the next FileProvider
    /// `getChanges` call will tell the extension to drop cached bytes.
    pub fn queue_eviction_if_hydrated(&self, relative_path: &str) {
        let hydrated: bool = {
            let conn = self.conn();
            conn.prepare_cached("SELECT is_hydrated FROM known_files WHERE path = ?1")
                .ok()
                .and_then(|mut stmt| {
                    stmt.query_row(params![relative_path], |row| {
                        let v: i64 = row.get(0)?;
                        Ok(v != 0)
                    })
                    .ok()
                })
                .unwrap_or(false)
        };
        if hydrated {
            self.pending_evictions
                .lock()
                .unwrap()
                .push(relative_path.to_string());
        }
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

/// Result of a stat-and-refresh call against the NAS.
#[derive(Debug)]
pub enum StatResult {
    /// Within TTL — no stat was performed. Caller can serve cached data.
    Skipped,
    /// NAS was stat'd; matched cached metadata. `last_verified_at` was refreshed.
    Fresh { size: u64, mtime: f64 },
    /// NAS was stat'd; drifted from cache. DB updated with new values.
    /// Caller may want to invalidate OS-side cached content.
    Drifted { size: u64, mtime: f64 },
    /// Path unknown to cache — caller should let the normal read path populate
    /// a baseline. Fresh stat values provided for convenience.
    Unknown { size: u64, mtime: f64 },
    /// NAS stat failed. Caller should log + fall through — freshness is
    /// an optimization hint, never a blocker.
    Error(std::io::Error),
}

/// Lazy freshness primitive: TTL-gated NAS stat against the cache.
///
/// If the entry was verified within `ttl_secs`, returns `Skipped` with no NAS
/// traffic. Otherwise stats `nas_path` and either stamps verification (match)
/// or updates cached metadata (drift). On stat failure returns `Error`; callers
/// should log and fall through to their normal path.
pub fn stat_and_refresh(
    cache: &MacosCache,
    relative_path: &str,
    nas_path: &Path,
    ttl_secs: f64,
) -> StatResult {
    let (cached_size, cached_mtime) = match cache.needs_verification(relative_path, ttl_secs) {
        Some(v) => v,
        None => {
            // Either within TTL or unknown. Disambiguate.
            if cache.is_known(relative_path) {
                return StatResult::Skipped;
            }
            return match std::fs::metadata(nas_path) {
                Ok(m) => StatResult::Unknown {
                    size: m.len(),
                    mtime: mtime_secs_f64(&m),
                },
                Err(e) => StatResult::Error(e),
            };
        }
    };

    let meta = match std::fs::metadata(nas_path) {
        Ok(m) => m,
        Err(e) => return StatResult::Error(e),
    };

    let nas_size = meta.len();
    let nas_mtime = mtime_secs_f64(&meta);

    // f64 equality is fine here — both values come from the same SystemTime
    // → Duration → f64 path. If a future platform introduces sub-nanosecond
    // jitter we'll want a tolerance, but mtime resolution on SMB is second-grained.
    if nas_size == cached_size && (nas_mtime - cached_mtime).abs() < 0.001 {
        cache.record_verification(relative_path);
        StatResult::Fresh {
            size: nas_size,
            mtime: nas_mtime,
        }
    } else {
        cache.update_nas_metadata(relative_path, nas_size, nas_mtime);
        StatResult::Drifted {
            size: nas_size,
            mtime: nas_mtime,
        }
    }
}

fn unix_now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn mtime_secs_f64(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
