use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<i64>,
    pub extension: String,
}

/// Check if a path is a symlink pointing to a directory.
fn is_dir_symlink(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
        }
        _ => false,
    }
}

/// List directory contents, returning file entries sorted (dirs first, then by name).
pub fn list_directory(path: &str) -> Result<Vec<FileEntry>, String> {
    let dir_path = Path::new(path);

    let mut entries = Vec::new();
    let read_dir =
        std::fs::read_dir(dir_path).map_err(|e| format!("Not a directory or cannot read: {}", e))?;

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Check if this entry is a symlink (before following it)
        let is_symlink = entry.file_type()
            .map(|ft| ft.is_symlink())
            .unwrap_or(false);

        // Use std::fs::metadata (follows symlinks) to get the target's metadata.
        // Fall back to symlink_metadata so symlinks still appear even if target is slow.
        let metadata = std::fs::metadata(entry.path())
            .or_else(|_| std::fs::symlink_metadata(entry.path()));
        let metadata = match metadata {
            Ok(m) => m,
            Err(_) => continue, // skip entries we can't stat
        };
        let name = entry.file_name().to_string_lossy().to_string();
        // Use the path as-is (preserving symlink paths like C:\gfx_nas\...).
        // Do NOT canonicalize — that resolves symlinks to their target,
        // breaking path prefix matching for subscriptions and mount detection.
        let path_str = entry.path().to_string_lossy().to_string();
        let extension = entry
            .path()
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);

        // Directory symlinks: metadata.is_dir() is true when std::fs::metadata
        // followed the symlink. If we fell back to symlink_metadata, is_dir is
        // false — so for symlinks, assume directory if we can't tell.
        let is_dir = metadata.is_dir() || (is_symlink && !metadata.is_file());

        entries.push(FileEntry {
            name,
            path: path_str,
            is_dir,
            size: metadata.len(),
            modified,
            extension,
        });
    }

    // Sort: directories first, then alphabetically by name (case-insensitive)
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok(entries)
}

/// Create a new directory.
pub fn create_directory(path: &str) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("Failed to create directory: {}", e))
}

/// Rename a file or directory.
pub fn rename_path(old_path: &str, new_path: &str) -> Result<(), String> {
    std::fs::rename(old_path, new_path).map_err(|e| format!("Failed to rename: {}", e))
}

/// Delete files/directories to the OS trash/recycle bin.
/// Falls back to native shell delete (with confirmation) for network/junction paths
/// where the Recycle Bin isn't available.
pub fn delete_to_trash(paths: &[String]) -> Result<(), String> {
    let mut failed: Vec<String> = Vec::new();

    for path in paths {
        if let Err(_) = trash::delete(path) {
            failed.push(path.clone());
        }
    }

    if failed.is_empty() {
        return Ok(());
    }

    // Fallback: use native shell delete for paths that couldn't be recycled
    // (typically network/junction paths with no Recycle Bin)
    #[cfg(windows)]
    {
        shell_delete_files(&failed)?;
    }

    #[cfg(not(windows))]
    {
        for path in &failed {
            let p = Path::new(path);
            if p.is_dir() {
                std::fs::remove_dir_all(p)
                    .map_err(|e| format!("Failed to delete '{}': {}", path, e))?;
            } else {
                std::fs::remove_file(p)
                    .map_err(|e| format!("Failed to delete '{}': {}", path, e))?;
            }
        }
    }

    Ok(())
}

/// Windows: delete files using SHFileOperationW which shows the native
/// "Are you sure you want to permanently delete?" dialog for network paths.
#[cfg(windows)]
fn shell_delete_files(paths: &[String]) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SHFileOperationW, SHFILEOPSTRUCTW, FO_DELETE};
    use windows::Win32::UI::Shell::FOF_ALLOWUNDO;

    // SHFileOperationW expects a double-null-terminated string of paths
    let mut wide: Vec<u16> = Vec::new();
    for path in paths {
        wide.extend(path.encode_utf16());
        wide.push(0); // null separator between paths
    }
    wide.push(0); // double-null terminator

    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: windows::core::PCWSTR(wide.as_ptr()),
        fFlags: FOF_ALLOWUNDO.0 as u16, // Try recycle first; if unavailable, shows permanent delete dialog
        ..Default::default()
    };

    let result = unsafe { SHFileOperationW(&mut op) };

    if result != 0 {
        // User cancelled or operation failed
        if op.fAnyOperationsAborted.as_bool() {
            return Ok(()); // User cancelled — not an error
        }
        return Err(format!("Shell delete failed (error {})", result));
    }

    Ok(())
}

/// Copy files to a destination directory.
pub fn copy_files(sources: &[String], dest: &str) -> Result<(), String> {
    let dest_path = Path::new(dest);
    for src in sources {
        let src_path = Path::new(src);
        let file_name = src_path
            .file_name()
            .ok_or_else(|| format!("Invalid source path: {}", src))?;
        let target = dest_path.join(file_name);

        if src_path.is_dir() {
            let mut options = fs_extra::dir::CopyOptions::new();
            options.overwrite = false;
            options.copy_inside = true;
            fs_extra::dir::copy(src_path, &target, &options)
                .map_err(|e| format!("Failed to copy dir '{}': {}", src, e))?;
        } else {
            let options = fs_extra::file::CopyOptions::new();
            fs_extra::file::copy(src_path, &target, &options)
                .map_err(|e| format!("Failed to copy file '{}': {}", src, e))?;
        }
    }
    Ok(())
}

