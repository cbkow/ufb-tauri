use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Global mutex to serialize macOS mount operations.
/// Prevents concurrent snapshot-open-poll cycles from misidentifying each other's volumes.
static MOUNT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
fn mount_mutex() -> &'static Mutex<()> {
    MOUNT_LOCK.get_or_init(|| Mutex::new(()))
}

/// Mount an SMB share on macOS.
///
/// Strategy (silent-first, native-fallback):
/// 1. Check if already mounted (user-owned location or `/Volumes/`). Reuse if so.
/// 2. Try `mount_smbfs -N smb://host/share ~/.local/share/ufb/smb-mounts/{share}`
///    — silent, uses credentials stored in the macOS login Keychain.
/// 3. If that fails, fall back to `open smb://user@host/share` — macOS's
///    native Connect to Server dialog appears with "Remember in my keychain"
///    pre-checked. User enters credentials once; future sessions satisfy
///    step 2 silently.
///
/// `nas_share_path` is UNC format: `\\server\share`
/// `username` may be empty for guest access. Password is intentionally unused
/// — credential management is delegated to the macOS login Keychain.
pub fn macos_smb_mount(
    nas_share_path: &str,
    username: &str,
    _password: &str,
) -> Result<String, String> {
    // Serialize mount operations so concurrent mounts don't misidentify each other's volumes
    let _guard = mount_mutex().lock().unwrap();

    // Extract expected share name for matching
    let expected_name = nas_share_path
        .trim_end_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or("")
        .to_string();

    // Check if already mounted (user-owned location first, then /Volumes/ fallback
    // for shares a user may have mounted manually via Finder).
    if let Some(existing) = find_existing_user_mount(&expected_name) {
        log::info!("macOS: share already mounted at {}", existing);
        return Ok(existing);
    }
    if let Some(existing) = find_existing_volume(&expected_name, nas_share_path) {
        log::info!("macOS: share already mounted at {}", existing);
        return Ok(existing);
    }

    // ── Silent path via Keychain ───────────────────────────────────────────
    match try_mount_smbfs(&expected_name, nas_share_path, username) {
        Ok(path) => {
            log::info!("macOS: silently mounted at {} via mount_smbfs", path);
            return Ok(path);
        }
        Err(e) => {
            log::info!(
                "macOS: mount_smbfs silent mount unavailable ({}), falling back to native dialog",
                e
            );
        }
    }

    // ── Fallback: native Finder dialog (one-time, Keychain fills in after) ──
    let before = list_volumes();
    let smb_url = unc_to_smb_url(nas_share_path, username);
    log::info!("macOS: mounting via `open {}` (Finder will prompt if first time)", smb_url);

    let output = Command::new("open")
        .arg(&smb_url)
        .output()
        .map_err(|e| format!("Failed to run `open {}`: {}", smb_url, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("open smb:// failed: {}", stderr.trim()));
    }

    // Poll /Volumes/ for the new mount point (macOS mounts asynchronously).
    let mount_point = poll_for_new_volume(&before, nas_share_path)?;
    log::info!("macOS: mounted at {}", mount_point);

    Ok(mount_point)
}

/// Attempt a silent `mount_smbfs -N` using credentials from the login Keychain.
/// Returns the mount path on success. Returns Err with stderr on any failure.
fn try_mount_smbfs(
    share_name: &str,
    nas_share_path: &str,
    username: &str,
) -> Result<String, String> {
    let smb_base = crate::config::MountConfig::smb_mount_base();
    std::fs::create_dir_all(&smb_base)
        .map_err(|e| format!("Failed to create SMB mount base: {}", e))?;
    let mountpoint = smb_base.join(share_name);

    if mountpoint.exists() {
        if is_mountpoint(&mountpoint) {
            return Ok(mountpoint.to_string_lossy().to_string());
        }
        let _ = std::fs::remove_dir(&mountpoint);
    }
    std::fs::create_dir_all(&mountpoint)
        .map_err(|e| format!("Failed to create mountpoint {}: {}", mountpoint.display(), e))?;

    let smb_url = unc_to_smb_url(nas_share_path, username);

    let output = Command::new("mount_smbfs")
        .arg("-N")
        .arg(&smb_url)
        .arg(&mountpoint)
        .output()
        .map_err(|e| format!("Failed to run mount_smbfs: {}", e))?;

    if output.status.success() {
        Ok(mountpoint.to_string_lossy().to_string())
    } else {
        let _ = std::fs::remove_dir(&mountpoint);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.trim().to_string())
    }
}

