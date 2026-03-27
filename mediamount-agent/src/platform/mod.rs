#[cfg(windows)]
pub mod windows;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

/// Trait for managing mount-point mappings (Linux only).
/// Uses symlinks to point user-facing paths to the actual CIFS mount.
pub trait DriveMapping: Send + Sync {
    /// Map a mount point to the given target path.
    fn switch(&self, mount_point: &str, target_path: &str) -> Result<(), String>;

    /// Read where a mount point currently points.
    fn read_target(&self, mount_point: &str) -> Result<String, String>;

    /// Remove a mount point mapping.
    fn remove(&self, mount_point: &str) -> Result<(), String>;

    /// Verify the mount point maps to the expected target.
    fn verify(&self, mount_point: &str, expected_target: &str) -> Result<bool, String>;
}

/// Trait for establishing SMB sessions (Linux only).
/// On Windows, WNetAddConnection2W handles auth + drive mapping in one call.
pub trait SmbSession: Send + Sync {
    /// Ensure an authenticated SMB session exists for the given share.
    /// `mount_point` is the CIFS mount target path on Linux.
    fn ensure_session(&self, share_path: &str, mount_point: &str, username: &str, password: &str) -> Result<(), String>;
}

/// Trait for credential storage.
pub trait CredentialStore: Send + Sync {
    /// Store credentials.
    fn store(&self, key: &str, username: &str, password: &str) -> Result<(), String>;

    /// Retrieve credentials. Returns (username, password).
    fn retrieve(&self, key: &str) -> Result<(String, String), String>;

    /// Delete stored credentials.
    fn delete(&self, key: &str) -> Result<(), String>;
}

/// Check if a drive letter (Windows) or mount path (Linux) is already in use.
pub fn is_drive_in_use(path_or_letter: &str) -> bool {
    #[cfg(windows)]
    {
        let path = format!("{}:\\", path_or_letter);
        std::path::Path::new(&path).exists()
    }
    #[cfg(target_os = "linux")]
    {
        // Check /proc/mounts for the given path
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            let normalized = path_or_letter.trim_end_matches('/');
            for line in mounts.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts[1].trim_end_matches('/') == normalized {
                    return true;
                }
            }
        }
        // Also check if the path exists as a non-empty directory
        if let Ok(mut entries) = std::fs::read_dir(path_or_letter) {
            return entries.next().is_some();
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        // Check `mount` command output for the path
        if let Ok(output) = std::process::Command::new("mount").output() {
            if output.status.success() {
                let mounts = String::from_utf8_lossy(&output.stdout);
                let normalized = path_or_letter.trim_end_matches('/');
                for line in mounts.lines() {
                    // mount output format: "//server/share on /Volumes/share (smbfs, ...)"
                    if let Some(on_pos) = line.find(" on ") {
                        let after_on = &line[on_pos + 4..];
                        let mount_point = after_on.split(' ').next().unwrap_or("");
                        if mount_point.trim_end_matches('/') == normalized {
                            return true;
                        }
                    }
                }
            }
        }
        // Also check if the path exists (symlink or directory)
        std::path::Path::new(path_or_letter).exists()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        std::path::Path::new(path_or_letter).exists()
    }
}

/// Manage auto-start at login.
#[cfg(windows)]
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;

    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get exe path: {}", e))?;
    let exe_str = exe_path.to_string_lossy();

    if enabled {
        // Add to HKCU\Software\Microsoft\Windows\CurrentVersion\Run
        let status = Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "MediaMountAgent",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", exe_str),
                "/f",
            ])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("Failed to run reg: {}", e))?;
        if !status.status.success() {
            return Err("Failed to add registry key".into());
        }
        log::info!("Auto-start enabled");
    } else {
        let _ = Command::new("reg")
            .args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "MediaMountAgent",
                "/f",
            ])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output();
        log::info!("Auto-start disabled");
    }
    Ok(())
}

/// Check if auto-start is enabled.
#[cfg(windows)]
pub fn is_auto_start_enabled() -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "MediaMountAgent",
        ])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Manage auto-start at login (Linux: XDG autostart .desktop file).
#[cfg(target_os = "linux")]
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    let autostart_dir = if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        std::path::PathBuf::from(config).join("autostart")
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".config/autostart")
    } else {
        return Err("Cannot determine autostart directory".into());
    };

    let desktop_file = autostart_dir.join("mediamount-agent.desktop");

    if enabled {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get exe path: {}", e))?;

        std::fs::create_dir_all(&autostart_dir)
            .map_err(|e| format!("Failed to create autostart dir: {}", e))?;

        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=MediaMount Agent\n\
             Exec={}\n\
             Hidden=false\n\
             NoDisplay=true\n\
             X-GNOME-Autostart-enabled=true\n",
            exe_path.display()
        );

        std::fs::write(&desktop_file, contents)
            .map_err(|e| format!("Failed to write desktop file: {}", e))?;

        log::info!("Auto-start enabled at {}", desktop_file.display());
    } else {
        if desktop_file.exists() {
            std::fs::remove_file(&desktop_file)
                .map_err(|e| format!("Failed to remove desktop file: {}", e))?;
        }
        log::info!("Auto-start disabled");
    }
    Ok(())
}

/// Check if auto-start is enabled (Linux).
#[cfg(target_os = "linux")]
pub fn is_auto_start_enabled() -> bool {
    let desktop_file = if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        std::path::PathBuf::from(config).join("autostart/mediamount-agent.desktop")
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".config/autostart/mediamount-agent.desktop")
    } else {
        return false;
    };
    desktop_file.exists()
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn set_auto_start(_enabled: bool) -> Result<(), String> {
    Err("Auto-start not implemented for this platform".into())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn is_auto_start_enabled() -> bool {
    false
}
