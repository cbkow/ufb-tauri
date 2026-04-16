#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod ipc;
mod messages;
mod mount_service;
mod orchestrator;
mod platform;
mod state;
mod sync;
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

#[cfg(unix)]
struct MutexGuard {
    _lock_file: std::fs::File,
}

#[cfg(not(any(windows, unix)))]
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

#[cfg(unix)]
fn ensure_single_instance() -> MutexGuard {
    use std::os::unix::io::AsRawFd;

    let lock_dir = if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = std::path::PathBuf::from(runtime_dir).join("ufb");
        let _ = std::fs::create_dir_all(&dir);
        dir
    } else {
        std::path::PathBuf::from("/tmp")
    };

    let lock_path = lock_dir.join("mediamount-agent.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| {
            eprintln!("Failed to create lock file {}: {}", lock_path.display(), e);
            process::exit(1);
        });

    let fd = lock_file.as_raw_fd();
    let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        log::error!("Another mediamount-agent is already running");
        process::exit(1);
    }

    MutexGuard { _lock_file: lock_file }
}

#[cfg(not(any(windows, unix)))]
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

    // Honor RUST_LOG=debug (or UFB_DEBUG=1) for verbose diagnostics during
    // development. Default is Info.
    let level = match std::env::var("RUST_LOG")
        .ok()
        .as_deref()
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some(s) if s.contains("debug") => LevelFilter::Debug,
        Some(s) if s.contains("trace") => LevelFilter::Trace,
        Some(s) if s.contains("warn") => LevelFilter::Warn,
        _ if std::env::var("UFB_DEBUG").ok().as_deref() == Some("1") => LevelFilter::Debug,
        _ => LevelFilter::Info,
    };
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