/// Check if a path is a mountpoint by comparing device IDs of path and parent.
fn is_mountpoint(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let path_meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let parent = match path.parent() {
        Some(p) => p,
        None => return false,
    };
    let parent_meta = match std::fs::metadata(parent) {
        Ok(m) => m,
        Err(_) => return false,
    };
    path_meta.dev() != parent_meta.dev()
}

/// Check if a matching share is already mounted at our user-owned base.
fn find_existing_user_mount(share_name: &str) -> Option<String> {
    let smb_base = crate::config::MountConfig::smb_mount_base();
    let candidate = smb_base.join(share_name);
    if is_mountpoint(&candidate) {
        Some(candidate.to_string_lossy().to_string())
    } else {
        None
    }
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
///
/// Usernames are percent-encoded per RFC 3986 userinfo rules so that names
/// containing spaces or other reserved characters produce a URL that
/// `mount_smbfs` accepts. Finder's `open smb://` tolerates unencoded
/// usernames; percent-encoded ones work in both.
fn unc_to_smb_url(unc_path: &str, username: &str) -> String {
    let stripped = unc_path.trim_start_matches('\\').replace('\\', "/");
    if username.is_empty() {
        format!("smb://{}", stripped)
    } else {
        format!("smb://{}@{}", percent_encode_userinfo(username), stripped)
    }
}

/// Percent-encode a string for use in the userinfo component of a URL.
/// Preserves unreserved characters and userinfo-safe sub-delims (RFC 3986 §3.2.1).
/// Encodes everything else — critically including ` `, `@`, `:`, `/`.
fn percent_encode_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'.' | b'_' | b'~'
            | b'!' | b'$' | b'&' | b'\'' | b'(' | b')'
            | b'*' | b'+' | b',' | b';' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Check if a volume matching the expected name is already mounted from the correct server.
/// Accepts macOS dedup suffixes (e.g. `MyShare-1` when another SMB mount already holds
/// `MyShare`) and verifies via `mount` output that the backing SMB source matches.
pub fn find_existing_volume(expected_name: &str, nas_share_path: &str) -> Option<String> {
    let candidates: Vec<String> = if let Ok(entries) = std::fs::read_dir("/Volumes") {
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if name.eq_ignore_ascii_case(expected_name)
                    || strip_macos_dedup_suffix(&name)
                        .map(|base| base.eq_ignore_ascii_case(expected_name))
                        .unwrap_or(false)
                {
                    let path = format!("/Volumes/{}", name);
                    // Verify it's actually a mount point (not just an empty dir)
                    if std::fs::read_dir(&path).map(|mut d| d.next().is_some()).unwrap_or(false) {
                        return Some(path);
                    }
                }
                None
            })
            .collect()
    } else {
        return None;
    };

    if candidates.is_empty() {
        return None;
    }

    // Verify the candidate is actually mounted from the expected server/share
    let smb_fragment = nas_share_path
        .trim_start_matches('\\')
        .replace('\\', "/")
        .to_lowercase();

    let mount_output = Command::new("mount").output().ok()?;
    let mount_text = String::from_utf8_lossy(&mount_output.stdout);

    for candidate in &candidates {
        for line in mount_text.lines() {
            if line.contains(candidate) && line.to_lowercase().contains(&smb_fragment) {
                return Some(candidate.clone());
            }
        }
    }

    None
}

