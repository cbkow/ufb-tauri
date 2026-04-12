use crate::config::{self, MountConfig};
use crate::messages::*;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// App group container directory for the FileProvider extension.
const APP_GROUP_ID: &str = "5Z4S9VHV56.group.com.unionfiles.mediamount-tray";

/// Resolve the app group container path.
fn app_group_container() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join("Library/Group Containers")
            .join(APP_GROUP_ID)
    } else {
        PathBuf::from("/tmp/ufb-fileprovider")
    }
}

/// Resolve the socket path for the file operations server.
fn socket_path() -> PathBuf {
    app_group_container().join("fp.sock")
}

/// Temp directory for staging files for the FileProvider extension.
fn temp_dir() -> PathBuf {
    app_group_container().join("temp")
}

/// File operations IPC server for the FileProvider extension.
/// Request-response model: each request from a client gets a response on the same stream.
pub struct FileOpsServer;

impl FileOpsServer {
    /// Start the file operations server in the background.
    /// Returns immediately — the server runs on spawned tokio tasks.
    pub fn start() {
        tokio::spawn(async {
            if let Err(e) = Self::run().await {
                log::error!("[FileOps] Server failed: {}", e);
            }
        });
    }

    async fn run() -> Result<(), String> {
        let sock_path = socket_path();
        let container = app_group_container();

        // Ensure the app group container and temp dir exist
        std::fs::create_dir_all(&container)
            .map_err(|e| format!("Failed to create app group container: {}", e))?;
        std::fs::create_dir_all(&temp_dir())
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;

        // Clean up stale socket
        if sock_path.exists() {
            let _ = std::fs::remove_file(&sock_path);
        }

        let listener = UnixListener::bind(&sock_path)
            .map_err(|e| format!("Failed to bind fileops socket at {}: {}", sock_path.display(), e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o700));
        }

        log::info!("[FileOps] Listening on {}", sock_path.display());

        // Accept connections in a blocking loop
        loop {
            let (stream, _addr) = match tokio::task::spawn_blocking({
                let listener_clone = listener.try_clone().expect("Failed to clone listener");
                move || listener_clone.accept()
            })
            .await
            {
                Ok(Ok(pair)) => pair,
                Ok(Err(e)) => {
                    log::error!("[FileOps] Accept failed: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
                Err(e) => {
                    log::error!("[FileOps] Accept task panicked: {}", e);
                    break;
                }
            };

            log::info!("[FileOps] Client connected");

            // Handle each client in its own blocking thread
            tokio::task::spawn_blocking(move || {
                Self::handle_client(stream);
            });
        }

        Ok(())
    }

    /// Handle a single client connection: read requests, send responses.
    fn handle_client(stream: UnixStream) {
        let mut reader = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => {
                log::error!("[FileOps] Failed to clone stream: {}", e);
                return;
            }
        };
        let mut writer = stream;

        loop {
            let req: FileOpsRequest = match super::recv_message(&mut reader) {
                Ok(msg) => msg,
                Err(_) => {
                    log::info!("[FileOps] Client disconnected");
                    break;
                }
            };

            log::debug!("[FileOps] Request: {:?}", req);

            let response = Self::handle_request(req);

            if let Err(e) = super::send_message(&mut writer, &response) {
                log::warn!("[FileOps] Failed to send response: {}", e);
                break;
            }
        }
    }

    /// Process a single file operation request and return the response.
    fn handle_request(req: FileOpsRequest) -> FileOpsResponse {
        match req {
            FileOpsRequest::Ping => FileOpsResponse::Pong,
            FileOpsRequest::ListDir(r) => Self::handle_list_dir(r),
            FileOpsRequest::Stat(r) => Self::handle_stat(r),
            FileOpsRequest::ReadFile(r) => Self::handle_read_file(r),
            FileOpsRequest::WriteFile(r) => Self::handle_write_file(r),
            FileOpsRequest::DeleteItem(r) => Self::handle_delete_item(r),
        }
    }

    /// Resolve a domain + relative path to an absolute filesystem path.
    /// For sync mode mounts, the user-facing symlink points to ~/Library/CloudStorage/
    /// (which is the FileProvider domain — circular). So we resolve directly to
    /// /Volumes/{share_name} where the actual SMB mount lives.
    fn resolve_path(domain: &str, relative_path: &str) -> Result<PathBuf, String> {
        let config = config::load_config();
        let mount = config
            .mounts
            .iter()
            .find(|m| m.share_name() == domain && m.enabled)
            .ok_or_else(|| format!("No enabled mount found for domain '{}'", domain))?;

        // For sync mode: go directly to /Volumes/{share_name} (the SMB mount)
        // For regular mode: mount_path() → /opt/ufb/mounts/{share_name} → /Volumes/ via symlink
        let base = if mount.is_sync_mode() {
            // Find the actual SMB mount in /Volumes/
            let volumes_path = PathBuf::from("/Volumes").join(&mount.share_name());
            if !volumes_path.exists() {
                return Err(format!(
                    "SMB mount not found at {}. Is the share mounted?",
                    volumes_path.display()
                ));
            }
            volumes_path
        } else {
            PathBuf::from(mount.mount_path())
        };

        // Canonicalize base first
        let base_canonical = base
            .canonicalize()
            .map_err(|e| format!("Base path resolution failed for {}: {}", base.display(), e))?;

        let full_path = if relative_path.is_empty() {
            base_canonical.clone()
        } else {
            base_canonical.join(relative_path)
        };

        // Safety: prevent path traversal
        let canonical = full_path
            .canonicalize()
            .map_err(|e| format!("Path resolution failed for {}: {}", full_path.display(), e))?;

        if !canonical.starts_with(&base_canonical) {
            return Err(format!("Path traversal detected: {}", relative_path));
        }

        Ok(canonical)
    }

