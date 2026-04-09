/// Client-side file system watcher on the local sync root.
///
/// Detects new or modified regular files (not placeholders) and forwards
/// events to the upload coordinator. Runs on a dedicated blocking thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileAttributesW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// Events from the client-side watcher.
#[derive(Debug)]
pub enum ClientFsEvent {
    /// A regular (non-placeholder) file was created or modified.
    Modified(PathBuf),
    /// A file was removed.
    Removed(PathBuf),
}

/// Start the client watcher on a dedicated thread.
pub fn start(
    client_root: PathBuf,
    tx: mpsc::Sender<ClientFsEvent>,
    shutdown: Arc<AtomicBool>,
    dir_handle: Arc<AtomicUsize>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("client-watcher".into())
        .spawn(move || {
            if let Err(e) = run(&client_root, &tx, &shutdown, &dir_handle) {
                if !shutdown.load(Ordering::Relaxed) {
                    log::error!("[write-through] Client watcher error: {}", e);
                }
            }
        })
        .expect("Failed to spawn client watcher thread")
}

fn run(
    client_root: &Path,
    tx: &mpsc::Sender<ClientFsEvent>,
    shutdown: &AtomicBool,
    dir_handle: &AtomicUsize,
) -> Result<(), String> {
    let wide_path: Vec<u16> = client_root
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            HANDLE::default(),
        )
    }
    .map_err(|e| format!("Failed to open sync root for watching: {}", e))?;

    // Store handle so stop() can cancel via CancelIoEx
    dir_handle.store(handle.0 as usize, Ordering::SeqCst);

    log::info!(
        "[write-through] Client watcher started on {:?}",
        client_root
    );

    let mut buffer = vec![0u8; 16384];
    let mut bytes_returned: u32 = 0;
    let filter =
        FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let result = unsafe {
            ReadDirectoryChangesW(
                handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                true, // watch subtree
                filter,
                Some(&mut bytes_returned),
                None,
                None,
            )
        };

        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        match result {
            Ok(()) if bytes_returned > 0 => {
                process_events(&buffer, bytes_returned, client_root, tx);
            }
            Ok(()) => {
                log::warn!("[write-through] Client watcher buffer overflow");
            }
            Err(e) => {
                if !shutdown.load(Ordering::Relaxed) {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return Err(format!("ReadDirectoryChangesW error: {}", e));
                }
                break;
            }
        }
    }

    unsafe {
        let _ = CloseHandle(handle);
    }
    Ok(())
}

fn process_events(
    buffer: &[u8],
    bytes_returned: u32,
    client_root: &Path,
    tx: &mpsc::Sender<ClientFsEvent>,
) {
    let mut offset: usize = 0;

    loop {
        if offset + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > bytes_returned as usize {
            break;
        }

        let info =
            unsafe { &*(buffer.as_ptr().add(offset) as *const FILE_NOTIFY_INFORMATION) };

        let name_len = info.FileNameLength as usize / 2;
        let name_slice =
            unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_len) };
        let relative_str = String::from_utf16_lossy(name_slice);

        if !should_process(&relative_str) {
            if info.NextEntryOffset == 0 {
                break;
            }
            offset += info.NextEntryOffset as usize;
            continue;
        }

        let full_path = client_root.join(&relative_str);
        let action = info.Action.0;

        match action {
            1 | 3 | 5 => {
                // FILE_ACTION_ADDED, MODIFIED, RENAMED_NEW
                if is_file(&full_path) {
                    log::debug!("[write-through] Detected change: {}", relative_str);
                    let _ = tx.blocking_send(ClientFsEvent::Modified(full_path));
                }
            }
            2 | 4 => {
                // FILE_ACTION_REMOVED, RENAMED_OLD
                let _ = tx.blocking_send(ClientFsEvent::Removed(full_path));
            }
            _ => {}
        }

        if info.NextEntryOffset == 0 {
            break;
        }
        offset += info.NextEntryOffset as usize;
    }
}

/// Filter filenames that should not trigger uploads.
fn should_process(relative_path: &str) -> bool {
    let name = relative_path.rsplit('\\').next().unwrap_or(relative_path);
    if name.contains(".~sync.") {
        return false;
    }
    if name.starts_with("~$") {
        return false;
    }
    if name.ends_with(".tmp") {
        return false;
    }
    if name.starts_with('#') || name.starts_with('@') {
        return false;
    }
    true
}

/// Check if a path is a file (not a directory). Includes both regular
/// files and placeholder files — we need to detect modifications to
/// placeholders too (user edits to hydrated files).
pub fn is_file(path: &Path) -> bool {
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
    if attrs == u32::MAX {
        return false; // Doesn't exist or error
    }
    (attrs & 0x10) == 0 // Not a directory
}
