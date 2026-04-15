use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, Result as SqlResult};
use std::path::PathBuf;

const POOL_SIZE: u32 = 6;

/// Manages the shared SQLite database via an r2d2 connection pool (WAL mode).
/// All managers access the DB through `with_conn`, which hands out a pooled
/// connection for the duration of the closure. Readers don't block each other;
/// writers still serialize at the SQLite level (WAL one-writer).
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
}

impl Database {
    /// Open (or create) the UFB database at the given path.
    pub fn open(path: &PathBuf) -> SqlResult<Self> {
        // One-time setup on a serial connection: enable WAL (persistent on disk)
        // + set pragmas the pool's init closure can't set (journal_mode).
        {
            let conn = Connection::open(path)?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;",
            )?;
        }

        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            c.execute_batch(
                "PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=5000;
                 PRAGMA synchronous=NORMAL;",
            )
        });
        let pool = Pool::builder()
            .max_size(POOL_SIZE)
            .build(manager)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        Ok(Self { pool })
    }

    /// Open an in-memory database (for testing).
    /// Note: in-memory DBs are per-connection, so the pool shares via a named
    /// shared-cache URI so all pooled connections see the same data.
    #[allow(dead_code)]
    pub fn open_in_memory() -> SqlResult<Self> {
        let manager = SqliteConnectionManager::memory().with_init(|c| {
            c.execute_batch("PRAGMA foreign_keys=ON;")
        });
        let pool = Pool::builder()
            .max_size(1) // single-conn for in-memory to keep schema shared
            .build(manager)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        Ok(Self { pool })
    }

    /// Execute a closure with a reference to a pooled connection.
    ///
    /// The connection is returned to the pool automatically when the closure
    /// returns. Keep the closure body short; long-running work should not hold
    /// a connection longer than necessary.
    pub fn with_conn<F, T>(&self, f: F) -> SqlResult<T>
    where
        F: FnOnce(&Connection) -> SqlResult<T>,
    {
        let conn = self.pool.get().map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(e))
        })?;
        f(&conn)
    }

    /// Execute a closure with a MUTABLE reference to a pooled connection.
    /// Required for `conn.transaction()` which needs `&mut Connection`.
    #[allow(dead_code)]
    pub fn with_conn_mut<F, T>(&self, f: F) -> SqlResult<T>
    where
        F: FnOnce(&mut Connection) -> SqlResult<T>,
    {
        let mut conn = self.pool.get().map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(e))
        })?;
        f(&mut conn)
    }

    /// Safely add a column if it doesn't already exist.
    fn add_column_if_missing(conn: &Connection, table: &str, column: &str, col_type: &str) {
        let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type);
        match conn.execute_batch(&sql) {
            Ok(()) => log::info!("Added column {}.{}", table, column),
            Err(_) => {} // Column already exists — expected
        }
    }

    /// Run all table-creation migrations.
    pub fn run_migrations(&self) -> SqlResult<()> {
        self.with_conn(|conn| {
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS subscriptions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_path TEXT NOT NULL UNIQUE,
                    job_name TEXT NOT NULL,
                    is_active INTEGER NOT NULL DEFAULT 1,
                    subscribed_time INTEGER NOT NULL,
                    last_sync_time INTEGER,
                    sync_status TEXT NOT NULL DEFAULT 'Pending',
                    shot_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_subs_active
                    ON subscriptions(is_active);

                CREATE TABLE IF NOT EXISTS item_metadata (
                    item_path TEXT NOT NULL,
                    job_path TEXT NOT NULL,
                    folder_name TEXT NOT NULL,
                    metadata_json TEXT NOT NULL DEFAULT '{}',
                    is_tracked INTEGER NOT NULL DEFAULT 0,
                    created_time INTEGER,
                    modified_time INTEGER,
                    device_id TEXT,
                    PRIMARY KEY (item_path)
                );
                CREATE INDEX IF NOT EXISTS idx_item_metadata_job
                    ON item_metadata(job_path);
                CREATE INDEX IF NOT EXISTS idx_item_metadata_tracked
                    ON item_metadata(is_tracked);
                CREATE INDEX IF NOT EXISTS idx_item_metadata_job_tracked
                    ON item_metadata(job_path, is_tracked);

                CREATE TABLE IF NOT EXISTS column_definitions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_path TEXT NOT NULL,
                    folder_name TEXT NOT NULL,
                    column_name TEXT NOT NULL,
                    column_type TEXT NOT NULL DEFAULT 'text',
                    column_order INTEGER NOT NULL DEFAULT 0,
                    column_width REAL NOT NULL DEFAULT 120.0,
                    is_visible INTEGER NOT NULL DEFAULT 1,
                    default_value TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_column_defs_job_folder
                    ON column_definitions(job_path, folder_name);

                CREATE TABLE IF NOT EXISTS column_options (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    column_id INTEGER NOT NULL,
                    option_name TEXT NOT NULL,
                    option_color TEXT,
                    FOREIGN KEY (column_id) REFERENCES column_definitions(id) ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS bookmarks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    path TEXT NOT NULL UNIQUE,
                    display_name TEXT NOT NULL,
                    created_time INTEGER NOT NULL,
                    is_project_folder INTEGER NOT NULL DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS column_presets (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    preset_name TEXT NOT NULL UNIQUE,
                    columns_json TEXT NOT NULL,
                    created_time INTEGER NOT NULL,
                    modified_time INTEGER NOT NULL
                );

                DROP TABLE IF EXISTS thumbnail_cache;
                CREATE TABLE thumbnail_cache (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    file_path_hash TEXT NOT NULL UNIQUE,
                    file_path TEXT NOT NULL,
                    file_size INTEGER NOT NULL,
                    file_mtime INTEGER NOT NULL,
                    thumbnail_width INTEGER NOT NULL,
                    thumbnail_height INTEGER NOT NULL,
                    thumbnail_data BLOB NOT NULL,
                    data_size INTEGER NOT NULL,
                    extracted_time INTEGER NOT NULL,
                    last_access_time INTEGER NOT NULL,
                    access_count INTEGER DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_thumbnail_hash
                    ON thumbnail_cache(file_path_hash);
                CREATE INDEX IF NOT EXISTS idx_thumbnail_lookup
                    ON thumbnail_cache(file_path_hash, file_size, file_mtime);
                ",
            )?;

            // ── Schema migrations for C++ app DB compatibility ──
            // The C++ app's item_metadata has no device_id column
            Self::add_column_if_missing(conn, "item_metadata", "device_id", "TEXT");

            Ok(())
        })
    }
}
