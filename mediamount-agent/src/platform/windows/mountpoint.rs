use crate::platform::DriveMapping;

/// Windows drive mapping implementation using DefineDosDevice.
/// No special privileges required — works without Developer Mode.
pub struct WindowsDriveMapping;

impl WindowsDriveMapping {
    pub fn new() -> Self {
        Self
    }
}

/// Convert a user-facing path to an NT target path for DefineDosDevice.
/// - `R:\` → `\??\R:\`
/// - `\\server\share` → `\??\UNC\server\share`
fn to_nt_target_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    if path.starts_with("\\\\") {
        // UNC path: \\server\share → \??\UNC\server\share
        format!("\\??\\UNC\\{}", &path[2..])
    } else {
        // Drive path: R:\ → \??\R:\
        format!("\\??\\{}", path)
    }
}

/// Convert an NT target path back to a user-facing path.
/// - `\??\R:\` → `R:\`
/// - `\??\UNC\server\share` → `\\server\share`
fn from_nt_target_path(raw: &str) -> String {
    let raw = raw.trim_end_matches('\0');
    if let Some(rest) = raw.strip_prefix("\\??\\UNC\\") {
        format!("\\\\{}", rest)
    } else if let Some(rest) = raw.strip_prefix("\\??\\") {
        rest.to_string()
    } else {
        raw.to_string()
    }
}

impl DriveMapping for WindowsDriveMapping {
    fn switch(&self, drive_letter: &str, target_path: &str) -> Result<(), String> {
        // Always remove existing mapping first to avoid stacking
        let _ = self.remove(drive_letter);

        let device_name = format!("{}:", drive_letter);
        let nt_target = to_nt_target_path(target_path);

        #[cfg(windows)]
        {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::Storage::FileSystem::{
                DefineDosDeviceW, DDD_RAW_TARGET_PATH,
            };

            let device_wide: Vec<u16> = OsStr::new(&device_name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let target_wide: Vec<u16> = OsStr::new(&nt_target)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            unsafe {
                DefineDosDeviceW(
                    DDD_RAW_TARGET_PATH,
                    windows::core::PCWSTR(device_wide.as_ptr()),
                    windows::core::PCWSTR(target_wide.as_ptr()),
                )
            }
            .map_err(|e| format!(
                "DefineDosDeviceW failed for {} → {}: {}",
                device_name, target_path, e
            ))?;
        }

        log::info!("Drive mapping {}:\\ → {}", drive_letter, target_path);
        Ok(())
    }

    fn read_target(&self, drive_letter: &str) -> Result<String, String> {
        let device_name = format!("{}:", drive_letter);

        #[cfg(windows)]
        {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::Storage::FileSystem::QueryDosDeviceW;

            let device_wide: Vec<u16> = OsStr::new(&device_name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut buffer = vec![0u16; 4096];

            let len = unsafe {
                QueryDosDeviceW(
                    windows::core::PCWSTR(device_wide.as_ptr()),
                    Some(&mut buffer),
                )
            };

            if len == 0 {
                return Err(format!("QueryDosDeviceW failed for {}", device_name));
            }

            // The buffer contains null-terminated strings; take the first one
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(len as usize);
            let raw = String::from_utf16_lossy(&buffer[..end]);
            return Ok(from_nt_target_path(&raw));
        }

        #[cfg(not(windows))]
        Err("DriveMapping not supported on this platform".into())
    }

    fn remove(&self, drive_letter: &str) -> Result<(), String> {
        let device_name = format!("{}:", drive_letter);

        // Read current target first so we can pass it to the remove call
        let current_target = match self.read_target(drive_letter) {
            Ok(t) => t,
            Err(_) => return Ok(()), // No mapping exists
        };

        let nt_target = to_nt_target_path(&current_target);

        #[cfg(windows)]
        {
            use std::ffi::OsStr;
            use std::os::windows::ffi::OsStrExt;
            use windows::Win32::Storage::FileSystem::{
                DefineDosDeviceW, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION,
            };

            let device_wide: Vec<u16> = OsStr::new(&device_name)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let target_wide: Vec<u16> = OsStr::new(&nt_target)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            unsafe {
                DefineDosDeviceW(
                    DDD_RAW_TARGET_PATH | DDD_REMOVE_DEFINITION,
                    windows::core::PCWSTR(device_wide.as_ptr()),
                    windows::core::PCWSTR(target_wide.as_ptr()),
                )
            }
            .map_err(|e| format!(
                "DefineDosDeviceW remove failed for {}: {}",
                device_name, e
            ))?;
        }

        log::info!("Removed drive mapping {}", device_name);
        Ok(())
    }

    fn verify(&self, drive_letter: &str, expected_target: &str) -> Result<bool, String> {
        match self.read_target(drive_letter) {
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

    #[test]
    fn test_to_nt_target_path_drive() {
        assert_eq!(to_nt_target_path(r"R:\"), r"\??\R:\");
        assert_eq!(to_nt_target_path(r"R:\media"), r"\??\R:\media");
    }

    #[test]
    fn test_to_nt_target_path_unc() {
        assert_eq!(
            to_nt_target_path(r"\\server\share"),
            r"\??\UNC\server\share"
        );
        assert_eq!(
            to_nt_target_path(r"\\nas\media\subfolder"),
            r"\??\UNC\nas\media\subfolder"
        );
    }

    #[test]
    fn test_from_nt_target_path_drive() {
        assert_eq!(from_nt_target_path(r"\??\R:\"), r"R:\");
        assert_eq!(from_nt_target_path(r"\??\R:\media"), r"R:\media");
    }

    #[test]
    fn test_from_nt_target_path_unc() {
        assert_eq!(
            from_nt_target_path(r"\??\UNC\server\share"),
            r"\\server\share"
        );
    }

    #[test]
    fn test_roundtrip_drive() {
        let original = r"R:\";
        let nt = to_nt_target_path(original);
        let back = from_nt_target_path(&nt);
        assert_eq!(back, original);
    }

    #[test]
    fn test_roundtrip_unc() {
        let original = r"\\server\share";
        let nt = to_nt_target_path(original);
        let back = from_nt_target_path(&nt);
        assert_eq!(back, original);
    }
}
