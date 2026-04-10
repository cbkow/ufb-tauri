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

/// Manage auto-start at login (macOS: LaunchAgent plist).
#[cfg(target_os = "macos")]
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    let plist_dir = if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join("Library/LaunchAgents")
    } else {
        return Err("Cannot determine HOME directory".into());
    };

    let plist_path = plist_dir.join("com.unionfiles.mediamount-agent.plist");

    if enabled {
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get exe path: {}", e))?;

        std::fs::create_dir_all(&plist_dir)
            .map_err(|e| format!("Failed to create LaunchAgents dir: {}", e))?;

        let contents = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.unionfiles.mediamount-agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
</dict>
</plist>"#,
            exe_path.display()
        );

        std::fs::write(&plist_path, contents)
            .map_err(|e| format!("Failed to write plist: {}", e))?;

        // Load the agent
        let _ = std::process::Command::new("launchctl")
            .args(["load", &plist_path.to_string_lossy()])
            .output();

        log::info!("Auto-start enabled at {}", plist_path.display());
    } else {
        if plist_path.exists() {
            // Unload first
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &plist_path.to_string_lossy()])
                .output();

            std::fs::remove_file(&plist_path)
                .map_err(|e| format!("Failed to remove plist: {}", e))?;
        }
        log::info!("Auto-start disabled");
    }
    Ok(())
}

/// Check if auto-start is enabled (macOS).
#[cfg(target_os = "macos")]
pub fn is_auto_start_enabled() -> bool {
    if let Some(home) = std::env::var_os("HOME") {
        let plist_path = std::path::PathBuf::from(home)
            .join("Library/LaunchAgents/com.unionfiles.mediamount-agent.plist");
        plist_path.exists()
    } else {
        false
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn set_auto_start(_enabled: bool) -> Result<(), String> {
    Err("Auto-start not implemented for this platform".into())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn is_auto_start_enabled() -> bool {
    false
}
