#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod health;
mod ipc;
mod messages;
mod mount_service;
mod orchestrator;
mod platform;
mod rclone;
mod state;
mod tray;

use std::process;

// ── Single-instance mutex ──

#[cfg(windows)]
struct MutexGuard {
    _handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for MutexGuard {
    fn drop(&mut self) {
        if !self._handle.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self._handle);
            }
        }
    }
}

#[cfg(not(windows))]
struct MutexGuard;

#[cfg(windows)]
fn ensure_single_instance() -> MutexGuard {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let mutex_name: Vec<u16> = "MediaMountAgent\0".encode_utf16().collect();

    let handle = unsafe { CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) };
    match handle {
        Ok(h) => {
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                log::error!("Another mediamount-agent is already running");
                process::exit(1);
            }
            MutexGuard { _handle: h }
        }
        Err(e) => {
            log::error!("Failed to create instance mutex: {}", e);
            process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn ensure_single_instance() -> MutexGuard {
    MutexGuard
}

// ── Logging ──

fn log_file_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let dir = std::path::PathBuf::from(local).join("ufb");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("mediamount-agent.log"));
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            let dir = std::path::PathBuf::from(home).join(".local/share/ufb");
            let _ = std::fs::create_dir_all(&dir);
            return Some(dir.join("mediamount-agent.log"));
        }
    }
    None
}

fn init_logging() {
    use simplelog::*;

    let level = LevelFilter::Info;
    let config = ConfigBuilder::new().set_time_format_rfc3339().build();

    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![TermLogger::new(
        level,
        config.clone(),
        TerminalMode::Stderr,
        ColorChoice::Auto,
    )];

    if let Some(path) = log_file_path() {
        // Truncate if log is > 2 MB
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 2 * 1024 * 1024 {
                let _ = std::fs::remove_file(&path);
            }
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(file) => {
                loggers.push(WriteLogger::new(level, config.clone(), file));
                eprintln!(
                    "[mediamount-agent] Logging to {}",
                    path.display()
                );
            }
            Err(e) => {
                eprintln!(
                    "[mediamount-agent] Warning: could not open log file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    CombinedLogger::init(loggers).unwrap_or_else(|e| {
        eprintln!("[mediamount-agent] Failed to init logger: {}", e);
    });
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        log::error!("PANIC at {}: {}", location, payload);

        if let Some(path) = log_file_path() {
            let msg = format!("[PANIC] {} at {}\n", payload, location);
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| {
                    use std::io::Write;
                    f.write_all(msg.as_bytes())
                });
        }

        default_hook(info);
    }));
}

// ── Main ──

#[tokio::main]
async fn main() {
    init_logging();
    install_panic_hook();

    log::info!(
        "mediamount-agent v{} starting",
        env!("CARGO_PKG_VERSION")
    );

    let _mutex_guard = ensure_single_instance();

    // Start IPC server
    #[cfg(windows)]
    let mut ipc_server = ipc::server::IpcServer::start();

    #[cfg(not(windows))]
    {
        log::error!("IPC server not implemented for this platform");
        process::exit(1);
    }

    // Main loop (Windows only — IPC requires named pipes)
    #[cfg(windows)]
    {
        // Channel for agent→UFB messages from mount orchestrators
        let (state_tx, mut state_rx) = tokio::sync::mpsc::channel::<messages::AgentToUfb>(128);

        // Tray receives a copy of state updates
        let (tray_state_tx, tray_state_rx) = tokio::sync::mpsc::channel::<messages::AgentToUfb>(64);

        // Start tray icon
        let (mut _tray_manager, mut tray_cmd_rx) = tray::TrayManager::start(tray_state_rx);

        // Start mount service
        let mut mount_service = mount_service::MountService::new(state_tx);
        mount_service.start_from_config().await;

        // Config file watcher — polls mtime every 5 seconds
        let (config_reload_tx, mut config_reload_rx) = tokio::sync::mpsc::channel::<()>(1);
        if let Some(config_path) = config::config_file_path() {
            tokio::spawn(async move {
                let mut last_mtime = std::fs::metadata(&config_path)
                    .and_then(|m| m.modified())
                    .ok();

                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                    let current_mtime = std::fs::metadata(&config_path)
                        .and_then(|m| m.modified())
                        .ok();

                    if current_mtime != last_mtime && current_mtime.is_some() {
                        last_mtime = current_mtime;
                        log::info!("Config file changed, triggering reload");
                        if config_reload_tx.send(()).await.is_err() {
                            break;
                        }
                    }
                }
            });
        }

        log::info!("mediamount-agent ready");

        // Main event loop
        loop {
            tokio::select! {
                // Commands from UFB via IPC
                Some(cmd) = ipc_server.command_rx.recv() => {
                    mount_service.handle_command(cmd).await;
                }

                // Outgoing state updates to forward to UFB and tray
                Some(msg) = state_rx.recv() => {
                    // Forward to tray
                    let _ = tray_state_tx.try_send(msg.clone());
                    // Forward to UFB
                    if let Err(e) = ipc_server.send(msg).await {
                        log::debug!("No UFB client connected: {}", e);
                    }
                }

                // Config file changed on disk
                Some(()) = config_reload_rx.recv() => {
                    mount_service.reload_config().await;
                }

                // Commands from tray context menu
                Some(tray_cmd) = tray_cmd_rx.recv() => {
                    match tray_cmd {
                        tray::TrayCommand::MountEvent(mount_id, event) => {
                            mount_service.route_event(&mount_id, event).await;
                        }
                        tray::TrayCommand::OpenUfb => {
                            open_ufb();
                        }
                        tray::TrayCommand::OpenLog => {
                            open_log();
                        }
                        tray::TrayCommand::Quit => {
                            log::info!("Quit requested from tray");
                            mount_service.shutdown().await;
                            _tray_manager.stop();
                            break;
                        }
                    }
                }

                // Ctrl+C / shutdown signal
                _ = tokio::signal::ctrl_c() => {
                    log::info!("Shutdown signal received");
                    mount_service.shutdown().await;
                    _tray_manager.stop();
                    break;
                }
            }
        }
    }

    log::info!("mediamount-agent exiting");

    // Force exit — background tasks (IPC listener, config watcher) would
    // otherwise keep the tokio runtime alive indefinitely.
    process::exit(0);
}

/// Launch UFB executable (next to our binary, in dev build output, or in PATH).
fn open_ufb() {
    let exe_name = if cfg!(target_os = "windows") {
        "ufb-tauri.exe"
    } else {
        "ufb-tauri"
    };

    let mut path = std::path::PathBuf::from(exe_name);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Production: next to agent exe
            let sidecar = dir.join(exe_name);
            if sidecar.exists() {
                path = sidecar;
            } else {
                // Dev: agent is in mediamount-agent/target/debug/,
                // UFB is in src-tauri/target/debug/
                let dev_path = dir.join("../../src-tauri/target/debug").join(exe_name);
                if let Ok(canon) = std::fs::canonicalize(&dev_path) {
                    path = canon;
                }
            }
        }
    }

    log::info!("Opening UFB: {}", path.display());
    let _ = std::process::Command::new(path)
        .spawn()
        .map_err(|e| log::error!("Failed to launch UFB: {}", e));
}

/// Open the agent log file in the default text editor.
fn open_log() {
    if let Some(path) = log_file_path() {
        log::info!("Opening log: {}", path.display());
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &path.to_string_lossy()])
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(&path)
                .spawn();
        }
    }
}

