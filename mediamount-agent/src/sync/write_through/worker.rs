/// Upload worker: blocking thread that writes local files to the NAS
/// via SMB, handles conflict detection, and converts to placeholder.
///
/// Flow per job:
/// 1. Open local file
/// 2. Write to NAS temp file (.filename.~sync.HOSTNAME) in 4MB chunks
/// 3. Check cancel between chunks
/// 4. Conflict detection: compare pre/post mtime+size of NAS target
/// 5. Rename temp to final (or save as .conflict file)

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use super::EchoSuppressor;

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB

/// A job for the upload worker.
pub struct UploadJob {
    pub local_path: PathBuf,
    pub nas_path: PathBuf,
    pub cancel_rx: oneshot::Receiver<()>,
}

/// Result from the upload worker.
#[derive(Debug)]
pub enum UploadResult {
    Success {
        local_path: PathBuf,
        nas_path: PathBuf,
    },
    Cancelled {
        local_path: PathBuf,
    },
    Failed {
        local_path: PathBuf,
        error: String,
    },
}

/// Number of concurrent upload worker threads.
/// 3-4 saturates typical NAS disk I/O without overwhelming spinning disks.
pub const WORKER_COUNT: usize = 3;

/// Start N upload workers on dedicated threads sharing a crossbeam channel.
pub fn start_pool(
    rx: crossbeam_channel::Receiver<UploadJob>,
    result_tx: mpsc::Sender<UploadResult>,
    echo: Arc<EchoSuppressor>,
) -> Vec<std::thread::JoinHandle<()>> {
    (0..WORKER_COUNT)
        .map(|i| {
            let rx = rx.clone();
            let result_tx = result_tx.clone();
            let echo = echo.clone();
            std::thread::Builder::new()
                .name(format!("upload-worker-{}", i))
                .spawn(move || {
                    for job in rx {
                        let result = process_upload(job, &echo);
                        if result_tx.blocking_send(result).is_err() {
                            break;
                        }
                    }
                    log::info!("[write-through] Upload worker {} exiting", i);
                })
                .expect("Failed to spawn upload worker thread")
        })
        .collect()
}

fn process_upload(mut job: UploadJob, echo: &EchoSuppressor) -> UploadResult {
    let local = &job.local_path;
    let nas_target = &job.nas_path;

    let hostname = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".into());
    let file_name = nas_target
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let temp_name = format!(".{}.~sync.{}", file_name, hostname);
    let nas_temp = nas_target.with_file_name(&temp_name);

    log::info!(
        "[write-through] Uploading {:?} -> {:?}",
        local,
        nas_target
    );

    // Ensure parent directory exists on NAS
    if let Some(parent) = nas_target.parent() {
        if !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                return UploadResult::Failed {
                    local_path: local.clone(),
                    error: format!("Failed to create NAS directory: {}", e),
                };
            }
        }
    }

    // Open local file
    let mut src = match fs::File::open(local) {
        Ok(f) => f,
        Err(e) => {
            return UploadResult::Failed {
                local_path: local.clone(),
                error: format!("Failed to open local file: {}", e),
            }
        }
    };

    // Record pre-upload state of NAS target (for conflict detection)
    let pre_stat = fs::metadata(nas_target)
        .ok()
        .map(|m| (m.len(), m.modified().ok()));

    // Create temp file on NAS
    let mut dst = match fs::File::create(&nas_temp) {
        Ok(f) => f,
        Err(e) => {
            return UploadResult::Failed {
                local_path: local.clone(),
                error: format!("Failed to create NAS temp file: {}", e),
            }
        }
    };

    // Stream in 4MB chunks, checking cancel between each
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut total: u64 = 0;

    loop {
        // Check cancel — only explicit Ok(()) means cancel.
        // Closed means the coordinator dropped the sender (e.g., startup
        // recovery jobs) — NOT a cancellation.
        if let Ok(()) = job.cancel_rx.try_recv() {
            drop(dst);
            let _ = fs::remove_file(&nas_temp);
            return UploadResult::Cancelled {
                local_path: local.clone(),
            };
        }

        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                drop(dst);
                let _ = fs::remove_file(&nas_temp);
                return UploadResult::Failed {
                    local_path: local.clone(),
                    error: format!("Read error at {} bytes: {}", total, e),
                };
            }
        };

        if let Err(e) = dst.write_all(&buf[..n]) {
            drop(dst);
            let _ = fs::remove_file(&nas_temp);
            return UploadResult::Failed {
                local_path: local.clone(),
                error: format!("Write error at {} bytes: {}", total, e),
            };
        }

        total += n as u64;
    }

    drop(dst);
    drop(src);

    // Conflict detection: did the NAS target change during upload?
    if let Some((pre_size, pre_mtime)) = pre_stat {
        if let Ok(post_meta) = fs::metadata(nas_target) {
            let size_changed = pre_size != post_meta.len();
            let mtime_changed = match (pre_mtime, post_meta.modified().ok()) {
                (Some(pre), Some(post)) => {
                    let diff = if post > pre {
                        post.duration_since(pre).unwrap_or_default()
                    } else {
                        pre.duration_since(post).unwrap_or_default()
                    };
                    diff.as_secs() > 2 // Synology mtime granularity tolerance
                }
                _ => false,
            };

            if size_changed || mtime_changed {
                let conflict_name = format!(
                    "{}.conflict.{}.{}",
                    file_name,
                    hostname,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                );
                let conflict_path = nas_target.with_file_name(&conflict_name);
                log::warn!(
                    "[write-through] Conflict detected: {:?}, saving as {:?}",
                    nas_target,
                    conflict_path
                );
                let _ = fs::rename(&nas_temp, &conflict_path);
                return UploadResult::Success {
                    local_path: local.clone(),
                    nas_path: conflict_path,
                };
            }
        }
    }

    // No conflict — suppress echo and rename temp to final
    echo.suppress(nas_target.clone());

    if let Err(e) = fs::rename(&nas_temp, nas_target) {
        let _ = fs::remove_file(&nas_temp);
        return UploadResult::Failed {
            local_path: local.clone(),
            error: format!("Rename to final failed: {}", e),
        };
    }

    log::info!(
        "[write-through] Uploaded {:?} ({} bytes)",
        nas_target,
        total
    );

    UploadResult::Success {
        local_path: local.clone(),
        nas_path: nas_target.clone(),
    }
}
