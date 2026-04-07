use crate::messages::{AgentToUfb, UfbToAgent};
use std::io::{self, Read, Write};
use std::sync::Arc;
use tokio::sync::mpsc;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile, FlushFileBuffers, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PeekNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

const PIPE_NAME: &str = r"\\.\pipe\MediaMountAgent";

/// A Send-safe wrapper around a raw HANDLE.
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

struct PipeHandle {
    h: SendHandle,
    owns: bool,
}

impl PipeHandle {
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
        unsafe { let _ = FlushFileBuffers(self.raw()); }
        Ok(())
    }
}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if self.owns && !self.raw().is_invalid() {
            unsafe { let _ = CloseHandle(self.raw()); }
        }
    }
}

/// IPC server using a single I/O thread per client to avoid the Windows
/// synchronous pipe deadlock (concurrent ReadFile/WriteFile on the same handle).
///
/// Outgoing messages are sent via a `std::sync::mpsc` channel so the blocking
/// I/O thread can `try_recv` without async.
pub struct IpcServer {
    pub command_rx: mpsc::Receiver<UfbToAgent>,
    /// Outgoing messages — the I/O thread drains this via try_recv.
    outgoing_tx: Arc<std::sync::Mutex<std::sync::mpsc::Sender<AgentToUfb>>>,
}

impl IpcServer {
    pub fn start() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<UfbToAgent>(64);

        // Outgoing channel: std::sync so the blocking I/O thread can try_recv.
        // The sender is wrapped in Arc<Mutex> so we can swap it per client session.
        let (out_tx, out_rx) = std::sync::mpsc::channel::<AgentToUfb>();
        let shared_out_tx = Arc::new(std::sync::Mutex::new(out_tx));

        let shared_out_tx_clone = shared_out_tx.clone();
        tokio::spawn(async move {
            loop {
                // Create pipe instance
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

                // Wait for client connection
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

                // Create a fresh outgoing channel for this client session.
                let (new_tx, new_rx) = std::sync::mpsc::channel::<AgentToUfb>();
                {
                    let mut lock = shared_out_tx_clone.lock().unwrap();
                    *lock = new_tx;
                }

                // Single blocking I/O thread per client
                let cmd_tx_clone = cmd_tx.clone();
                let io_done = tokio::task::spawn_blocking(move || {
                    let mut pipe = PipeHandle::alias(send_handle);

                    loop {
                        // 1. Write any pending outgoing messages
                        while let Ok(msg) = new_rx.try_recv() {
                            if let Err(e) = super::send_message(&mut pipe, &msg) {
                                log::warn!("Failed to send to UFB client: {}", e);
                                return;
                            }
                            let _ = pipe.flush();
                        }

                        // 2. Peek for incoming data
                        let mut available = 0u32;
                        let peek_ok = unsafe {
                            PeekNamedPipe(pipe.raw(), None, 0, None, Some(&mut available), None)
                        };
                        if peek_ok.is_err() {
                            log::info!("UFB client disconnected (peek)");
                            return;
                        }

                        if available > 0 {
                            match super::recv_message::<_, UfbToAgent>(&mut pipe) {
                                Ok(msg) => {
                                    if cmd_tx_clone.blocking_send(msg).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => {
                                    log::info!("UFB client disconnected (read)");
                                    return;
                                }
                            }
                        } else {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                });

                // Wait for this client session to end
                let _ = io_done.await;
                unsafe { let _ = CloseHandle(send_handle.to_handle()); }
            }
        });

        Self {
            command_rx: cmd_rx,
            outgoing_tx: shared_out_tx,
        }
    }

    pub async fn send(&self, msg: AgentToUfb) -> Result<(), String> {
        let lock = self.outgoing_tx.lock().unwrap();
        lock.send(msg).map_err(|e| format!("Failed to queue response: {}", e))
    }
}

fn create_pipe() -> io::Result<HANDLE> {
    let pipe_name: Vec<u16> = format!("{}\0", PIPE_NAME).encode_utf16().collect();
    let pipe_access = FILE_FLAGS_AND_ATTRIBUTES(0x00000003); // PIPE_ACCESS_DUPLEX

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
    unsafe { let _ = ConnectNamedPipe(handle, None); }
    Ok(handle)
}
