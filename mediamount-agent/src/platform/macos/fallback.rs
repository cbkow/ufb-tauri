use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Mount an SMB share on macOS using `open smb://`.
///
/// Flow:
/// 1. Record current /Volumes/ contents
/// 2. Run `open smb://user@server/share`
/// 3. Poll /Volumes/ to discover the new mount point
/// 4. Return the actual mount path (handles -1/-2 suffix variance)
///
/// `nas_share_path` is UNC format: `\\server\share`
/// `username` may be empty for guest access.
pub fn macos_smb_mount(
    nas_share_path: &str,
    username: &str,
    _password: &str,
) -> Result<String, String> {
    // Snapshot /Volumes/ before mount
    let before = list_volumes();

    // Convert UNC path to smb:// URL
    // \\server\share → smb://user@server/share
    let smb_url = unc_to_smb_url(nas_share_path, username);
    log::info!("macOS: mounting via `open {}`", smb_url);

    // Note: `open smb://` uses the system's Finder-based SMB mount.
    // Credentials are typically handled by the macOS Keychain after first interactive login,
    // or by the username embedded in the URL. Password is not passed via URL for security.
    let output = Command::new("open")
        .arg(&smb_url)
        .output()
        .map_err(|e| format!("Failed to run `open {}`: {}", smb_url, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("open smb:// failed: {}", stderr.trim()));
    }

    // Poll /Volumes/ for the new mount point (macOS mounts asynchronously)
    let mount_point = poll_for_new_volume(&before, nas_share_path)?;
    log::info!("macOS: mounted at {}", mount_point);

    Ok(mount_point)
}

/// Unmount an SMB share on macOS.
/// `volumes_path` is the actual /Volumes/... path (not the symlink).
pub fn macos_smb_unmount(volumes_path: &str) -> Result<(), String> {
    let path = Path::new(volumes_path);
    if !path.exists() {
        log::info!("macOS: mount point {} doesn't exist, nothing to unmount", volumes_path);
        return Ok(());
    }

    log::info!("macOS: unmounting {}", volumes_path);

    // Try diskutil first (clean unmount)
    let output = Command::new("diskutil")
        .args(["unmount", volumes_path])
        .output()
        .map_err(|e| format!("Failed to run diskutil unmount: {}", e))?;

    if output.status.success() {
        return Ok(());
    }

    // Fallback to umount
    let output = Command::new("umount")
        .arg(volumes_path)
        .output()
        .map_err(|e| format!("Failed to run umount: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Unmount failed: {}", stderr.trim()))
    }
}

/// Convert a UNC path to an smb:// URL.
/// `\\server\share` → `smb://user@server/share` (or `smb://server/share` if no user)
fn unc_to_smb_url(unc_path: &str, username: &str) -> String {
    // Strip leading backslashes and normalize
    let stripped = unc_path.trim_start_matches('\\').replace('\\', "/");

    if username.is_empty() {
        format!("smb://{}", stripped)
    } else {
        // Insert user@ before the server
        format!("smb://{}@{}", username, stripped)
    }
}

/// List current entries in /Volumes/.
fn list_volumes() -> HashSet<String> {
    let mut volumes = HashSet::new();
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                volumes.insert(name.to_string());
            }
        }
    }
    volumes
}

/// Poll /Volumes/ for a new entry that appeared after the mount command.
/// Retries for up to 10 seconds.
fn poll_for_new_volume(before: &HashSet<String>, nas_share_path: &str) -> Result<String, String> {
    // Extract the expected share name from UNC path for matching
    // \\server\share → "share"
    let expected_name = nas_share_path
        .trim_end_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or("")
        .to_string();

    for attempt in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));

        let after = list_volumes();
        let new_entries: Vec<&String> = after.difference(before).collect();

        if !new_entries.is_empty() {
            // Prefer exact match on share name
            for entry in &new_entries {
                if entry.eq_ignore_ascii_case(&expected_name) {
                    return Ok(format!("/Volumes/{}", entry));
                }
            }
            // Accept name with -1, -2 suffix
            for entry in &new_entries {
                let base = entry.split('-').next().unwrap_or(entry);
                if base.eq_ignore_ascii_case(&expected_name) {
                    return Ok(format!("/Volumes/{}", entry));
                }
            }
            // If we can't match by name, take any new volume that appeared
            if let Some(entry) = new_entries.first() {
                log::warn!(
                    "macOS: new volume '{}' doesn't match expected '{}', using anyway",
                    entry, expected_name
                );
                return Ok(format!("/Volumes/{}", entry));
            }
        }

        if attempt % 4 == 3 {
            log::debug!("macOS: waiting for mount to appear in /Volumes/ ({}s)...", (attempt + 1) / 2);
        }
    }

    Err(format!(
        "Timed out waiting for SMB mount to appear in /Volumes/ (expected '{}')",
        expected_name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unc_to_smb_url_with_user() {
        assert_eq!(
            unc_to_smb_url(r"\\nas\media", "chris"),
            "smb://chris@nas/media"
        );
    }

    #[test]
    fn test_unc_to_smb_url_no_user() {
        assert_eq!(
            unc_to_smb_url(r"\\nas\media", ""),
            "smb://nas/media"
        );
    }

    #[test]
    fn test_unc_to_smb_url_deep_path() {
        assert_eq!(
            unc_to_smb_url(r"\\server.local\share\subfolder", "admin"),
            "smb://admin@server.local/share/subfolder"
        );
    }
}
