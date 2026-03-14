use crate::messages::{AgentToUfb, UfbToAgent};
use std::io::{self, Read, Write};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

const PIPE_NAME: &str = r"\\.\pipe\MediaMountAgent";

/// A Send-safe wrapper around a raw HANDLE, stored as a usize for Send safety.
#[derive(Clone, Copy)]
struct SendHandle(usize);

impl SendHandle {
    fn from_handle(h: HANDLE) -> Self {
        Self(h.0 as usize)
    }
    fn to_handle(self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }
}

/// Named pipe handle with Read/Write implementations.
struct PipeHandle {
    h: SendHandle,
    owns: bool, // whether Drop should close the handle
}

impl PipeHandle {
    #[allow(dead_code)]
    fn new(handle: HANDLE) -> Self {
        Self {
            h: SendHandle::from_handle(handle),
            owns: true,
        }
    }

    /// Create a non-owning alias (won't close on drop).
    fn alias(handle: SendHandle) -> Self {
        Self { h: handle, owns: false }
    }

    fn raw(&self) -> HANDLE {
        self.h.to_handle()
    }
}

impl Read for PipeHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_read = 0u32;
        unsafe {
            ReadFile(self.raw(), Some(buf), Some(&mut bytes_read), None)
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
        }
        if bytes_read == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"));
        }
        Ok(bytes_read as usize)
    }
}

impl Write for PipeHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut bytes_written = 0u32;
        unsafe {
            WriteFile(self.raw(), Some(buf), Some(&mut bytes_written), None)
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e.to_string()))?;
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if self.owns && !self.raw().is_invalid() {
            unsafe {
                let _ = CloseHandle(self.raw());
            }
        }
    }
}

/// IPC server that listens for UFB connections on a named pipe.
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

        // Shared handle for current client connection
        let active_handle: Arc<Mutex<Option<SendHandle>>> = Arc::new(Mutex::new(None));

        // Response writer task
        let writer_handle = active_handle.clone();
        tokio::spawn(async move {
            while let Some(msg) = resp_rx.recv().await {
                let mut lock = writer_handle.lock().await;
                if let Some(h) = *lock {
                    let mut pipe = PipeHandle::alias(h);
                    if let Err(e) = super::send_message(&mut pipe, &msg) {
                        log::warn!("Failed to send to UFB client: {}", e);
                        *lock = None;
                    }
                }
            }
        });

        // Connection listener task
        let listener_handle = active_handle;
        tokio::spawn(async move {
            loop {
                // Create pipe (blocking)
                let send_handle = match tokio::task::spawn_blocking(|| {
                    create_pipe().map(SendHandle::from_handle)
                }).await {
                    Ok(Ok(h)) => h,
                    Ok(Err(e)) => {
                        log::error!("Failed to create pipe: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                    Err(e) => {
                        log::error!("Pipe task panicked: {}", e);
                        break;
                    }
                };

                log::info!("Waiting for UFB client on {}", PIPE_NAME);

                // Wait for client (blocking)
                let sh = send_handle;
                let send_handle = match tokio::task::spawn_blocking(move || {
                    wait_connect(sh.to_handle()).map(SendHandle::from_handle)
                }).await {
                    Ok(Ok(h)) => h,
                    Ok(Err(e)) => {
                        log::error!("ConnectNamedPipe failed: {}", e);
                        continue;
                    }
                    Err(e) => {
                        log::error!("Connect task panicked: {}", e);
                        break;
                    }
                };

                log::info!("UFB client connected");

                // Store for writer
                {
                    let mut lock = listener_handle.lock().await;
                    *lock = Some(send_handle);
                }

                // Read commands in blocking thread
                let cmd_tx_clone = cmd_tx.clone();
                let _listener_ref = listener_handle.clone();
                tokio::task::spawn_blocking(move || {
                    let mut pipe = PipeHandle::alias(send_handle);
                    loop {
                        match super::recv_message::<_, UfbToAgent>(&mut pipe) {
                            Ok(msg) => {
                                if cmd_tx_clone.blocking_send(msg).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                log::info!("UFB client disconnected");
                                break;
                            }
                        }
                    }
                    // Clean up: close handle
                    unsafe {
                        let _ = CloseHandle(send_handle.to_handle());
                    }
                    // We can't async lock from blocking context, but writer will detect broken pipe
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

fn create_pipe() -> io::Result<HANDLE> {
    let pipe_name: Vec<u16> = format!("{}\0", PIPE_NAME).encode_utf16().collect();

    // PIPE_ACCESS_DUPLEX = 0x00000003
    let pipe_access = FILE_FLAGS_AND_ATTRIBUTES(0x00000003);

    let handle = unsafe {
        CreateNamedPipeW(
            windows::core::PCWSTR(pipe_name.as_ptr()),
            pipe_access,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            65536,
            65536,
            0,
            None,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(handle)
}

fn wait_connect(handle: HANDLE) -> io::Result<HANDLE> {
    unsafe {
        let _ = ConnectNamedPipe(handle, None);
    }
    Ok(handle)
}
