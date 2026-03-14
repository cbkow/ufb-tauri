#[cfg(windows)]
pub mod windows;

/// Trait for managing mount points (junctions on Windows, symlinks on macOS/Linux).
pub trait MountPoint: Send + Sync {
    /// Create or switch a junction/symlink to point at the given target.
    fn switch(&self, junction_path: &str, target_path: &str) -> Result<(), String>;

    /// Read where a junction/symlink currently points.
    fn read_target(&self, junction_path: &str) -> Result<String, String>;

    /// Remove a junction/symlink.
    fn remove(&self, junction_path: &str) -> Result<(), String>;

    /// Verify the junction points to the expected target.
    fn verify(&self, junction_path: &str, expected_target: &str) -> Result<bool, String>;
}

/// Trait for managing SMB/NFS fallback mounts.
pub trait FallbackMount: Send + Sync {
    /// Map a network share to a drive letter.
    fn map(&self, share_path: &str, drive_letter: &str, username: &str, password: &str) -> Result<(), String>;

    /// Unmap a network drive.
    fn unmap(&self, drive_letter: &str) -> Result<(), String>;

    /// Check if a drive letter is currently mapped.
    fn is_mapped(&self, drive_letter: &str) -> Result<bool, String>;
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

/// Check if a drive letter is already in use.
pub fn is_drive_in_use(letter: &str) -> bool {
    let path = format!("{}:\\", letter);
    std::path::Path::new(&path).exists()
}

/// Manage auto-start at login.
#[cfg(windows)]
pub fn set_auto_start(enabled: bool) -> Result<(), String> {
    use std::process::Command;

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
            .output();
        log::info!("Auto-start disabled");
    }
    Ok(())
}

/// Check if auto-start is enabled.
#[cfg(windows)]
pub fn is_auto_start_enabled() -> bool {
    std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v",
            "MediaMountAgent",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn set_auto_start(_enabled: bool) -> Result<(), String> {
    Err("Auto-start not implemented for this platform".into())
}

#[cfg(not(windows))]
pub fn is_auto_start_enabled() -> bool {
    false
}
