use crate::platform::SmbSession;

/// Linux SMB session management.
/// Strategy:
/// 1. Check if already mounted (kernel CIFS via /proc/mounts or gvfs)
/// 2. If not, mount via `gio mount` (userspace, no root needed)
/// 3. Symlink from the expected mount_point to wherever the share actually lives
pub struct LinuxSmbSession;

impl LinuxSmbSession {
    pub fn new() -> Self {
        Self
    }
}

impl SmbSession for LinuxSmbSession {
    fn ensure_session(
        &self,
        share_path: &str,
        mount_point: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        // Translate UNC \\server\share → //server/share for CIFS matching
        let cifs_path = share_path.replace('\\', "/").trim_start_matches('/').to_string();
        let cifs_path = format!("//{}", cifs_path);

        // Build smb:// URI for gio
        let smb_uri = format!("smb:{}", cifs_path);

        // 1. Check if already mounted at the expected mount point (real mount or working symlink)
        if is_mounted_at(mount_point) || is_accessible(mount_point) {
            log::info!("Share already accessible at {}", mount_point);
            return Ok(());
        }

        // 2. Check kernel CIFS mounts (/proc/mounts)
        if let Some(existing) = find_source_mount_point(&cifs_path) {
            log::info!("CIFS share {} already mounted at {}", cifs_path, existing);
            ensure_symlink(mount_point, &existing);
            return Ok(());
        }

        // 3. Check if gvfs already has it mounted
        if let Some(gvfs_path) = find_gvfs_mount(&cifs_path) {
            log::info!("Share {} already in gvfs at {}", cifs_path, gvfs_path);
            ensure_symlink(mount_point, &gvfs_path);
            return Ok(());
        }

        // 4. Mount via gio (no root needed)
        log::info!("Mounting {} via gio mount", smb_uri);
        let gio_result = gio_mount(&smb_uri, username, password);

        match gio_result {
            Ok(()) => {
                // Find where gvfs put it
                if let Some(gvfs_path) = find_gvfs_mount(&cifs_path) {
                    log::info!("gio mount succeeded, gvfs path: {}", gvfs_path);
                    ensure_symlink(mount_point, &gvfs_path);
                    return Ok(());
                }
                // Maybe it ended up as a kernel mount
                if let Some(existing) = find_source_mount_point(&cifs_path) {
                    log::info!("gio mount resulted in kernel mount at {}", existing);
                    ensure_symlink(mount_point, &existing);
                    return Ok(());
                }
                log::warn!("gio mount returned success but share not found in gvfs or /proc/mounts");
                Err("gio mount succeeded but share not found".into())
            }
            Err(e) => {
                // gio failed — last resort, check if something else mounted it in the meantime
                if let Some(existing) = find_source_mount_point(&cifs_path) {
                    log::info!("Share appeared at {} despite gio error", existing);
                    ensure_symlink(mount_point, &existing);
                    return Ok(());
                }
                Err(format!("gio mount failed: {}", e))
            }
        }
    }
}

/// Mount an SMB share via `gio mount`. Passes credentials via stdin
/// using the GIO_MOUNT_SPEC environment approach, or falls back to
/// embedding them in the URI for anonymous-style auth.
fn gio_mount(smb_uri: &str, username: &str, password: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // If we have credentials, try with --anonymous first won't work,
    // so we use a small expect-like approach: gio mount prompts on stdin.
    // However the simplest reliable approach is to use the URI with
    // the credentials and --anonymous flag to skip the interactive prompt.

    // First try: if no credentials needed (guest access)
    if username.is_empty() {
        let output = Command::new("gio")
            .args(["mount", "--anonymous", smb_uri])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("Failed to run gio: {}", e))?;

        if output.status.success() {
            return Ok(());
        }
        // Fall through to authenticated attempt
    }

    // gio mount with credentials: it reads from stdin interactively.
    // We spawn with piped stdin and feed the prompts.
    let mut child = Command::new("gio")
        .args(["mount", smb_uri])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn gio: {}", e))?;

    // gio mount prompts: "User [user]: ", "Domain [WORKGROUP]: ", "Password: "
    // We feed responses line by line.
    if let Some(mut stdin) = child.stdin.take() {
        // Small delay to let gio print its prompts
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = writeln!(stdin, "{}", username);    // User
        let _ = writeln!(stdin, "");                // Domain (accept default)
        let _ = writeln!(stdin, "{}", password);    // Password
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("gio mount failed to complete: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // "Already mounted" is not an error
        if stderr.contains("Already mounted") || stdout.contains("Already mounted") {
            return Ok(());
        }
        Err(format!("{}{}", stderr.trim(), stdout.trim()))
    }
}

/// Find a gvfs mount matching the given CIFS path (//server/share).
/// gvfs mounts live under /run/user/$UID/gvfs/smb-share:server=X,share=Y
fn find_gvfs_mount(cifs_path: &str) -> Option<String> {
    let uid = unsafe { libc::getuid() };
    let gvfs_dir = format!("/run/user/{}/gvfs", uid);

    // Parse //server/share from cifs_path
    let trimmed = cifs_path.trim_start_matches('/');
    let parts: Vec<&str> = trimmed.splitn(2, '/').collect();
    if parts.len() < 2 {
        return None;
    }
    let server = parts[0].to_lowercase();
    let share = parts[1].to_lowercase();

    // Scan gvfs directory for matching mount
    let entries = std::fs::read_dir(&gvfs_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        // Format: smb-share:server=X,share=Y or smb-share:server=X,share=Y,user=Z,...
        if name.starts_with("smb-share:") {
            let has_server = name.contains(&format!("server={}", server));
            let has_share = name.contains(&format!("share={}", share));
            if has_server && has_share {
                return Some(entry.path().to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Create or update a symlink from link_path to target_path.
fn ensure_symlink(link_path: &str, target_path: &str) {
    let link = std::path::Path::new(link_path);

    // Ensure parent exists
    if let Some(parent) = link.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Check if symlink already points to the right place
    if link.is_symlink() {
        if let Ok(existing_target) = std::fs::read_link(link) {
            if existing_target.to_string_lossy() == target_path {
                return; // Already correct
            }
        }
        let _ = std::fs::remove_file(link);
    }

    // Don't overwrite a real directory/file
    if link.exists() && !link.is_symlink() {
        log::warn!("{} exists and is not a symlink, skipping", link_path);
        return;
    }

    match std::os::unix::fs::symlink(target_path, link) {
        Ok(()) => log::info!("Symlink {} → {}", link_path, target_path),
        Err(e) => log::warn!("Failed to symlink {} → {}: {}", link_path, target_path, e),
    }
}

/// Check if a specific mount point has something mounted on it (via /proc/mounts).
fn is_mounted_at(mount_point: &str) -> bool {
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        let normalized = mount_point.trim_end_matches('/');
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1].trim_end_matches('/') == normalized {
                return true;
            }
        }
    }
    false
}

/// Check if a path is accessible (exists and can be listed).
fn is_accessible(path: &str) -> bool {
    std::fs::read_dir(path).is_ok()
}

/// Find where a CIFS source (//server/share) is mounted via /proc/mounts.
fn find_source_mount_point(cifs_path: &str) -> Option<String> {
    let normalized = cifs_path.trim_end_matches('/');
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let source = parts[0].trim_end_matches('/');
                if source.eq_ignore_ascii_case(normalized) {
                    return Some(parts[1].to_string());
                }
            }
        }
    }
    None
}
