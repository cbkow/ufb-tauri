use crate::platform::DriveMapping;
use std::path::Path;

/// macOS mount mapping using symlinks.
/// The user-facing path (e.g. /opt/ufb/mounts/nas-main) is a symlink pointing
/// to the actual /Volumes/ mount point that `open smb://` created.
pub struct MacosMountMapping;

impl MacosMountMapping {
    pub fn new() -> Self {
        Self
    }
}

impl DriveMapping for MacosMountMapping {
    fn switch(&self, mount_point: &str, target_path: &str) -> Result<(), String> {
        let link_path = Path::new(mount_point);

        // Ensure parent directory exists
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dir {}: {}", parent.display(), e))?;
        }

        // Check if path exists but is NOT a symlink (refuse to overwrite real dirs/files)
        if link_path.exists() && !link_path.is_symlink() {
            return Err(format!(
                "Path {} exists and is not a symlink — refusing to overwrite",
                mount_point
            ));
        }

        // Remove old symlink if it exists
        if link_path.is_symlink() {
            std::fs::remove_file(link_path)
                .map_err(|e| format!("Failed to remove old symlink {}: {}", mount_point, e))?;
        }

        // Create new symlink
        std::os::unix::fs::symlink(target_path, link_path)
            .map_err(|e| format!("Failed to create symlink {} → {}: {}", mount_point, target_path, e))?;

        log::info!("Mount mapping {} → {}", mount_point, target_path);
        Ok(())
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
            std::fs::remove_file(link_path)
                .map_err(|e| format!("Failed to remove symlink {}: {}", mount_point, e))?;
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
