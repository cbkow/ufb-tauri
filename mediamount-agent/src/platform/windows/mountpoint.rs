use crate::platform::MountPoint;
use std::path::Path;

/// Windows mount point implementation using raw NTFS reparse points.
/// Uses DeviceIoControl + FSCTL_SET_REPARSE_POINT to create junctions
/// that work with both local paths (R:\) and UNC paths (\\server\share).
/// No elevation or Developer Mode required.
pub struct WindowsMountPoint;

impl WindowsMountPoint {
    pub fn new() -> Self {
        Self
    }
}

/// Convert a target path to the NT reparse target format:
/// - Local paths: `C:\foo` → `\??\C:\foo`
/// - UNC paths:   `\\server\share` → `\??\UNC\server\share`
/// - Drive letters: `S:\` → `\??\S:\`
fn to_reparse_target(target: &str) -> String {
    if target.starts_with(r"\\") {
        // UNC path: \\server\share → \??\UNC\server\share
        let trimmed = target.trim_end_matches('\\');
        format!(r"\??\UNC\{}", &trimmed[2..])
    } else {
        // Local path — preserve trailing backslash for drive roots (S:\ → \??\S:\)
        // This is required for junctions to drive roots to work correctly.
        let normalized = if target.ends_with('\\') {
            target.to_string()
        } else if target.ends_with(':') {
            // Bare drive letter like "S:" → "S:\"
            format!(r"{}\", target)
        } else {
            target.to_string()
        };
        format!(r"\??\{}", normalized)
    }
}

/// Build the REPARSE_DATA_BUFFER for a mount point (junction).
/// Layout:
///   ReparseTag:           u32 (IO_REPARSE_TAG_MOUNT_POINT = 0xA0000003)
///   ReparseDataLength:    u16
///   Reserved:             u16
///   SubstituteNameOffset: u16
///   SubstituteNameLength: u16
///   PrintNameOffset:      u16
///   PrintNameLength:      u16
///   PathBuffer:           [u16; ...] (substitute name + NUL + print name + NUL)
fn build_reparse_buffer(target: &str, print_name: &str) -> Vec<u8> {
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA0000003;

    let substitute: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let print: Vec<u16> = print_name.encode_utf16().chain(std::iter::once(0)).collect();

    let substitute_byte_len = (substitute.len() * 2) as u16;
    let print_byte_len = (print.len() * 2) as u16;

    // SubstituteNameOffset = 0
    // SubstituteNameLength = substitute bytes (excluding NUL? no, including NUL in buffer but length excludes it)
    // Actually: lengths exclude the NUL terminator
    let sub_name_len = ((substitute.len() - 1) * 2) as u16;
    let print_name_len = ((print.len() - 1) * 2) as u16;
    let print_name_offset = substitute_byte_len;

    // Total path buffer size
    let path_buffer_size = substitute_byte_len + print_byte_len;

    // ReparseDataLength = 8 bytes (4 offsets/lengths) + path buffer
    let reparse_data_length = 8 + path_buffer_size;

    // Total buffer = 8 bytes header + reparse data
    let total_size = 8 + reparse_data_length as usize;
    let mut buf = vec![0u8; total_size];

    // Header
    buf[0..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf[4..6].copy_from_slice(&reparse_data_length.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes()); // Reserved

    // MountPointReparseBuffer
    buf[8..10].copy_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
    buf[10..12].copy_from_slice(&sub_name_len.to_le_bytes()); // SubstituteNameLength
    buf[12..14].copy_from_slice(&print_name_offset.to_le_bytes()); // PrintNameOffset
    buf[14..16].copy_from_slice(&print_name_len.to_le_bytes()); // PrintNameLength

    // PathBuffer: substitute name bytes then print name bytes
    let path_start = 16;
    for (i, &word) in substitute.iter().enumerate() {
        let offset = path_start + i * 2;
        buf[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
    }
    for (i, &word) in print.iter().enumerate() {
        let offset = path_start + substitute.len() * 2 + i * 2;
        buf[offset..offset + 2].copy_from_slice(&word.to_le_bytes());
    }

    buf
}

impl MountPoint for WindowsMountPoint {
    fn switch(&self, junction_path: &str, target_path: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, FILE_GENERIC_WRITE,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        const FSCTL_SET_REPARSE_POINT: u32 = 0x000900A4;

        let jp = Path::new(junction_path);

        // If link already exists, remove it first
        if jp.exists() || jp.read_link().is_ok() {
            self.remove(junction_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = jp.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create parent dirs: {}", e))?;
        }

        // Create an empty directory for the junction
        std::fs::create_dir(jp)
            .map_err(|e| format!("Failed to create junction dir {}: {}", junction_path, e))?;

        // Build the reparse data
        let reparse_target = to_reparse_target(target_path);
        let buf = build_reparse_buffer(&reparse_target, target_path);

        // Open the directory with reparse point and backup semantics
        let jp_wide: Vec<u16> = junction_path.encode_utf16().chain(std::iter::once(0)).collect();

        let handle = unsafe {
            CreateFileW(
                PCWSTR(jp_wide.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|e| {
            // Clean up the empty directory on failure
            let _ = std::fs::remove_dir(jp);
            format!("Failed to open junction dir for reparse: {}", e)
        })?;

        // Set the reparse point
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_SET_REPARSE_POINT,
                Some(buf.as_ptr() as *const _),
                buf.len() as u32,
                None,
                0,
                None,
                None,
            )
        };

        unsafe {
            let _ = CloseHandle(handle);
        }

        if let Err(e) = result {
            let _ = std::fs::remove_dir(jp);
            return Err(format!("FSCTL_SET_REPARSE_POINT failed: {}", e));
        }

        log::info!("Junction {} → {}", junction_path, target_path);
        Ok(())
    }

    fn read_target(&self, junction_path: &str) -> Result<String, String> {
        let jp = Path::new(junction_path);
        let target = std::fs::read_link(jp)
            .map_err(|e| format!("Failed to read junction {}: {}", junction_path, e))?;
        Ok(target.to_string_lossy().to_string())
    }

    fn remove(&self, junction_path: &str) -> Result<(), String> {
        let jp = Path::new(junction_path);
        if jp.exists() || jp.read_link().is_ok() {
            // For junctions, remove the directory entry (not recursive delete)
            std::fs::remove_dir(jp)
                .map_err(|e| format!("Failed to remove junction {}: {}", junction_path, e))?;
            log::info!("Removed junction {}", junction_path);
        }
        Ok(())
    }

    fn verify(&self, junction_path: &str, expected_target: &str) -> Result<bool, String> {
        match self.read_target(junction_path) {
            Ok(target) => {
                // Normalize: remove \\?\ and \??\ prefixes, handle UNC conversion
                let normalized_target = normalize_reparse_path(&target);
                let normalized_expected = normalize_reparse_path(expected_target);
                Ok(normalized_target.eq_ignore_ascii_case(&normalized_expected))
            }
            Err(e) => Err(e),
        }
    }
}

/// Normalize a path for comparison, handling reparse point path formats:
/// - `\??\C:\foo` → `C:\foo`
/// - `\??\UNC\server\share` → `\\server\share`
/// - `\\?\C:\foo` → `C:\foo`
/// - `\\?\UNC\server\share` → `\\server\share`
fn normalize_reparse_path(path: &str) -> String {
    let p = path.replace('/', "\\");
    let p = p.trim_end_matches('\\');

    if let Some(rest) = p.strip_prefix(r"\??\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = p.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = p.strip_prefix(r"\??\") {
        rest.to_string()
    } else if let Some(rest) = p.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        p.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_reparse_target_local() {
        assert_eq!(to_reparse_target(r"R:\"), r"\??\R:\");
        assert_eq!(to_reparse_target(r"R:"), r"\??\R:\");
        assert_eq!(to_reparse_target(r"C:\foo\bar"), r"\??\C:\foo\bar");
        assert_eq!(to_reparse_target(r"S:\"), r"\??\S:\");
    }

    #[test]
    fn test_to_reparse_target_unc() {
        assert_eq!(
            to_reparse_target(r"\\192.168.40.100\Jobs_Live"),
            r"\??\UNC\192.168.40.100\Jobs_Live"
        );
        assert_eq!(
            to_reparse_target(r"\\nas\media"),
            r"\??\UNC\nas\media"
        );
    }

    #[test]
    fn test_normalize_reparse_path() {
        assert_eq!(normalize_reparse_path(r"\??\C:\foo"), r"C:\foo");
        assert_eq!(normalize_reparse_path(r"\\?\C:\foo"), r"C:\foo");
        assert_eq!(
            normalize_reparse_path(r"\??\UNC\server\share"),
            r"\\server\share"
        );
        assert_eq!(
            normalize_reparse_path(r"\\?\UNC\server\share"),
            r"\\server\share"
        );
        assert_eq!(normalize_reparse_path(r"R:\"), r"R:");
        assert_eq!(
            normalize_reparse_path(r"\\server\share"),
            r"\\server\share"
        );
    }

    #[test]
    fn test_reparse_buffer_not_empty() {
        let buf = build_reparse_buffer(r"\??\R:", r"R:\");
        assert!(buf.len() > 16);
        // Check tag
        let tag = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(tag, 0xA0000003);
    }
}
