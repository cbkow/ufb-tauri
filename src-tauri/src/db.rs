use rusqlite::{Connection, Result as SqlResult};
use std::path::PathBuf;
use std::sync::Mutex;

/// Manages the shared SQLite database connection (WAL mode).
/// All managers access the DB through this shared connection.
pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    /// Open (or create) the UFB database at the given path.
    pub fn open(path: &PathBuf) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (for testing).
    #[allow(dead_code)]
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Execute a closure with a reference to the locked connection.
    pub fn with_conn<F, T>(&self, f: F) -> SqlResult<T>
    where
        F: FnOnce(&Connection) -> SqlResult<T>,
    {
        let conn = self.conn.lock().expect("database mutex poisoned");
        f(&conn)
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