/// The async event loop — runs on the main thread (Windows/Linux) or a background thread (macOS).
async fn run_event_loop() {
    // Start IPC server
    #[cfg(windows)]
    let mut ipc_server = ipc::server::IpcServer::start();

    #[cfg(unix)]
    let mut ipc_server = ipc::unix_server::IpcServer::start();


    #[cfg(not(any(windows, unix)))]
    {
        log::error!("IPC server not implemented for this platform");
        process::exit(1);
    }

    #[cfg(any(windows, unix))]
    {
        // Channel for agent→UFB messages from mount orchestrators
        let (state_tx, mut state_rx) = tokio::sync::mpsc::channel::<messages::AgentToUfb>(128);

        // Shared config cache — loaded once at startup, refreshed when the
        // config file changes (watcher below). FileOpsServer reads from this
        // instead of hitting disk + JSON-parsing mounts.json on every handler.
        let config_cache: std::sync::Arc<std::sync::RwLock<config::MountsConfig>> =
            std::sync::Arc::new(std::sync::RwLock::new(config::load_config()));

        // Shared cache of canonicalized base paths per domain. Eliminates one
        // of the two SMB `canonicalize()` round-trips per file op. Main clears
        // this on config reload so mounts that moved are re-resolved.
        let canonical_bases: std::sync::Arc<
            std::sync::RwLock<std::collections::HashMap<String, std::path::PathBuf>>,
        > = std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()));

        // Shared per-domain cache map — used by both the FileOps IPC server
        // and the NFS loopback server (one MacosCache instance per DB file).
        #[cfg(target_os = "macos")]
        let shared_caches: ipc::fileops_server::SharedCaches = std::sync::Arc::new(
            std::sync::RwLock::new(std::collections::HashMap::new()),
        );

        // Start file operations server for FileProvider extension (macOS only).
        // It uses the same agent→UFB channel to emit out-of-band events such
        // as conflict-detected notifications.
        //
        // When UFB_ENABLE_NFS=1 we skip fileops startup entirely — the NFS
        // loopback is the sole user-facing surface, and keeping FileProvider
        // active in parallel causes it to hammer the shared cache DB from
        // the extension side, stepping on our NFS writes. (Phase 3 debug:
        // the FP extension's record_enumeration + orphan scans were racing
        // our NFS handler writes and producing "disappearing row" symptoms.)
        #[cfg(target_os = "macos")]
        let nfs_enabled = std::env::var("UFB_ENABLE_NFS").ok().as_deref() == Some("1");
        #[cfg(target_os = "macos")]
        if !nfs_enabled {
            ipc::fileops_server::FileOpsServer::start(
                state_tx.clone(),
                std::sync::Arc::clone(&config_cache),
                std::sync::Arc::clone(&canonical_bases),
                std::sync::Arc::clone(&shared_caches),
            );
        } else {
            log::info!(
                "[FileOps] Skipped — UFB_ENABLE_NFS=1 is set; NFS loopback is the sole file-serving surface"
            );
        }

        // Tray receives a copy of state updates
        let (tray_state_tx, tray_state_rx) = tokio::sync::mpsc::channel::<messages::AgentToUfb>(64);

        // Start tray icon — on macOS, tray runs on the main thread (see main()),
        // so TrayManager::start spawns a no-op; the real tray is started separately.
        let (mut _tray_manager, mut tray_cmd_rx) = tray::TrayManager::start(tray_state_rx);

        // Start mount service
        let mut mount_service = mount_service::MountService::new(state_tx);
        mount_service.start_from_config().await;

        // Start NFS loopback servers (macOS only). One per sync-enabled mount.
        // Gated behind UFB_ENABLE_NFS=1 — when set, NFS replaces FileProvider
        // as the user-facing surface (see earlier fileops gate).
        #[cfg(target_os = "macos")]
        if nfs_enabled {
            let config_for_nfs = std::sync::Arc::clone(&config_cache);
            let caches_for_nfs = std::sync::Arc::clone(&shared_caches);
            tokio::spawn(async move {
                // Brief delay so mount_service has time to finish initial mounts
                // before we try to canonicalize the SMB mount paths. Lifecycle
                // wiring (start on MountSucceeded event) is Phase 1.5.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                let mounts = config_for_nfs.read().unwrap().mounts.clone();
                let mut sync_mounts: Vec<_> = mounts
                    .into_iter()
                    .filter(|m| m.enabled && m.is_sync_mode())
                    .collect();
                sync_mounts.sort_by(|a, b| a.id.cmp(&b.id));

                for (idx, mount) in sync_mounts.iter().enumerate() {
                    let port = sync::nfs_server::BASE_PORT + idx as u16;
                    let Some(root) = resolve_mount_root_for_nfs(mount) else {
                        log::warn!(
                            "[nfs-server] {}: no SMB mount path resolved, skipping",
                            mount.share_name()
                        );
                        continue;
                    };
                    let share = mount.share_name();

                    // Share a single MacosCache instance per domain with the
                    // FileOps IPC server. Open on-demand and insert into the
                    // shared map (fileops_server::ensure_cache uses the same
                    // map, so either server populates and both see it).
                    let cache = {
                        let mut caches = caches_for_nfs.write().unwrap();
                        if let Some(existing) = caches.get(&share).cloned() {
                            existing
                        } else {
                            match sync::MacosCache::open(
                                &share,
                                root.clone(),
                                mount.sync_cache_limit_bytes,
                            ) {
                                Ok(c) => {
                                    let arc = std::sync::Arc::new(c);
                                    caches.insert(share.clone(), std::sync::Arc::clone(&arc));
                                    log::info!(
                                        "[nfs-server] Cache opened (shared) for domain: {}",
                                        share
                                    );
                                    arc
                                }
                                Err(e) => {
                                    log::error!(
                                        "[nfs-server] {}: failed to open cache: {}",
                                        share,
                                        e
                                    );
                                    continue;
                                }
                            }
                        }
                    };
                    sync::nfs_server::start(share, root, port, cache);
                }
            });
        }

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
                    log::debug!("IPC command received: {:?}", cmd);
                    mount_service.handle_command(cmd).await;
                }

                // Outgoing state updates to forward to UFB and tray
                Some(msg) = state_rx.recv() => {
                    log::debug!("Forwarding to UFB+tray: {:?}", msg);
                    // Forward to tray
                    let _ = tray_state_tx.try_send(msg.clone());
                    // Forward to UFB
                    if let Err(e) = ipc_server.send(msg).await {
                        log::warn!("Failed to forward to UFB: {}", e);
                    }
                }

                // Config file changed on disk
                Some(()) = config_reload_rx.recv() => {
                    // Refresh shared cache BEFORE mount_service reloads, so
                    // any FileOpsServer handlers that race with the reload
                    // see the new config. Also invalidate the canonical-base
                    // cache — a mount path change would otherwise stick.
                    *config_cache.write().unwrap() = config::load_config();
                    canonical_bases.write().unwrap().clear();
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
    process::exit(0);
}

/// macOS: headless agent — tray UI handled by companion Swift MenuBarExtra app.
/// The Swift app communicates with this agent via the same Unix socket IPC.
#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() {
    init_logging();
    install_panic_hook();

    log::info!(
        "mediamount-agent v{} starting (headless — tray via companion app)",
        env!("CARGO_PKG_VERSION")
    );

    let _mutex_guard = ensure_single_instance();
    ensure_macos_mount_dir();

    run_event_loop().await;
}

