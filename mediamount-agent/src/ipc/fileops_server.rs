use crate::config::{self, MountConfig};
use crate::messages::*;
use std::collections::HashMap;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "macos")]
use crate::sync::MacosCache;

/// App group container directory for the FileProvider extension.
const APP_GROUP_ID: &str = "5Z4S9VHV56.group.com.unionfiles.mediamount-tray";

fn app_group_container() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join("Library/Group Containers")
            .join(APP_GROUP_ID)
    } else {
        PathBuf::from("/tmp/ufb-fileprovider")
    }
}

fn socket_path() -> PathBuf {
    app_group_container().join("fp.sock")
}

fn temp_dir() -> PathBuf {
    app_group_container().join("temp")
}

/// Shared state for the file operations server.
struct ServerState {
    /// Per-domain cache databases.
    caches: std::sync::RwLock<HashMap<String, MacosCache>>,
}

/// File operations IPC server for the FileProvider extension.
pub struct FileOpsServer;

impl FileOpsServer {
    pub fn start() {
        tokio::spawn(async move {
            if let Err(e) = Self::run().await {
                log::error!("[FileOps] Server failed: {}", e);
            }
        });
    }

    async fn run() -> Result<(), String> {
        let sock_path = socket_path();
        let container = app_group_container();

        std::fs::create_dir_all(&container)
            .map_err(|e| format!("Failed to create app group container: {}", e))?;
        std::fs::create_dir_all(&temp_dir())
            .map_err(|e| format!("Failed to create temp dir: {}", e))?;

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

        // Initialize caches for sync-enabled mounts
        let mut caches = HashMap::new();
        let config = config::load_config();
        for mount in &config.mounts {
            if mount.enabled && mount.is_sync_mode() {
                let domain = mount.share_name();
                let nas_root = PathBuf::from("/Volumes").join(&domain);
                match MacosCache::open(&domain, nas_root, mount.sync_cache_limit_bytes) {
                    Ok(cache) => {
                        log::info!("[FileOps] Cache opened for domain: {}", domain);
                        caches.insert(domain, cache);
                    }
                    Err(e) => {
                        log::error!("[FileOps] Failed to open cache for {}: {}", domain, e);
                    }
                }
            }
        }

        let state = Arc::new(ServerState { caches: std::sync::RwLock::new(caches) });

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

            let state = Arc::clone(&state);
            tokio::task::spawn_blocking(move || {
                handle_client(stream, &state);
            });
        }

        Ok(())
    }
}

fn handle_client(stream: UnixStream, state: &ServerState) {
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

        let response = handle_request(req, state);

        if let Err(e) = super::send_message(&mut writer, &response) {
            log::warn!("[FileOps] Failed to send response: {}", e);
            break;
        }
    }
}

fn handle_request(req: FileOpsRequest, state: &ServerState) -> FileOpsResponse {
    match req {
        FileOpsRequest::Ping => FileOpsResponse::Pong,
        FileOpsRequest::ListDir(r) => handle_list_dir(r, state),
        FileOpsRequest::Stat(r) => handle_stat(r),
        FileOpsRequest::ReadFile(r) => handle_read_file(r, state),
        FileOpsRequest::WriteFile(r) => handle_write_file(r, state),
        FileOpsRequest::DeleteItem(r) => handle_delete_item(r, state),
        FileOpsRequest::RenameItem(r) => handle_rename_item(r, state),
        FileOpsRequest::RecordEnumeration(r) => handle_record_enumeration(r, state),
        FileOpsRequest::ClearCache(r) => handle_clear_cache(r, state),
        FileOpsRequest::GetChanges(r) => handle_get_changes(r, state),
    }
}

/// Ensure a cache exists for a domain (opens on demand for mounts added at runtime).
fn ensure_cache(state: &ServerState, domain: &str) {
    // Quick check with read lock
    if state.caches.read().unwrap().contains_key(domain) {
        return;
    }
    // Need to open — use write lock
    let config = config::load_config();
    if let Some(mount) = config.mounts.iter().find(|m| m.share_name() == domain && m.enabled && m.is_sync_mode()) {
        let nas_root = PathBuf::from("/Volumes").join(&mount.share_name());
        match MacosCache::open(domain, nas_root, mount.sync_cache_limit_bytes) {
            Ok(cache) => {
                log::info!("[FileOps] Cache opened on demand for domain: {}", domain);
                state.caches.write().unwrap().insert(domain.to_string(), cache);
            }
            Err(e) => {
                log::error!("[FileOps] Failed to open cache for {}: {}", domain, e);
            }
        }
    }
}

/// Helper to access cache with read lock.
fn with_cache<F, R>(state: &ServerState, domain: &str, f: F) -> Option<R>
where F: FnOnce(&MacosCache) -> R {
    ensure_cache(state, domain);
    let caches = state.caches.read().unwrap();
    caches.get(domain).map(f)
}