/// Strip a macOS dedup suffix like "-1", "-2" from a volume name.
/// Returns the base name if a suffix was stripped, or None if no suffix present.
/// Correctly handles names that already contain hyphens (e.g. "my-share-1" → "my-share").
fn strip_macos_dedup_suffix(name: &str) -> Option<&str> {
    if let Some(pos) = name.rfind('-') {
        let after = &name[pos + 1..];
        if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            return Some(&name[..pos]);
        }
    }
    None
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
/// Retries for up to 60 seconds (user may need to enter credentials in a dialog).
fn poll_for_new_volume(before: &HashSet<String>, nas_share_path: &str) -> Result<String, String> {
    // Extract the expected share name from UNC path for matching
    // \\server\share\subfolder → "subfolder"
    let expected_name = nas_share_path
        .trim_end_matches('\\')
        .rsplit('\\')
        .next()
        .unwrap_or("")
        .to_string();

    log::info!("macOS: polling /Volumes/ for '{}' (timeout 60s)", expected_name);

    for attempt in 0..120 {
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
            // Accept name with macOS dedup suffix (-1, -2, etc.)
            for entry in &new_entries {
                if let Some(base) = strip_macos_dedup_suffix(entry) {
                    if base.eq_ignore_ascii_case(&expected_name) {
                        return Ok(format!("/Volumes/{}", entry));
                    }
                }
            }
            // No match — keep polling. Do NOT grab an arbitrary volume.
        }

        if attempt % 10 == 9 {
            log::info!("macOS: still waiting for mount in /Volumes/ ({}s)...", (attempt + 1) / 2);
        }
    }

    Err(format!(
        "Timed out after 60s waiting for SMB mount to appear in /Volumes/ (expected '{}')",
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

    #[test]
    fn test_unc_to_smb_url_username_with_space() {
        // Regression: mount_smbfs rejects unencoded spaces in usernames.
        assert_eq!(
            unc_to_smb_url(r"\\nas\share", "first last"),
            "smb://first%20last@nas/share"
        );
    }

    #[test]
    fn test_percent_encode_userinfo_common_chars() {
        assert_eq!(percent_encode_userinfo("alice"), "alice");
        assert_eq!(percent_encode_userinfo("first last"), "first%20last");
        assert_eq!(percent_encode_userinfo("user:pass"), "user%3Apass");
        assert_eq!(percent_encode_userinfo("a@b"), "a%40b");
        assert_eq!(percent_encode_userinfo("a.b-c_d"), "a.b-c_d");
    }

    #[test]
    fn test_strip_macos_dedup_suffix_simple() {
        assert_eq!(strip_macos_dedup_suffix("media-1"), Some("media"));
        assert_eq!(strip_macos_dedup_suffix("media-2"), Some("media"));
        assert_eq!(strip_macos_dedup_suffix("media-12"), Some("media"));
    }

    #[test]
    fn test_strip_macos_dedup_suffix_hyphenated_name() {
        assert_eq!(strip_macos_dedup_suffix("my-share-1"), Some("my-share"));
        assert_eq!(strip_macos_dedup_suffix("my-share-2"), Some("my-share"));
    }

    #[test]
    fn test_strip_macos_dedup_suffix_no_suffix() {
        assert_eq!(strip_macos_dedup_suffix("media"), None);
        assert_eq!(strip_macos_dedup_suffix("my-share"), None);
        assert_eq!(strip_macos_dedup_suffix("archive-2023data"), None);
    }

    #[test]
    fn test_strip_macos_dedup_suffix_edge_cases() {
        // Name ending in hyphen-digits that is part of the real name
        // The function strips it, but callers only use the result when it matches expected_name
        assert_eq!(strip_macos_dedup_suffix("archive-2023"), Some("archive"));
        // Trailing hyphen with no digits
        assert_eq!(strip_macos_dedup_suffix("media-"), None);
    }
}