/// Move files to a destination directory.
pub fn move_files(sources: &[String], dest: &str) -> Result<(), String> {
    let dest_path = Path::new(dest);
    for src in sources {
        let src_path = Path::new(src);
        let file_name = src_path
            .file_name()
            .ok_or_else(|| format!("Invalid source path: {}", src))?;
        let target = dest_path.join(file_name);

        if src_path.is_dir() {
            let mut options = fs_extra::dir::CopyOptions::new();
            options.overwrite = false;
            options.copy_inside = true;
            fs_extra::dir::move_dir(src_path, &target, &options)
                .map_err(|e| format!("Failed to move dir '{}': {}", src, e))?;
        } else {
            let options = fs_extra::file::CopyOptions::new();
            fs_extra::file::move_file(src_path, &target, &options)
                .map_err(|e| format!("Failed to move file '{}': {}", src, e))?;
        }
    }
    Ok(())
}

/// Copy file paths to clipboard.
/// On Windows, uses CF_HDROP format so Explorer recognizes them for paste.
/// On other platforms, falls back to plain text (one path per line).
pub fn clipboard_copy_paths(paths: &[String]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        clipboard_copy_paths_windows(paths)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let text = paths.join("\n");
        let mut clipboard =
            arboard::Clipboard::new().map_err(|e| format!("Failed to open clipboard: {}", e))?;
        clipboard
            .set_text(text)
            .map_err(|e| format!("Failed to set clipboard: {}", e))
    }
}

/// Windows: write CF_HDROP (DROPFILES struct + null-terminated wide-string paths)
/// so that Explorer can paste the files. Also sets "Preferred DropEffect" to COPY.
#[cfg(target_os = "windows")]
fn clipboard_copy_paths_windows(paths: &[String]) -> Result<(), String> {
    use clipboard_win::{Clipboard, raw};

    // Open clipboard (RAII guard closes on drop)
    let _clip = Clipboard::new_attempts(10)
        .map_err(|e| format!("Failed to open clipboard: {}", e))?;

    // Clear existing data
    raw::empty().map_err(|e| format!("Failed to empty clipboard: {}", e))?;

    // Write CF_HDROP
    raw::set_file_list(paths)
        .map_err(|e| format!("Failed to set clipboard CF_HDROP: {}", e))?;

    // Set "Preferred DropEffect" = DROPEFFECT_COPY (0x1) so Explorer knows to copy
    set_preferred_drop_effect(0x01);

    Ok(())
}

/// Windows: set the "Preferred DropEffect" clipboard format (tells Explorer to copy vs move).
#[cfg(target_os = "windows")]
fn set_preferred_drop_effect(effect: u32) {
    use windows::Win32::System::DataExchange::{RegisterClipboardFormatW, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::core::w;

    unsafe {
        let fmt = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        if fmt == 0 {
            return;
        }
        let hmem = GlobalAlloc(GMEM_MOVEABLE, 4);
        if let Ok(hmem) = hmem {
            let lock: *mut std::ffi::c_void = GlobalLock(hmem);
            if !lock.is_null() {
                std::ptr::write(lock as *mut u32, effect);
                let _ = GlobalUnlock(hmem);
            }
            let _ = SetClipboardData(fmt, windows::Win32::Foundation::HANDLE(hmem.0));
        }
    }
}

/// Paste file paths from clipboard (returns list of paths).
/// On Windows, reads CF_HDROP first; falls back to plain text.
pub fn clipboard_paste_paths() -> Result<Vec<String>, String> {
    #[cfg(target_os = "windows")]
    {
        if let Ok(paths) = clipboard_paste_paths_hdrop() {
            if !paths.is_empty() {
                return Ok(paths);
            }
        }
    }

    // Fallback: read plain text
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("Failed to open clipboard: {}", e))?;
    let text = clipboard
        .get_text()
        .map_err(|e| format!("Failed to read clipboard: {}", e))?;
    let paths: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && Path::new(l).exists())
        .collect();
    Ok(paths)
}

/// Windows: read CF_HDROP from clipboard to get file paths.
#[cfg(target_os = "windows")]
fn clipboard_paste_paths_hdrop() -> Result<Vec<String>, String> {
    use clipboard_win::{Clipboard, raw};

    let _clip = Clipboard::new_attempts(10)
        .map_err(|e| format!("Failed to open clipboard: {}", e))?;

    let mut file_list: Vec<String> = Vec::new();
    raw::get_file_list(&mut file_list)
        .map_err(|e| format!("Failed to read CF_HDROP: {}", e))?;

    let paths: Vec<String> = file_list
        .into_iter()
        .filter(|s| !s.is_empty() && Path::new(s).exists())
        .collect();

    Ok(paths)
}

/// Reveal a path in the native file manager.
pub fn reveal_in_file_manager(path: &str) -> Result<(), String> {
    opener::reveal(path).map_err(|e| format!("Failed to reveal: {}", e))
}

/// Open a file with the default application.
pub fn open_file(path: &str) -> Result<(), String> {
    opener::open(path).map_err(|e| format!("Failed to open: {}", e))
}