// ── Path resolution ──

fn resolve_path(domain: &str, relative_path: &str) -> Result<PathBuf, String> {
    let config = config::load_config();
    let mount = config
        .mounts
        .iter()
        .find(|m| m.share_name() == domain && m.enabled)
        .ok_or_else(|| format!("No enabled mount found for domain '{}'", domain))?;

    // macOS: all mounts go through FileProvider, resolve to /Volumes/{share} directly
    // (not through the symlink which points to CloudStorage — circular)
    #[cfg(target_os = "macos")]
    let base = {
        let volumes_path = PathBuf::from("/Volumes").join(&mount.share_name());
        if !volumes_path.exists() {
            return Err(format!("SMB mount not found at {}", volumes_path.display()));
        }
        volumes_path
    };
    #[cfg(not(target_os = "macos"))]
    let base = PathBuf::from(mount.mount_path());

    let base_canonical = base
        .canonicalize()
        .map_err(|e| format!("Base path resolution failed for {}: {}", base.display(), e))?;

    let full_path = if relative_path.is_empty() {
        base_canonical.clone()
    } else {
        base_canonical.join(relative_path)
    };

    let canonical = full_path
        .canonicalize()
        .map_err(|e| format!("Path resolution failed for {}: {}", full_path.display(), e))?;

    if !canonical.starts_with(&base_canonical) {
        return Err(format!("Path traversal detected: {}", relative_path));
    }

    Ok(canonical)
}

/// Resolve for new files that don't exist yet (can't canonicalize).
fn resolve_path_for_write(domain: &str, relative_path: &str) -> Result<PathBuf, String> {
    match resolve_path(domain, relative_path) {
        Ok(p) => Ok(p),
        Err(_) => {
            let config = config::load_config();
            let mount = config.mounts.iter()
                .find(|m| m.share_name() == domain && m.enabled)
                .ok_or_else(|| format!("No mount for domain '{}'", domain))?;
            #[cfg(target_os = "macos")]
            let base = PathBuf::from("/Volumes").join(&mount.share_name());
            #[cfg(not(target_os = "macos"))]
            let base = PathBuf::from(mount.mount_path());
            Ok(base.join(relative_path))
        }
    }
}

// ── Handlers ──

fn handle_list_dir(req: ListDirReq, state: &ServerState) -> FileOpsResponse {
    let dir_path = match resolve_path(&req.domain, &req.relative_path) {
        Ok(p) => p,
        Err(e) => {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: e,
            });
        }
    };

    let entries = match std::fs::read_dir(&dir_path) {
        Ok(rd) => rd,
        Err(e) => {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: format!("Failed to read directory: {}", e),
            });
        }
    };

    let mut result = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
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
            modified: meta.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64()).unwrap_or(0.0),
            created: meta.created().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64()).unwrap_or(0.0),
        });
    }

    // Record in cache if available
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        cache.record_enumeration(&req.relative_path, &result);
    }

    FileOpsResponse::DirListing(DirListingResp {
        request_id: req.request_id,
        entries: result,
    })
}

fn handle_stat(req: StatReq) -> FileOpsResponse {
    let file_path = match resolve_path(&req.domain, &req.relative_path) {
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
                message: format!("Stat failed: {}", e),
            });
        }
    };

    let name = file_path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    FileOpsResponse::FileStat(FileStatResp {
        request_id: req.request_id,
        name,
        is_dir: meta.is_dir(),
        size: meta.len(),
        modified: meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
        created: meta.created().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
    })
}

fn handle_read_file(req: ReadFileReq, state: &ServerState) -> FileOpsResponse {
    let source_path = match resolve_path(&req.domain, &req.relative_path) {
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

    log::info!("[FileOps] ReadFile: {} → {} ({} bytes)", source_path.display(), temp_path.display(), meta.len());

    if let Err(e) = std::fs::copy(&source_path, &temp_path) {
        return FileOpsResponse::Error(FileOpsErrorResp {
            request_id: req.request_id,
            message: format!("Failed to copy file: {}", e),
        });
    }

    // Record hydration for cache eviction tracking
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        cache.record_hydration(&req.relative_path, meta.len());
    }

    FileOpsResponse::FileReady(FileReadyResp {
        request_id: req.request_id,
        temp_path: temp_path.to_string_lossy().to_string(),
        size: meta.len(),
        modified: meta.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
    })
}

fn handle_write_file(req: WriteFileReq, state: &ServerState) -> FileOpsResponse {
    let dest_path = match resolve_path_for_write(&req.domain, &req.relative_path) {
        Ok(p) => p,
        Err(e) => {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: e,
            });
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
        if let Some(parent) = dest_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::copy(&req.source_path, &dest_path) {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: format!("Failed to write file: {}", e),
            });
        }
        let _ = std::fs::remove_file(&req.source_path);
    }

    let meta = std::fs::metadata(&dest_path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let modified = meta.as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    // Update cache
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        let name = Path::new(&req.relative_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        cache.record_known_file(&req.relative_path, &DirEntry {
            name,
            is_dir: req.is_dir,
            size,
            modified,
            created: modified,
        });
    }

    FileOpsResponse::WriteOk(WriteOkResp {
        request_id: req.request_id,
        size,
        modified,
    })
}

