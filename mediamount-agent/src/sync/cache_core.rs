/// Shared cache helpers used by both macOS (`macos_cache`) and Windows
/// (`windows_cache`) sync backends.
///
/// Contains: constants, chunk-bitmap bit operations, path helpers,
/// SQLite type aliases, integrity checking, and the `CachedAttr` type
/// returned by cache-serving accessors.

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

pub const EVICTION_TARGET_PERCENT: f64 = 0.8;
pub const POOL_SIZE: u32 = 6;

/// Content cache chunk size (1 MiB). Matches the NFS `rsize` mount option
/// on macOS and the ProjFS read-request granularity on Windows so each
/// platform's read callback maps cleanly to one chunk.
pub const CHUNK_SIZE: u64 = 1024 * 1024;

pub type SqlitePool = Pool<SqliteConnectionManager>;
pub type SqliteConn = PooledConnection<SqliteConnectionManager>;

// ── Chunk-bitmap bit operations ──
//
// Bits are packed LSB-first within each byte: chunk `i` lives in byte
// `i / 8`, bit `i % 8`.

#[inline]
pub fn num_chunks(size: u64) -> u64 {
    (size + CHUNK_SIZE - 1) / CHUNK_SIZE
}

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

#[inline]
pub fn bitmap_is_complete(bitmap: &[u8], total_chunks: u64) -> bool {
    if total_chunks == 0 {
        return true;
    }
    let full_bytes = (total_chunks / 8) as usize;
    for i in 0..full_bytes {
        if bitmap.get(i).copied().unwrap_or(0) != 0xFF {
            return false;
        }
    }
    let remainder = total_chunks % 8;
    if remainder > 0 {
        let mask = (1u8 << remainder) - 1;
        if bitmap.get(full_bytes).copied().unwrap_or(0) & mask != mask {
            return false;
        }
    }
    true
}

// ── Path helpers ──

/// Parent directory of a forward-slash-separated relative path.
/// `"a/b/c.txt"` → `"a/b"`. `"foo.txt"` → `""`.
#[inline]
pub fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

// ── Shared cache attribute type ──

/// Subset of `known_files` fields needed by the VFS provider layer
/// (NFS `fattr3` on macOS, ProjFS placeholder info on Windows).
#[derive(Debug, Clone)]
pub struct CachedAttr {
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since Unix epoch (NAS mtime). Stored as `f64` for
    /// sub-second precision on platforms that support it.
    pub mtime: f64,
    /// Seconds since Unix epoch (NAS ctime / birth time).
    pub created: f64,
    pub is_hydrated: bool,
    pub hydrated_size: u64,
}

// ── SQLite helpers ──

/// Per-connection PRAGMAs applied on every pool checkout.
pub const PER_CONN_PRAGMAS: &str =
    "PRAGMA synchronous=NORMAL;\n\
     PRAGMA busy_timeout=5000;\n\
     PRAGMA foreign_keys=ON;";

/// One-time pragmas applied on the serial setup connection before the
/// pool opens (WAL is persistent on the DB file).
pub const INIT_PRAGMAS: &str =
    "PRAGMA journal_mode=WAL;\n\
     PRAGMA synchronous=NORMAL;";

/// `metadata` table DDL — key-value store for global state. Shared
/// across both platforms.
pub const METADATA_DDL: &str =
    "CREATE TABLE IF NOT EXISTS metadata (\n\
         key TEXT PRIMARY KEY,\n\
         value TEXT NOT NULL\n\
     );";

/// Check SQLite integrity. Returns `true` if the DB is healthy.
pub fn check_integrity(conn: &Connection) -> bool {
    conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .map(|result| result == "ok")
        .unwrap_or(false)
}

/// Current wall-clock time as seconds since Unix epoch.
pub fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Current wall-clock time as fractional seconds since Unix epoch.
pub fn unix_now_f64() -> f64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    d.as_secs() as f64 + d.subsec_nanos() as f64 / 1_000_000_000.0
}

/// Build a connection pool with shared per-connection pragmas.
pub fn build_pool(db_path: &std::path::Path) -> Result<SqlitePool, String> {
    let manager = SqliteConnectionManager::file(db_path).with_init(|c| {
        c.execute_batch(PER_CONN_PRAGMAS)
    });
    Pool::builder()
        .max_size(POOL_SIZE)
        .build(manager)
        .map_err(|e| format!("Failed to build SQLite pool: {}", e))
}
