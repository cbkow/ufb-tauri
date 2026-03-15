#[cfg(windows)]
pub mod windows;

/// Trait for managing drive letter mappings via DefineDosDevice (Windows).
pub trait DriveMapping: Send + Sync {
    /// Map a drive letter to the given target path.
    fn switch(&self, drive_letter: &str, target_path: &str) -> Result<(), String>;

    /// Read where a drive letter currently points.
    fn read_target(&self, drive_letter: &str) -> Result<String, String>;

    /// Remove a drive letter mapping.
    fn remove(&self, drive_letter: &str) -> Result<(), String>;

    /// Verify the drive letter maps to the expected target.
    fn verify(&self, drive_letter: &str, expected_target: &str) -> Result<bool, String>;
}

/// Trait for establishing SMB sessions (no drive letter mapping).
pub trait SmbSession: Send + Sync {
    /// Ensure an authenticated SMB session exists for the given share.
    fn ensure_session(&self, share_path: &str, username: &str, password: &str) -> Result<(), String>;
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

#[cfg(not(windows))]
pub fn set_auto_start(_enabled: bool) -> Result<(), String> {
    Err("Auto-start not implemented for this platform".into())
}

#[cfg(not(windows))]
pub fn is_auto_start_enabled() -> bool {
    false
}