fn handle_delete_item(req: DeleteItemReq, state: &ServerState) -> FileOpsResponse {
    let target_path = match resolve_path(&req.domain, &req.relative_path) {
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

    // Update cache
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        cache.remove_known_file(&req.relative_path);
    }

    FileOpsResponse::DeleteOk(DeleteOkResp {
        request_id: req.request_id,
    })
}

fn handle_rename_item(req: RenameItemReq, state: &ServerState) -> FileOpsResponse {
    let old_path = match resolve_path(&req.domain, &req.old_path) {
        Ok(p) => p,
        Err(e) => {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: e,
            });
        }
    };

    let new_path = match resolve_path_for_write(&req.domain, &req.new_path) {
        Ok(p) => p,
        Err(e) => {
            return FileOpsResponse::Error(FileOpsErrorResp {
                request_id: req.request_id,
                message: e,
            });
        }
    };

    log::info!("[FileOps] Rename: {} → {}", old_path.display(), new_path.display());

    if let Err(e) = std::fs::rename(&old_path, &new_path) {
        return FileOpsResponse::Error(FileOpsErrorResp {
            request_id: req.request_id,
            message: format!("Rename failed: {}", e),
        });
    }

    // Update cache
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        cache.remove_known_file(&req.old_path);
        let meta = std::fs::metadata(&new_path).ok();
        let name = new_path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta.as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        cache.record_known_file(&req.new_path, &DirEntry {
            name,
            is_dir,
            size,
            modified,
            created: modified,
        });
    }

    let meta = std::fs::metadata(&new_path).ok();
    FileOpsResponse::RenameOk(RenameOkResp {
        request_id: req.request_id,
        new_path: req.new_path,
        size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        modified: meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0),
    })
}

fn handle_record_enumeration(req: RecordEnumerationReq, state: &ServerState) -> FileOpsResponse {
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        cache.record_enumeration(&req.relative_path, &req.entries);
    }
    FileOpsResponse::RecordOk(RecordOkResp {
        request_id: req.request_id,
    })
}

fn handle_clear_cache(req: ClearCacheReq, state: &ServerState) -> FileOpsResponse {
    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        let (count, bytes) = cache.clear_all_hydrated();
        log::info!("[FileOps] ClearCache {}: {} files, {:.1} MB", req.domain, count, bytes as f64 / 1_048_576.0);
    }
    // Evictions will be delivered in the next getChanges call
    FileOpsResponse::RecordOk(RecordOkResp {
        request_id: req.request_id,
    })
}

fn handle_get_changes(req: GetChangesReq, state: &ServerState) -> FileOpsResponse {
    let since: f64 = req.since_anchor.parse().unwrap_or(0.0);

    ensure_cache(state, &req.domain); if let Some(cache) = state.caches.read().unwrap().get(&req.domain) {
        let result = cache.get_changes_since(since);

        let updated: Vec<ChangedEntry> = result.updated.into_iter().map(|e| {
            ChangedEntry {
                relative_path: e.relative_path,
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
                modified: e.modified,
                created: e.created,
            }
        }).collect();

        // Drain pending evictions
        let evict = cache.drain_pending_evictions();

        FileOpsResponse::Changes(ChangesResp {
            request_id: req.request_id,
            updated,
            deleted: result.deleted,
            evict,
            new_anchor: format!("{}", result.new_anchor),
        })
    } else {
        // Passthrough mount (no cache DB) — do a fresh readdir of root
        // and return everything as "updated". The system diffs against its cache.
        let config = config::load_config();
        let mount = config.mounts.iter()
            .find(|m| m.share_name() == req.domain && m.enabled);

        let updated = if let Some(mount) = mount {
            let nas_root = PathBuf::from("/Volumes").join(&mount.share_name());
            std::fs::read_dir(&nas_root)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') || name.starts_with('@') || name.starts_with('#') {
                        return None;
                    }
                    let meta = entry.metadata().ok()?;
                    Some(ChangedEntry {
                        relative_path: name.clone(),
                        name,
                        is_dir: meta.is_dir(),
                        size: meta.len(),
                        modified: meta.modified().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
                        created: meta.created().ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs_f64()).unwrap_or(0.0),
                    })
                })
                .collect()
        } else {
            vec![]
        };

        FileOpsResponse::Changes(ChangesResp {
            request_id: req.request_id,
            updated,
            deleted: vec![],
            evict: vec![],
            new_anchor: format!("{}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()),
        })
    }
}