    fn handle_list_dir(req: ListDirReq) -> FileOpsResponse {
        let dir_path = match Self::resolve_path(&req.domain, &req.relative_path) {
            Ok(p) => p,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: e,
                });
            }
        };

        log::debug!("[FileOps] ListDir: {}", dir_path.display());

        let entries = match std::fs::read_dir(&dir_path) {
            Ok(rd) => rd,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("Failed to read directory {}: {}", dir_path.display(), e),
                });
            }
        };

        let mut result = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden/system files
            if name.starts_with('.') || name.starts_with('@') || name.starts_with('#') {
                continue;
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            result.push(DirEntry {
                name,
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified: meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
                created: meta
                    .created()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0),
            });
        }

        log::debug!("[FileOps] ListDir: {} entries", result.len());

        FileOpsResponse::DirListing(DirListingResp {
            request_id: req.request_id,
            entries: result,
        })
    }

    fn handle_stat(req: StatReq) -> FileOpsResponse {
        let file_path = match Self::resolve_path(&req.domain, &req.relative_path) {
            Ok(p) => p,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: e,
                });
            }
        };

        let meta = match std::fs::metadata(&file_path) {
            Ok(m) => m,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("Stat failed for {}: {}", file_path.display(), e),
                });
            }
        };

        let name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        FileOpsResponse::FileStat(FileStatResp {
            request_id: req.request_id,
            name,
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
            created: meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        })
    }

    fn handle_read_file(req: ReadFileReq) -> FileOpsResponse {
        let source_path = match Self::resolve_path(&req.domain, &req.relative_path) {
            Ok(p) => p,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: e,
                });
            }
        };

        let meta = match std::fs::metadata(&source_path) {
            Ok(m) => m,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("File not found: {}", e),
                });
            }
        };

        if meta.is_dir() {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: "Cannot read a directory".to_string(),
            });
        }

        // Copy file to temp dir in app group container
        let temp = temp_dir();
        let temp_name = format!(
            "{}-{:x}.tmp",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros(),
            std::process::id()
        );
        let temp_path = temp.join(&temp_name);

        log::info!(
            "[FileOps] ReadFile: {} → {} ({} bytes)",
            source_path.display(),
            temp_path.display(),
            meta.len()
        );

        if let Err(e) = std::fs::copy(&source_path, &temp_path) {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: format!("Failed to copy file: {}", e),
            });
        }

        FileOpsResponse::FileReady(FileReadyResp {
            request_id: req.request_id,
            temp_path: temp_path.to_string_lossy().to_string(),
            size: meta.len(),
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        })
    }

    fn handle_write_file(req: WriteFileReq) -> FileOpsResponse {
        let dest_path = match Self::resolve_path(&req.domain, &req.relative_path) {
            Ok(p) => p,
            Err(e) => {
                // Path doesn't exist yet — build it from base + relative
                // resolve_path fails because canonicalize needs the file to exist
                let config = config::load_config();
                let mount = match config.mounts.iter().find(|m| m.share_name() == req.domain && m.enabled) {
                    Some(m) => m,
                    None => {
                        return FileOpsResponse::Error(FileOpsErrorResp {
                            request_id: req.request_id,
                            message: format!("No mount for domain '{}': {}", req.domain, e),
                        });
                    }
                };
                let base = if mount.is_sync_mode() {
                    PathBuf::from("/Volumes").join(&mount.share_name())
                } else {
                    PathBuf::from(mount.mount_path())
                };
                base.join(&req.relative_path)
            }
        };

        if req.is_dir {
            log::info!("[FileOps] CreateDir: {}", dest_path.display());
            if let Err(e) = std::fs::create_dir_all(&dest_path) {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("Failed to create directory: {}", e),
                });
            }
        } else {
            log::info!("[FileOps] WriteFile: {} → {}", req.source_path, dest_path.display());

            // Ensure parent directory exists
            if let Some(parent) = dest_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if let Err(e) = std::fs::copy(&req.source_path, &dest_path) {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("Failed to write file: {}", e),
                });
            }

            // Clean up the source temp file
            let _ = std::fs::remove_file(&req.source_path);
        }

        let meta = std::fs::metadata(&dest_path).ok();
        FileOpsResponse::WriteOk(WriteOkResp {
            request_id: req.request_id,
            size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0),
        })
    }

    fn handle_delete_item(req: DeleteItemReq) -> FileOpsResponse {
        let target_path = match Self::resolve_path(&req.domain, &req.relative_path) {
            Ok(p) => p,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: e,
                });
            }
        };

        log::info!("[FileOps] Delete: {}", target_path.display());

        let meta = match std::fs::metadata(&target_path) {
            Ok(m) => m,
            Err(e) => {
                return FileOpsResponse::Error(FileOpsErrorResp {
                    request_id: req.request_id,
                    message: format!("Item not found: {}", e),
                });
            }
        };

        let result = if meta.is_dir() {
            std::fs::remove_dir_all(&target_path)
        } else {
            std::fs::remove_file(&target_path)
        };

        if let Err(e) = result {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: format!("Failed to delete: {}", e),
            });
        }

        FileOpsResponse::DeleteOk(DeleteOkResp {
            request_id: req.request_id,
        })
    }
}
