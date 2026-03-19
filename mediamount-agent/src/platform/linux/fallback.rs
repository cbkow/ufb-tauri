use crate::platform::SmbSession;

/// Linux SMB session management via mount -t cifs.
/// Uses pkexec for elevation when needed. Falls back to checking /proc/mounts
/// for shares already mounted via fstab or manual mount.
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
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        // Translate UNC \\server\share → //server/share
        let cifs_path = share_path.replace('\\', "/").trim_start_matches('/').to_string();
        let cifs_path = format!("//{}", cifs_path);

        // Derive a mount point from the config.
        // The orchestrator will call switch_drive_mapping separately, but we need
        // the share actually mounted for it to be accessible.
        // Use the smb_target_path from config (passed via share_path context),
        // but the SmbSession trait only gives us share_path, username, password.
        // We'll mount at the standard location derived from the share name.
        let mount_point = smb_mount_point_for(&cifs_path);

        // Check if already mounted at this mount point
        if is_mounted_at(&mount_point) {
            log::info!("SMB share {} already mounted at {}", cifs_path, mount_point);
            return Ok(());
        }

        // Check if the CIFS source is mounted anywhere
        if is_source_mounted(&cifs_path) {
            log::info!("SMB share {} already mounted elsewhere", cifs_path);
            return Ok(());
        }

        // Ensure mount point exists
        let _ = std::fs::create_dir_all(&mount_point);

        // Build credentials file (temporary, chmod 600)
        let data_dir = if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(".local/share/ufb")
        } else {
            std::path::PathBuf::from("/tmp/ufb")
        };
        let _ = std::fs::create_dir_all(&data_dir);
        let cred_path = data_dir.join(".smb_cred_tmp");

        let cred_content = format!("username={}\npassword={}\n", username, password);
        std::fs::write(&cred_path, &cred_content)
            .map_err(|e| format!("Failed to write temp credentials: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600));
        }

        let uid = get_uid();
        let gid = get_gid();

        let mount_opts = format!(
            "credentials={},uid={},gid={},file_mode=0644,dir_mode=0755,vers=3.0,rw",
            cred_path.display(), uid, gid
        );

        log::info!("Mounting {} at {} via pkexec mount -t cifs", cifs_path, mount_point);

        let output = std::process::Command::new("pkexec")
            .args(["mount", "-t", "cifs", &cifs_path, &mount_point, "-o", &mount_opts])
            .output();

        // Clean up credentials file immediately
        let _ = std::fs::remove_file(&cred_path);

        match output {
            Ok(o) if o.status.success() => {
                log::info!("SMB mount succeeded: {} at {}", cifs_path, mount_point);
                Ok(())
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // If pkexec was dismissed/cancelled, check if maybe it's already mounted
                if is_mounted_at(&mount_point) || is_source_mounted(&cifs_path) {
                    log::info!("SMB share appears to be mounted despite mount error");
                    Ok(())
                } else {
                    Err(format!("mount -t cifs failed: {}", stderr.trim()))
                }
            }
            Err(e) => {
                // pkexec not available — check if pre-mounted
                if is_mounted_at(&mount_point) || is_source_mounted(&cifs_path) {
                    log::info!("SMB share already mounted (pkexec unavailable)");
                    Ok(())
                } else {
                    Err(format!(
                        "Failed to mount SMB share (pkexec unavailable, not pre-mounted): {}",
                        e
                    ))
                }
            }
        }
    }
}

/// Derive a mount point path from a CIFS source path like //server/share.
fn smb_mount_point_for(cifs_path: &str) -> String {
    let stripped = cifs_path.trim_start_matches('/');
    // "server/share" → take the share name
    let share_name = stripped.split('/').nth(1).unwrap_or(stripped);

    let base = if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local/share/ufb/mnt")
    } else {
        std::path::PathBuf::from("/tmp/ufb-mnt")
    };
    let _ = std::fs::create_dir_all(&base);
    base.join(format!("{}-smb", share_name)).to_string_lossy().to_string()
}

/// Check if a specific mount point has something mounted on it.
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

/// Check if a CIFS source (//server/share) is mounted anywhere.
fn is_source_mounted(cifs_path: &str) -> bool {
    let normalized = cifs_path.trim_end_matches('/');
    if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let source = parts[0].trim_end_matches('/');
                if source.eq_ignore_ascii_case(normalized) {
                    return true;
                }
            }
        }
    }
    false
}

fn get_uid() -> String {
    unsafe { libc::getuid() }.to_string()
}

fn get_gid() -> String {
    unsafe { libc::getgid() }.to_string()
}
