pub mod cache;
pub mod parser;

use crate::config::MountConfig;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Signals emitted by the rclone manager based on log output or process exit.
#[derive(Debug)]
pub enum RcloneSignal {
    Started,
    Fatal(String),
    Exited { code: Option<i32> },
}

/// Manages the lifecycle of a single rclone process.
pub struct RcloneManager {
    child: Option<Child>,
    rc_port: u16,
}

impl RcloneManager {
    /// Resolve the rclone binary path.
    /// Priority: 1) next to agent exe, 2) next to UFB exe (shared build output), 3) PATH
    pub fn resolve_rclone_path() -> PathBuf {
        let ext = if cfg!(target_os = "windows") { ".exe" } else { "" };
        let rclone_name = format!("rclone{}", ext);

        // Check next to our own executable
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let sidecar = exe_dir.join(&rclone_name);
                if sidecar.exists() {
                    log::info!("Found rclone sidecar at {}", sidecar.display());
                    return sidecar;
                }
            }
        }

        // Fallback to PATH
        log::info!("rclone not found next to exe, falling back to PATH");
        PathBuf::from(rclone_name)
    }

    /// Verify rclone binary exists and is executable.
    pub fn verify_rclone() -> Result<PathBuf, String> {
        let path = Self::resolve_rclone_path();

        // If it's an absolute path, check existence directly
        if path.is_absolute() {
            if path.exists() {
                return Ok(path);
            }
            return Err(format!("rclone not found at {}", path.display()));
        }

        // For PATH-based lookup, try running `rclone version`
        match std::process::Command::new(&path)
            .arg("version")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                let first_line = version.lines().next().unwrap_or("unknown");
                log::info!("Found rclone in PATH: {}", first_line);
                Ok(path)
            }
            Ok(_) => Err("rclone found but returned error".into()),
            Err(e) => Err(format!("rclone not found in PATH: {}", e)),
        }
    }

    /// Check if WinFSP is installed (Windows only).
    #[cfg(windows)]
    pub fn check_winfsp() -> bool {
        // Check for winfsp-x64.dll in Program Files
        let program_files = std::env::var("ProgramFiles").unwrap_or_default();
        let dll_path = std::path::Path::new(&program_files).join("WinFsp/bin/winfsp-x64.dll");
        if dll_path.exists() {
            log::info!("WinFSP found at {}", dll_path.display());
            return true;
        }

        // Also check Program Files (x86) for 32-bit installs
        let program_files_x86 = std::env::var("ProgramFiles(x86)").unwrap_or_default();
        let dll_path_x86 =
            std::path::Path::new(&program_files_x86).join("WinFsp/bin/winfsp-x64.dll");
        if dll_path_x86.exists() {
            log::info!("WinFSP found at {}", dll_path_x86.display());
            return true;
        }

        log::error!("WinFSP not found — install from https://winfsp.dev/");
        false
    }

    #[cfg(not(windows))]
    pub fn check_winfsp() -> bool {
        true
    }

    /// Check if a TCP port is available.
    fn is_port_available(port: u16) -> bool {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    /// Find an available RC port starting from the given port.
    pub fn find_available_port(start: u16) -> u16 {
        for port in start..start + 100 {
            if Self::is_port_available(port) {
                return port;
            }
        }
        start // Fall back, rclone will report the conflict
    }

    /// Spawn rclone with the given mount configuration. Returns a manager + signal receiver.
    pub async fn spawn(
        config: &MountConfig,
        rc_port: u16,
    ) -> Result<(Self, mpsc::Receiver<RcloneSignal>), String> {
        let rclone_path = Self::verify_rclone()?;

        // Ensure cache directory exists
        let cache_dir = std::path::Path::new(&config.cache_dir_path);
        if !cache_dir.exists() {
            std::fs::create_dir_all(cache_dir)
                .map_err(|e| format!("Failed to create cache dir {}: {}", config.cache_dir_path, e))?;
        }

        // Build the rclone remote spec.
        // rclone can mount SMB shares directly using the :smb: backend inline:
        //   rclone mount :smb:path R: --smb-host=nas --smb-user=... --smb-pass=...
        // Or for a UNC path like \\nas\media, we use :smb: with the server and share parsed out.
        //
        // However, the simplest approach for a local network share is to use the
        // local-path-based mounting. Since the NAS is already accessible via SMB on Windows,
        // we mount the UNC path directly — rclone supports this on Windows natively.
        let remote_spec = config.nas_share_path.clone();
        let drive = format!("{}:", config.rclone_drive_letter);

        let mut args = vec![
            "mount".to_string(),
            remote_spec,
            drive,
            "--vfs-cache-mode".into(),
            "full".into(),
            "--cache-dir".into(),
            config.cache_dir_path.clone(),
            "--vfs-cache-max-size".into(),
            config.cache_max_size.clone(),
            "--vfs-cache-max-age".into(),
            config.cache_max_age.clone(),
            "--vfs-write-back".into(),
            config.vfs_write_back.clone(),
            "--vfs-read-chunk-size".into(),
            config.vfs_read_chunk_size.clone(),
            "--vfs-read-chunk-size-limit=off".into(),
            format!("--transfers={}", config.vfs_read_chunk_streams),
            "--vfs-read-ahead".into(),
            config.vfs_read_ahead.clone(),
            "--buffer-size".into(),
            config.buffer_size.clone(),
            // RC API for cache stats / flush
            "--rc".into(),
            format!("--rc-addr=127.0.0.1:{}", rc_port),
            "--rc-no-auth".into(),
            // Logging: use -v for verbose to stderr (which we capture)
            "-v".into(),
        ];

        args.extend(config.extra_rclone_flags.clone());

        log::info!(
            "Spawning rclone: {} {}",
            rclone_path.display(),
            args.join(" ")
        );

        let mut cmd = Command::new(&rclone_path);
        cmd.args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn rclone: {}", e))?;

        let pid = child.id();
        log::info!("rclone spawned with PID {:?}", pid);

        let (signal_tx, signal_rx) = mpsc::channel(32);

        // Spawn stderr log reader — parses rclone output for started/fatal signals
        if let Some(stderr) = child.stderr.take() {
            let tx = signal_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!("[rclone] {}", line);
                    if let Some(signal) = parser::parse_log_line(&line) {
                        if tx.send(signal).await.is_err() {
                            break;
                        }
                    }
                }
                // When stderr closes, the process has likely exited.
                // Send nothing here — the exit watcher handles that.
                log::debug!("rclone stderr reader finished");
            });
        }

        // Spawn exit watcher — monitors the child process and sends Exited when it dies
        let exit_tx = signal_tx.clone();
        // We need to give the exit watcher a way to wait on the child.
        // Since child is owned by RcloneManager, we use a shared flag.
        // Instead, we'll poll from a background task using the PID.
        // On Windows we can use WaitForSingleObject, but for simplicity
        // we'll poll try_wait from the orchestrator's loop.
        // The exit watcher below is a fallback that polls via PID.
        if let Some(raw_pid) = pid {
            tokio::spawn(async move {
                // Poll every 2 seconds to check if the process is still alive
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                    // Check if process is alive using a platform-specific method
                    #[cfg(windows)]
                    let alive = {
                        use windows::Win32::System::Threading::{
                            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
                        };
                        use windows::Win32::Foundation::CloseHandle;

                        let handle = unsafe {
                            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, raw_pid)
                        };
                        match handle {
                            Ok(h) => {
                                unsafe { let _ = CloseHandle(h); }
                                true
                            }
                            Err(_) => false,
                        }
                    };

                    #[cfg(not(windows))]
                    let alive = {
                        std::path::Path::new(&format!("/proc/{}", raw_pid)).exists()
                    };

                    if !alive {
                        log::warn!("rclone process {} no longer exists", raw_pid);
                        let _ = exit_tx.send(RcloneSignal::Exited { code: None }).await;
                        break;
                    }
                }
            });
        }

        Ok((
            Self {
                child: Some(child),
                rc_port,
            },
            signal_rx,
        ))
    }

    /// Kill the rclone process gracefully, then forcefully.
    pub async fn kill(&mut self) {
        if let Some(ref mut child) = self.child {
            let pid = child.id();
            log::info!("Killing rclone process (pid={:?})", pid);

            // First try graceful shutdown via RC API
            let rc_url = format!("http://127.0.0.1:{}/core/quit", self.rc_port);
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build();
            if let Ok(client) = client {
                let _ = client.post(&rc_url).send().await;
                // Give it a moment to shut down
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }

            // Force kill if still running
            match child.try_wait() {
                Ok(Some(_)) => {
                    log::info!("rclone exited gracefully");
                }
                _ => {
                    log::info!("Force-killing rclone");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }

            self.child = None;
        }
    }

    /// Get the RC API port for this rclone instance.
    pub fn rc_port(&self) -> u16 {
        self.rc_port
    }

    /// Check if process is running.
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// Kill any orphaned rclone processes that are mounting a specific drive letter.
    /// Called at startup before spawning a new rclone instance.
    /// Uses PowerShell to query process command lines (WMIC is deprecated/unavailable).
    #[cfg(windows)]
    pub fn kill_orphaned_rclone(drive_letter: &str) {
        use std::process::Command;

        let drive_pattern = format!("{}:", drive_letter);

        // Use PowerShell to find rclone processes with their command lines
        let ps_cmd = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='rclone.exe'\" | Select-Object ProcessId,CommandLine | ForEach-Object {{ \"$($_.ProcessId)|$($_.CommandLine)\" }}"
        );

        let output = match Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps_cmd])
            .output()
        {
            Ok(o) => o,
            Err(e) => {
                log::debug!("Failed to query rclone processes: {}", e);
                return;
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Format: PID|CommandLine
            let (pid_str, cmd_line) = match line.split_once('|') {
                Some(pair) => pair,
                None => continue,
            };

            // Check if this rclone is mounting our drive letter
            if cmd_line.contains(&drive_pattern) {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    log::warn!(
                        "Found orphaned rclone process (PID {}) using drive {}, killing it",
                        pid, drive_pattern
                    );

                    // Kill via Windows API
                    use windows::Win32::System::Threading::{
                        OpenProcess, TerminateProcess, PROCESS_TERMINATE,
                    };
                    use windows::Win32::Foundation::CloseHandle;

                    unsafe {
                        if let Ok(handle) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                            let _ = TerminateProcess(handle, 1);
                            let _ = CloseHandle(handle);
                        }
                    }

                    // Give it a moment to release the drive
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        }
    }
}
