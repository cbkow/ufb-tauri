use crate::messages::{AgentToUfb, UfbToAgent};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Resolve the socket path for the IPC server.
///
/// On macOS we put it inside the shared app group container so the sandboxed
/// FileProvider and FinderSync extensions can reach it — sandboxed processes
/// can't open sockets in `/tmp`. The tray is not sandboxed but uses the same
/// path for consistency.
///
/// Linux / other platforms keep the XDG_RUNTIME_DIR → /tmp fallback.
fn socket_path() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let dir = std::path::PathBuf::from(home).join(
                "Library/Group Containers/5Z4S9VHV56.group.com.unionfiles.mediamount-tray",
            );
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("mediamount-agent.sock");
        }
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = std::path::PathBuf::from(runtime_dir).join("ufb");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("mediamount-agent.sock")
    } else {
        std::path::PathBuf::from("/tmp/ufb-mediamount-agent.sock")
    }
}

/// Get the socket path (for use by the client side too).
pub fn get_socket_path() -> std::path::PathBuf {
    socket_path()
}

/// IPC server that listens for connections on a Unix domain socket.
/// Supports multiple concurrent clients (e.g. UFB + Swift tray app).
pub struct IpcServer {
    pub command_rx: mpsc::Receiver<UfbToAgent>,
    response_tx: mpsc::Sender<AgentToUfb>,
    _cancel_tx: tokio::sync::oneshot::Sender<()>,
}

impl IpcServer {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<UfbToAgent>(64);
        let (resp_tx, mut resp_rx) = mpsc::channel::<AgentToUfb>(64);
        let (cancel_tx, _cancel_rx) = tokio::sync::oneshot::channel::<()>();

        // Shared list of connected client write streams
        let writers: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));

        // Response writer task — broadcasts to all connected clients
        let writer_handle = writers.clone();
        tokio::spawn(async move {
            while let Some(msg) = resp_rx.recv().await {
                let mut lock = writer_handle.lock().await;
                let mut failed = Vec::new();
                for (i, stream) in lock.iter_mut().enumerate() {
                    if let Err(e) = super::send_message(stream, &msg) {
                        log::debug!("Failed to send to client {}: {}", i, e);
                        failed.push(i);
                    }
                }
                // Remove disconnected clients (reverse order to preserve indices)
                for i in failed.into_iter().rev() {
                    log::info!("Removing disconnected client {}", i);
                    lock.remove(i);
                }
            }
        });

        // Connection listener task
        let listener_handle = writers;
        tokio::spawn(async move {
            let sock_path = socket_path();

            // Clean up stale socket
            if sock_path.exists() {
                let _ = std::fs::remove_file(&sock_path);
            }

            let listener = match UnixListener::bind(&sock_path) {
                Ok(l) => {
                    log::info!("IPC listening on {}", sock_path.display());
                    l
                }
                Err(e) => {
                    log::error!("Failed to bind Unix socket at {}: {}", sock_path.display(), e);
                    return;
                }
            };

            // Set permissions so only current user can connect
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o700));
            }

            loop {
                // Accept connection (blocking)
                let (stream, _addr) = match tokio::task::spawn_blocking({
                    let listener_fd = listener.try_clone().expect("Failed to clone listener");
                    move || listener_fd.accept()
                })
                .await
                {
                    Ok(Ok(pair)) => pair,
                    Ok(Err(e)) => {
                        log::error!("Accept failed: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                    Err(e) => {
                        log::error!("Accept task panicked: {}", e);
                        break;
                    }
                };

                log::info!("IPC client connected");

                // Clone stream for writing
                let write_stream = match stream.try_clone() {
                    Ok(s) => s,
                    Err(e) => {
                        log::error!("Failed to clone stream: {}", e);
                        continue;
                    }
                };

                // Add write half to client list
                {
                    let mut lock = listener_handle.lock().await;
                    lock.push(write_stream);
                }

                // Read commands in blocking thread
                let cmd_tx_clone = cmd_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let mut reader = stream;
                    loop {
                        match super::recv_message::<_, UfbToAgent>(&mut reader) {
                            Ok(msg) => {
                                if cmd_tx_clone.blocking_send(msg).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                log::info!("IPC client disconnected");
                                break;
                            }
                        }
                    }
                });
            }
        });

        Self {
            command_rx: cmd_rx,
            response_tx: resp_tx,
            _cancel_tx: cancel_tx,
        }
    }

    pub async fn send(&self, msg: AgentToUfb) -> Result<(), String> {
        self.response_tx
            .send(msg)
            .await
            .map_err(|e| format!("Failed to queue response: {}", e))
    }
}
