/// Windows mount mapping using directory symlinks.
///
/// Creates symlinks at C:\Volumes\ufb\{share_name} pointing to UNC paths.
/// Requires either Developer Mode or admin elevation for CreateSymbolicLinkW.

use crate::platform::DriveMapping;
use std::os::windows::process::CommandExt;
use std::path::Path;

/// Base directory for all volume symlinks.
pub const VOLUMES_BASE: &str = r"C:\Volumes\ufb";

/// Error returned when symlink creation fails due to missing privileges.
pub const ERROR_PRIVILEGE_NOT_HELD: u32 = 1314;

/// Windows mount mapping using directory symlinks to UNC paths.
pub struct WindowsMountMapping;

impl WindowsMountMapping {
    pub fn new() -> Self {
        Self
    }

    /// Ensure the volumes base directory exists (C:\Volumes\ufb).
    /// This doesn't require elevation — only symlink creation does.
    pub fn ensure_volumes_dir() -> Result<(), String> {
        std::fs::create_dir_all(VOLUMES_BASE)
            .map_err(|e| format!("Failed to create {}: {}", VOLUMES_BASE, e))
    }

    /// Try to create a symlink. Returns Ok(()) on success, or a specific error
    /// distinguishing privilege failure from other errors.
    pub fn try_create_symlink(link_path: &Path, target: &str) -> Result<(), SymlinkError> {
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{
            CreateSymbolicLinkW, SYMBOLIC_LINK_FLAGS, SYMBOLIC_LINK_FLAG_DIRECTORY,
        };

        // SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE (0x2) — required for
        // Developer Mode to work without admin elevation.
        const ALLOW_UNPRIVILEGED: SYMBOLIC_LINK_FLAGS = SYMBOLIC_LINK_FLAGS(0x2);

        // Ensure parent exists
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| SymlinkError::Other(e.to_string()))?;
        }

        let link_wide: Vec<u16> = link_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let target_wide: Vec<u16> = target
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let result = unsafe {
            CreateSymbolicLinkW(
                PCWSTR(link_wide.as_ptr()),
                PCWSTR(target_wide.as_ptr()),
                SYMBOLIC_LINK_FLAG_DIRECTORY | ALLOW_UNPRIVILEGED,
            )
        };

        if result.as_bool() {
            Ok(())
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) {
                Err(SymlinkError::NeedsElevation)
            } else {
                Err(SymlinkError::Other(format!(
                    "CreateSymbolicLinkW failed for {:?}: {}",
                    link_path, err
                )))
            }
        }
    }

    /// Create a junction (reparse point) to a local directory.
    /// Junctions don't require elevation and Explorer preserves the junction path
    /// in the address bar (unlike symlinks which get resolved).
    /// Only works for local→local paths (not UNC targets).
    pub fn create_junction(link_path: &Path, target: &str) -> Result<(), String> {
        // Create the junction directory (must exist as empty dir first)
        std::fs::create_dir_all(link_path)
            .map_err(|e| format!("Failed to create junction dir {:?}: {}", link_path, e))?;

        // Use mklink /J
        let link_str = link_path.to_string_lossy();
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &link_str, target])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("Failed to run mklink: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            // mklink /J fails if the directory isn't empty — remove and retry
            let _ = std::fs::remove_dir(link_path);
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J", &link_str, target])
                .creation_flags(0x08000000)
                .output()
                .map_err(|e| format!("Failed to run mklink: {}", e))?;

            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("mklink /J failed: {}", stderr.trim()))
            }
        }
    }
}

/// Symlink creation error with elevation distinction.
#[derive(Debug)]
pub enum SymlinkError {
    NeedsElevation,
    Other(String),
}

impl std::fmt::Display for SymlinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymlinkError::NeedsElevation => write!(f, "Symlink creation requires elevation"),
            SymlinkError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

/// Returns true if the target path is a UNC path (\\server\share).
fn is_unc_path(path: &str) -> bool {
    path.starts_with(r"\\") && !path.starts_with(r"\\?\")
}

impl DriveMapping for WindowsMountMapping {
    fn switch(&self, mount_point: &str, target_path: &str) -> Result<(), String> {
        let link_path = Path::new(mount_point);

        // Ensure parent directory exists
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dir {}: {}", parent.display(), e))?;
        }

        // If path exists and is already a correct link (symlink or junction), nothing to do
        if link_path.is_symlink() {
            if let Ok(existing) = std::fs::read_link(link_path) {
                if existing.to_string_lossy() == target_path {
                    log::debug!("Link already correct: {} → {}", mount_point, target_path);
                    return Ok(());
                }
            }
            // Target changed — remove old link
            std::fs::remove_dir(link_path)
                .or_else(|_| std::fs::remove_file(link_path))
                .map_err(|e| format!("Failed to remove old link {}: {}", mount_point, e))?;
        }

        // Check if path exists but is NOT a link (refuse to overwrite real dirs/files)
        if link_path.exists() && !link_path.is_symlink() {
            return Err(format!(
                "Path {} exists and is not a symlink/junction — refusing to overwrite",
                mount_point
            ));
        }

        // UNC targets → symlink (requires elevation or Dev Mode)
        // Local targets → junction (no elevation needed, preserves path in Explorer)
        if is_unc_path(target_path) {
            match Self::try_create_symlink(link_path, target_path) {
                Ok(()) => {
                    log::info!("Mapped {} → {} (symlink)", mount_point, target_path);
                    Ok(())
                }
                Err(SymlinkError::NeedsElevation) => {
                    Err("NEEDS_ELEVATION".to_string())
                }
                Err(SymlinkError::Other(e)) => Err(e),
            }
        } else {
            // Local path → use junction (no elevation, path preserved in Explorer)
            Self::create_junction(link_path, target_path)?;
            log::info!("Mapped {} → {} (junction)", mount_point, target_path);
            Ok(())
        }
    }

    fn read_target(&self, mount_point: &str) -> Result<String, String> {
        let link_path = Path::new(mount_point);
        std::fs::read_link(link_path)
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| format!("Failed to read symlink {}: {}", mount_point, e))
    }

    fn remove(&self, mount_point: &str) -> Result<(), String> {
        let link_path = Path::new(mount_point);
        if link_path.is_symlink() {
            // Junctions are removed with remove_dir, symlinks with remove_file
            let result = std::fs::remove_dir(link_path)
                .or_else(|_| std::fs::remove_file(link_path));
            result.map_err(|e| format!("Failed to remove mount link {}: {}", mount_point, e))?;
            log::info!("Removed mount mapping {}", mount_point);
        }
        Ok(())
    }

    fn verify(&self, mount_point: &str, expected_target: &str) -> Result<bool, String> {
        match self.read_target(mount_point) {
            Ok(target) => Ok(target == expected_target),
            Err(e) => Err(e),
        }
    }
}
