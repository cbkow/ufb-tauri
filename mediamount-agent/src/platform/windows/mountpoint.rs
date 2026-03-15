use crate::platform::MountPoint;
use std::path::Path;

/// Windows mount point implementation using directory symlinks.
/// Requires Developer Mode enabled on Windows.
pub struct WindowsMountPoint;

impl WindowsMountPoint {
    pub fn new() -> Self {
        Self
    }
}

impl MountPoint for WindowsMountPoint {
    fn switch(&self, link_path: &str, target_path: &str) -> Result<(), String> {
        let lp = Path::new(link_path);

        // Remove existing symlink/dir if present
        if lp.exists() || lp.read_link().is_ok() {
            self.remove(link_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = lp.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dirs: {}", e))?;
        }

        std::os::windows::fs::symlink_dir(target_path, link_path)
            .map_err(|e| format!("symlink_dir failed: {} (ensure Developer Mode is enabled)", e))?;

        log::info!("Symlink {} → {}", link_path, target_path);
        Ok(())
    }

    fn read_target(&self, link_path: &str) -> Result<String, String> {
        let lp = Path::new(link_path);
        let target = std::fs::read_link(lp)
            .map_err(|e| format!("Failed to read symlink {}: {}", link_path, e))?;
        Ok(target.to_string_lossy().to_string())
    }

    fn remove(&self, link_path: &str) -> Result<(), String> {
        let lp = Path::new(link_path);
        if lp.exists() || lp.read_link().is_ok() {
            std::fs::remove_dir(lp)
                .map_err(|e| format!("Failed to remove symlink {}: {}", link_path, e))?;
            log::info!("Removed symlink {}", link_path);
        }
        Ok(())
    }

    fn verify(&self, link_path: &str, expected_target: &str) -> Result<bool, String> {
        match self.read_target(link_path) {
            Ok(target) => {
                let norm_target = normalize_path(&target);
                let norm_expected = normalize_path(expected_target);
                Ok(norm_target.eq_ignore_ascii_case(&norm_expected))
            }
            Err(e) => Err(e),
        }
    }
}

/// Normalize a path for comparison: trim trailing backslashes, unify separators.
fn normalize_path(path: &str) -> String {
    let p = path.replace('/', "\\");
    p.trim_end_matches('\\').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path(r"R:\"), "R:");
        assert_eq!(normalize_path(r"\\server\share\"), r"\\server\share");
        assert_eq!(normalize_path(r"\\server\share"), r"\\server\share");
        assert_eq!(normalize_path("R:/foo/bar"), r"R:\foo\bar");
    }
}