/// Windows/Linux: tokio runs on the main thread, tray on a spawned thread.
#[cfg(not(target_os = "macos"))]
#[tokio::main]
async fn main() {
    init_logging();
    install_panic_hook();

    // Check for --create-symlinks mode (runs elevated, creates symlinks, exits)
    #[cfg(windows)]
    if std::env::args().any(|a| a == "--create-symlinks") {
        log::info!("mediamount-agent --create-symlinks (elevated mode)");
        create_symlinks_and_exit();
        return;
    }

    log::info!(
        "mediamount-agent v{} starting",
        env!("CARGO_PKG_VERSION")
    );

    let _mutex_guard = ensure_single_instance();

    run_event_loop().await;
}

/// Elevated mode: create symlinks for all configured mounts, migrate old drive letters, exit.
/// Called via ShellExecuteW "runas" from the normal agent instance.
#[cfg(windows)]
fn create_symlinks_and_exit() {
    use crate::platform::DriveMapping;
    use crate::platform::windows::mountpoint::WindowsMountMapping;

    let config = config::load_config();
    let cache_root = config.cache_root();
    let mapping = WindowsMountMapping::new();

    // Ensure base directory exists
    if let Err(e) = WindowsMountMapping::ensure_volumes_dir() {
        log::error!("[symlinks] Failed to create volumes dir: {}", e);
        return;
    }

    let mut created = 0u32;
    let mut migrated = 0u32;

    for mount in &config.mounts {
        if !mount.enabled {
            continue;
        }

        let volume_path = mount.volume_path();

        // Sync mounts: symlink → cache dir. Traditional mounts: symlink → UNC path.
        let target = if mount.sync_enabled {
            mount.sync_root_dir(&cache_root).to_string_lossy().to_string()
        } else {
            mount.nas_share_path.clone()
        };

        // Create symlink
        match mapping.switch(&volume_path, &target) {
            Ok(()) => {
                created += 1;
                log::info!("[symlinks] Created {} → {}", volume_path, target);
            }
            Err(e) => {
                log::error!("[symlinks] Failed {} → {}: {}", volume_path, target, e);
            }
        }

        // Migrate old drive letter if present
        if !mount.mount_drive_letter.is_empty() {
            let drive = &mount.mount_drive_letter;
            match crate::platform::windows::fallback::disconnect_drive(drive) {
                Ok(()) => {
                    migrated += 1;
                    log::info!("[symlinks] Migrated {}:\\ → {}", drive, volume_path);
                }
                Err(e) => {
                    // Not fatal — drive may not be connected
                    log::debug!("[symlinks] Drive {} disconnect skipped: {}", drive, e);
                }
            }
        }
    }

    log::info!(
        "[symlinks] Done: {} created, {} drive letters migrated",
        created, migrated
    );
}

/// macOS: ensure the user-facing mount directories exist.
///
/// Both live under user-owned paths (no admin required):
/// - `~/ufb/mounts/` — user-facing symlinks to actual mount points
/// - `~/.local/share/ufb/smb-mounts/` — private mountpoints for `mount_smbfs` targets
///
/// Legacy `/opt/ufb/mounts/` is left in place if present (harmless on upgrade);
/// future installs will never need admin privileges again.
#[cfg(target_os = "macos")]
fn ensure_macos_mount_dir() {
    let volumes_base = crate::config::MountConfig::volumes_base();
    if let Err(e) = std::fs::create_dir_all(&volumes_base) {
        log::error!(
            "Failed to create {}: {}",
            volumes_base.display(),
            e
        );
    }

    let smb_base = crate::config::MountConfig::smb_mount_base();
    if let Err(e) = std::fs::create_dir_all(&smb_base) {
        log::error!("Failed to create {}: {}", smb_base.display(), e);
    }
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
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .arg(&path)
                .spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = std::process::Command::new("xdg-open")
                .arg(&path)
                .spawn();
        }
    }
}

/// Resolve the backing SMB mount path for a mount config. Mirrors the
/// `resolve_smb_mount_path` helper in ipc::fileops_server so the NFS server
/// picks up dedup-suffixed mounts (`/Volumes/Share-1`) the same way. Will be
/// consolidated with the fileops_server copy once NFS replaces it in Phase 5.
#[cfg(target_os = "macos")]
fn resolve_mount_root_for_nfs(mount: &config::MountConfig) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let share_name = mount.share_name();
    let candidates = [
        config::MountConfig::smb_mount_base().join(&share_name),
        std::path::PathBuf::from("/Volumes").join(&share_name),
    ];
    for candidate in &candidates {
        let Ok(path_meta) = std::fs::metadata(candidate) else { continue };
        let Some(parent_meta) = candidate.parent().and_then(|p| std::fs::metadata(p).ok())
        else { continue };
        if path_meta.dev() != parent_meta.dev() {
            return Some(candidate.clone());
        }
    }
    crate::platform::macos::find_existing_volume(&share_name, &mount.nas_share_path)
        .map(std::path::PathBuf::from)
}
