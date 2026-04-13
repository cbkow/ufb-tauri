/// Placeholder lifecycle helpers shared by the watcher and the CF filter.
///
/// CF's `CfUpdatePlaceholders` is unreliable for size changes (especially
/// 0-byte → non-zero on Synology), so the codebase standardizes on a
/// delete + recreate pattern instead.

use cloud_filter::{metadata::Metadata, placeholder_file::PlaceholderFile};
use std::fs;
use std::path::{Path, PathBuf};

use super::cache::CacheIndex;

/// Rebuild a client-side placeholder from current NAS state.
///
/// Deletes the local file (throwing away any hydrated content) and creates a
/// fresh placeholder with NAS's current size + mtime. Works whether or not
/// the file was previously hydrated.
///
/// No-op if the client file is missing, is a directory, or if NAS reports
/// size 0 (the 0-byte unreliability case on Synology — let deferred polling
/// pick it up instead).
pub(crate) fn refresh_placeholder(
    nas_path: &Path,
    client_dir: &Path,
    file_name: &str,
    display: &str,
    cache: &CacheIndex,
) {
    let client_path = client_dir.join(file_name);
    if !client_path.exists() || client_path.is_dir() {
        return;
    }

    let nas_meta = match fs::metadata(nas_path) {
        Ok(m) => m,
        Err(_) => return,
    };

    let nas_size = nas_meta.len();
    if nas_size == 0 {
        return;
    }

    // Delete and recreate — CfUpdatePlaceholder doesn't reliably update
    // size on 0-byte placeholders via Win32 handles.
    let _ = fs::remove_file(&client_path);
    let blob = nas_path.to_string_lossy().as_bytes().to_vec();
    let cf_meta = Metadata::file().size(nas_size);
    let placeholder = PlaceholderFile::new(file_name)
        .metadata(cf_meta)
        .mark_in_sync()
        .blob(blob);
    match placeholder.create::<PathBuf>(client_dir.to_path_buf()) {
        Ok(_) => {
            log::info!("[placeholder] refreshed {} ({})", display, nas_size);
            let entry_mtime = nas_meta
                .modified()
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                })
                .unwrap_or(0);
            cache.record_known_file(&client_path, nas_size, entry_mtime);
        }
        Err(e) => log::debug!("[placeholder] refresh failed {}: {}", display, e),
    }
}
