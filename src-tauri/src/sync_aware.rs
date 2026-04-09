/// Sync-aware file operations.
///
/// When source and destination are both within a Cloud Files sync root,
/// we can use optimized operations that avoid hydrating dehydrated files:
/// - Move → fs::rename (instant, no hydration)
/// - Copy → create dehydrated placeholder with same NAS blob (instant)
///
/// Falls back to normal fs_extra operations when outside sync roots.

use std::path::{Path, PathBuf};

use crate::mount_client::{load_mount_config, MountConfig};

/// Info about a sync root that a path belongs to.
#[derive(Clone)]
pub struct SyncRootInfo {
    pub client_root: PathBuf,
    pub nas_root: PathBuf,
}

/// Check if a path is inside a sync root. Returns the sync root info if so.
pub fn find_sync_root(path: &Path) -> Option<SyncRootInfo> {
    let config = load_mount_config();
    for mount in &config.mounts {
        if !mount.sync_enabled {
            continue;
        }
        let client_root = sync_root_dir(mount);
        if path.starts_with(&client_root) {
            return Some(SyncRootInfo {
                client_root,
                nas_root: PathBuf::from(&mount.nas_share_path),
            });
        }
    }
    None
}

/// Check if source and dest are in the SAME sync root.
pub fn same_sync_root(src: &Path, dest: &Path) -> Option<SyncRootInfo> {
    let src_root = find_sync_root(src)?;
    if dest.starts_with(&src_root.client_root) {
        Some(src_root)
    } else {
        None
    }
}

/// Move a file or directory within a sync root using fs::rename.
/// Returns Ok(true) if handled, Ok(false) to fall back to normal move.
pub fn sync_move(src: &Path, dest_dir: &Path) -> Result<bool, String> {
    let file_name = match src.file_name() {
        Some(f) => f,
        None => return Ok(false),
    };
    let target = dest_dir.join(file_name);

    // Only optimize if both are in the same sync root
    if same_sync_root(src, &target).is_none() {
        return Ok(false);
    }

    // fs::rename is instant within the same filesystem — no hydration needed.
    // The CF API rename callback propagates the rename to the NAS.
    std::fs::rename(src, &target)
        .map_err(|e| format!("Rename failed: {}", e))?;

    log::info!("[sync-aware] Move (rename): {:?} → {:?}", src, target);
    Ok(true)
}

/// Copy a file within a sync root by creating a dehydrated placeholder.
/// Returns Ok(true) if handled, Ok(false) to fall back to normal copy.
#[cfg(windows)]
pub fn sync_copy(src: &Path, dest_dir: &Path) -> Result<bool, String> {
    use cloud_filter::metadata::Metadata;
    use cloud_filter::placeholder_file::PlaceholderFile;

    let file_name = match src.file_name() {
        Some(f) => f,
        None => return Ok(false),
    };
    let target = dest_dir.join(file_name);

    let root = match same_sync_root(src, &target) {
        Some(r) => r,
        None => return Ok(false),
    };

    if src.is_dir() {
        // For directories, fall back to normal copy — too complex to replicate
        // the full subtree as placeholders.
        return Ok(false);
    }

    // Derive the NAS path for the source file
    let relative = src.strip_prefix(&root.client_root).unwrap_or(Path::new(""));
    let nas_path = root.nas_root.join(relative);

    // Get file metadata from NAS (size)
    let meta = std::fs::metadata(&nas_path)
        .map_err(|e| format!("Can't read NAS file: {}", e))?;

    // Create a dehydrated placeholder at the destination with the same NAS blob
    let blob = nas_path.to_string_lossy().as_bytes().to_vec();
    let cf_meta = Metadata::file().size(meta.len());

    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }

    PlaceholderFile::new(file_name.to_string_lossy().as_ref())
        .metadata(cf_meta)
        .mark_in_sync()
        .blob(blob)
        .create::<PathBuf>(dest_dir.to_path_buf())
        .map_err(|e| format!("Failed to create placeholder copy: {}", e))?;

    log::info!("[sync-aware] Copy (placeholder): {:?} → {:?}", src, target);
    Ok(true)
}

#[cfg(not(windows))]
pub fn sync_copy(_src: &Path, _dest_dir: &Path) -> Result<bool, String> {
    Ok(false)
}

/// Derive the sync root directory for a mount config.
/// Must match the logic in mediamount-agent config.rs.
fn sync_root_dir(mount: &MountConfig) -> PathBuf {
    if let Some(ref p) = mount.sync_root_path {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    // Default: C:\Volumes\ufb\{shareName}
    let share_name = mount
        .nas_share_path
        .trim_start_matches('\\')
        .split('\\')
        .nth(1)
        .unwrap_or(&mount.id);

    #[cfg(windows)]
    {
        PathBuf::from(format!(r"C:\Volumes\ufb\{}", share_name))
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home).join(".local/share/ufb/sync").join(share_name)
        } else {
            PathBuf::from("/tmp/ufb-sync").join(share_name)
        }
    }
}
