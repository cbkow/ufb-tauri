//! Conflict-detection helpers shared between the FileProvider fileops
//! write path (pre-NFS, retiring in Phase 5) and the NFS loopback write
//! path. On a detected concurrent edit, we rename our about-to-be-written
//! file to a sidecar so both versions survive:
//!
//!   `{stem}.conflict-{host}-{YYYYMMDD-HHMMSS}{.ext}`
//!
//! Callers also emit a `ConflictDetected` message to UFB so the UI can
//! surface the sidecar to the user.

use std::path::{Path, PathBuf};

/// Construct the conflict sidecar path for a collided write.
pub fn make_conflict_path(dest_path: &Path) -> PathBuf {
    let stem = dest_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let ext = dest_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let host = hostname_short();
    let ts = format_timestamp_compact();
    let name = format!("{}.conflict-{}-{}{}", stem, host, ts, ext);
    dest_path.with_file_name(name)
}

pub fn hostname_short() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".into())
        })
        .split('.')
        .next()
        .unwrap_or("unknown")
        .to_string()
}

pub fn format_timestamp_compact() -> String {
    // YYYYMMDD-HHMMSS in UTC, formatted manually to avoid pulling in chrono.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, mo, d, h, mi, s) = epoch_to_ymdhms(secs);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", y, mo, d, h, mi, s)
}

/// Convert a unix timestamp to civil date components (UTC).
/// Uses Howard Hinnant's days_from_civil algorithm.
fn epoch_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let h = (sod / 3_600) as u32;
    let mi = ((sod % 3_600) / 60) as u32;
    let s = (sod % 60) as u32;

    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y_civil = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (y_civil + if mo <= 2 { 1 } else { 0 }) as u32;
    (y, mo, d, h, mi, s)
}
