use crate::messages::AgentToUfb;
use crate::state::MountEvent;
use tokio::sync::mpsc;

/// Commands the tray thread sends back to the tokio runtime.
#[derive(Debug)]
pub enum TrayCommand {
    MountEvent(String, MountEvent), // (mount_id, event)
    OpenUfb,
    OpenLog,
    Quit,
}

/// System tray icon manager.
pub struct TrayManager {
    _cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl TrayManager {
    /// Start the tray icon. Returns the manager handle and a receiver for tray commands.
    pub fn start(
        _state_rx: mpsc::Receiver<AgentToUfb>,
    ) -> (Self, mpsc::Receiver<TrayCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        #[cfg(windows)]
        let cancel_tx = {
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            let cmd_tx_clone = cmd_tx.clone();
            std::thread::Builder::new()
                .name("tray".into())
                .spawn(move || {
                    windows_tray::run_tray(cmd_tx_clone, _state_rx, cancel_rx);
                })
                .expect("Failed to spawn tray thread");
            Some(cancel_tx)
        };

        #[cfg(target_os = "linux")]
        let cancel_tx = {
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            let cmd_tx_clone = cmd_tx.clone();
            std::thread::Builder::new()
                .name("tray".into())
                .spawn(move || {
                    linux_tray::run_tray(cmd_tx_clone, _state_rx, cancel_rx);
                })
                .expect("Failed to spawn tray thread");
            Some(cancel_tx)
        };

        // macOS: tray runs on the main thread (see main.rs macos_main_tray).
        // TrayManager::start is a no-op on macOS.
        #[cfg(target_os = "macos")]
        let cancel_tx = {
            log::info!("macOS tray managed by main thread");
            let _ = (cmd_tx, _state_rx);
            None
        };

        #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
        let cancel_tx = {
            log::info!("Tray icon not implemented for this platform");
            let _ = (cmd_tx, _state_rx);
            None
        };

        (Self { _cancel_tx: cancel_tx }, cmd_rx)
    }

    /// Stop the tray icon.
    pub fn stop(&mut self) {
        if let Some(tx) = self._cancel_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
mod windows_tray {
    use super::TrayCommand;
    use crate::messages::AgentToUfb;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY_CALLBACK: u32 = 0x0400 + 1; // WM_APP + 1
    const TRAY_ICON_ID: u32 = 1;

    // Menu item IDs
    const IDM_RESTART_BASE: u32 = 1000; // 1000 + mount_index for per-mount restart
    const IDM_TOGGLE_BASE: u32 = 1100; // 1100 + mount_index for per-mount connect/disconnect
    const IDM_OPEN_UFB: u32 = 2001;
    const IDM_OPEN_LOG: u32 = 2002;
    const IDM_TOGGLE_AUTOSTART: u32 = 2003;
    const IDM_QUIT: u32 = 9001;

    struct TrayState {
        cmd_tx: mpsc::Sender<TrayCommand>,
        mounts: std::collections::HashMap<String, crate::messages::MountStateUpdateMsg>,
    }

    // SAFETY: TrayState is only mutated from the tray thread via the message pump.
    // The Mutex ensures exclusive access. cmd_tx (mpsc::Sender) is Send+Sync.
    unsafe impl Send for TrayState {}

    static TRAY_STATE: std::sync::OnceLock<Arc<Mutex<Option<TrayState>>>> =
        std::sync::OnceLock::new();

    fn get_tray_state() -> &'static Arc<Mutex<Option<TrayState>>> {
        TRAY_STATE.get_or_init(|| Arc::new(Mutex::new(None)))
    }

    fn wide_string(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn set_tooltip(nid: &mut NOTIFYICONDATAW, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let len = wide.len().min(nid.szTip.len() - 1);
        nid.szTip[..len].copy_from_slice(&wide[..len]);
        nid.szTip[len] = 0;
    }

    pub fn run_tray(
        cmd_tx: mpsc::Sender<TrayCommand>,
        state_rx: mpsc::Receiver<AgentToUfb>,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        unsafe {
            let hinstance = GetModuleHandleW(None).unwrap_or_default();
            let class_name = wide_string("MediaMountTray");

            let wc = WNDCLASSW {
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };

            RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(wide_string("MediaMount").as_ptr()),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                None,
                hinstance,
                None,
            )
            .unwrap_or_default();

            if hwnd.0.is_null() {
                log::error!("Failed to create tray message window");
                return;
            }

            // Try to load UFB icon from known locations, fall back to default
            let hicon = load_app_icon().unwrap_or_else(|| {
                LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
            });

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: WM_TRAY_CALLBACK,
                hIcon: hicon,
                ..Default::default()
            };
            set_tooltip(&mut nid, "MediaMount Agent");

            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                log::error!("Shell_NotifyIconW NIM_ADD failed");
                return;
            }

            log::info!("Tray icon created");

            {
                let mut lock = get_tray_state().lock().unwrap();
                *lock = Some(TrayState {
                    cmd_tx,
                    mounts: std::collections::HashMap::new(),
                });
            }

            // Timer to poll state updates and cancellation
            SetTimer(hwnd, 1, 500, None);

            // Store receivers as window user data
            let receivers = Box::new((state_rx, cancel_rx));
            let receivers_ptr = Box::into_raw(receivers);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, receivers_ptr as isize);

            // Win32 message pump
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // Cleanup
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = Box::from_raw(receivers_ptr);
            log::info!("Tray icon removed");
        }
    }

    unsafe extern "system" fn tray_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY_CALLBACK => {
                let event = (lparam.0 & 0xFFFF) as u32;
                if event == WM_RBUTTONUP {
                    show_context_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let cmd_id = (wparam.0 & 0xFFFF) as u32;
                handle_menu_command(cmd_id);
                LRESULT(0)
            }
            WM_TIMER => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                if ptr != 0 {
                    let receivers = &mut *(ptr
                        as *mut (
                            mpsc::Receiver<AgentToUfb>,
                            tokio::sync::oneshot::Receiver<()>,
                        ));

                    // Check for cancellation
                    if let Ok(()) = receivers.1.try_recv() {
                        PostQuitMessage(0);
                        return LRESULT(0);
                    }

                    // Drain state updates
                    let mut changed = false;
                    while let Ok(msg) = receivers.0.try_recv() {
                        if let AgentToUfb::MountStateUpdate(update) = msg {
                            let mut lock = get_tray_state().lock().unwrap();
                            if let Some(ref mut state) = *lock {
                                state.mounts.insert(update.mount_id.clone(), update);
                                changed = true;
                            }
                        }
                    }
                    if changed {
                        let lock = get_tray_state().lock().unwrap();
                        if let Some(ref state) = *lock {
                            let total = state.mounts.len();
                            let mounted = state.mounts.values().filter(|m| m.state == "mounted").count();
                            let tip = if total == 0 {
                                "MediaMount".to_string()
                            } else if mounted == total {
                                format!("MediaMount — all mounted ({}/{})", mounted, total)
                            } else {
                                format!("MediaMount — {}/{} mounted", mounted, total)
                            };
                            let mut nid = NOTIFYICONDATAW {
                                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                                hWnd: hwnd,
                                uID: TRAY_ICON_ID,
                                uFlags: NIF_TIP,
                                ..Default::default()
                            };
                            set_tooltip(&mut nid, &tip);
                            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
                        }
                    }
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn show_context_menu(hwnd: HWND) {
        let menu = CreatePopupMenu().unwrap_or_default();
        if menu.is_invalid() {
            return;
        }

        let lock = get_tray_state().lock().unwrap();
        let state = match lock.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Title (disabled)
        append_menu_item(menu, 0, "MediaMount", MF_DISABLED | MF_GRAYED);
        AppendMenuW(menu, MF_SEPARATOR, 0, None).ok();

        // Per-mount status + restart
        if state.mounts.is_empty() {
            append_menu_item(menu, 0, "No mounts configured", MF_DISABLED | MF_GRAYED);
        } else {
            // Store mount IDs in order for IDM_RESTART_BASE offset
            let mut mount_ids: Vec<&String> = state.mounts.keys().collect();
            mount_ids.sort();
            for (i, mount_id) in mount_ids.iter().enumerate() {
                if let Some(ms) = state.mounts.get(*mount_id) {
                    let icon = if ms.state == "mounted" { "\u{25CF}" } else { "\u{2715}" };
                    let label = format!("{} {} {}", ms.mount_id, icon, ms.state_detail);
                    append_menu_item(menu, 0, &label, MF_DISABLED | MF_GRAYED);
                    // Connect/Disconnect toggle for this mount
                    let toggle_id = IDM_TOGGLE_BASE + i as u32;
                    let is_active = ms.state == "mounted" || ms.state == "mounting" || ms.state == "initializing";
                    let toggle_label = if is_active {
                        format!("  Disconnect {}", ms.mount_id)
                    } else {
                        format!("  Connect {}", ms.mount_id)
                    };
                    append_menu_item(menu, toggle_id, &toggle_label, MF_STRING);
                    // Restart button for this mount (base ID + index)
                    let restart_id = IDM_RESTART_BASE + i as u32;
                    let restart_label = format!("  Restart {}", ms.mount_id);
                    append_menu_item(menu, restart_id, &restart_label, MF_STRING);
                }
            }
        }

        AppendMenuW(menu, MF_SEPARATOR, 0, None).ok();

        append_menu_item(menu, IDM_OPEN_UFB, "Open UFB", MF_STRING);
        append_menu_item(menu, IDM_OPEN_LOG, "Open log", MF_STRING);
        let autostart_label = if crate::platform::is_auto_start_enabled() {
            "Disable auto-start"
        } else {
            "Start at login"
        };
        append_menu_item(menu, IDM_TOGGLE_AUTOSTART, autostart_label, MF_STRING);
        AppendMenuW(menu, MF_SEPARATOR, 0, None).ok();
        append_menu_item(menu, IDM_QUIT, "Quit", MF_STRING);

        drop(lock);

        let _ = SetForegroundWindow(hwnd);

        let mut pt = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);

        let _ = DestroyMenu(menu);
    }

    unsafe fn append_menu_item(menu: HMENU, id: u32, text: &str, flags: MENU_ITEM_FLAGS) {
        let wide = wide_string(text);
        AppendMenuW(menu, flags, id as usize, PCWSTR(wide.as_ptr())).ok();
    }

    /// Load the UFB app icon. Tries embedded resource first, then file-based fallbacks.
    fn load_app_icon() -> Option<HICON> {
        // Try embedded resource (set by winres in build.rs, resource ID 1)
        let hinstance = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let icon = unsafe {
            LoadImageW(
                hinstance,
                PCWSTR(1 as *const u16), // MAKEINTRESOURCE(1)
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE,
            )
        };
        if let Ok(handle) = icon {
            if !handle.is_invalid() {
                log::info!("Loaded tray icon from embedded resource");
                return Some(HICON(handle.0));
            }
        }

        // Fallback: file-based search for dev builds
        let mut candidates = Vec::new();

        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("icon.ico"));
                candidates.push(dir.join("../../src-tauri/icons/icon.ico"));
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join("../src-tauri/icons/icon.ico"));
        }

        for path in candidates {
            if let Ok(canon) = std::fs::canonicalize(&path) {
                let wide = wide_string(&canon.to_string_lossy());
                let icon = unsafe {
                    LoadImageW(
                        None,
                        PCWSTR(wide.as_ptr()),
                        IMAGE_ICON,
                        0,
                        0,
                        LR_LOADFROMFILE | LR_DEFAULTSIZE,
                    )
                };
                if let Ok(handle) = icon {
                    if !handle.is_invalid() {
                        log::info!("Loaded tray icon from {}", canon.display());
                        return Some(HICON(handle.0));
                    }
                }
            }
        }

        log::debug!("No custom icon found, using default");
        None
    }

    fn handle_menu_command(cmd_id: u32) {
        let lock = get_tray_state().lock().unwrap();
        let state = match lock.as_ref() {
            Some(s) => s,
            None => return,
        };

        let cmd = match cmd_id {
            IDM_OPEN_UFB => Some(TrayCommand::OpenUfb),
            IDM_OPEN_LOG => Some(TrayCommand::OpenLog),
            IDM_TOGGLE_AUTOSTART => {
                let currently_enabled = crate::platform::is_auto_start_enabled();
                if let Err(e) = crate::platform::set_auto_start(!currently_enabled) {
                    log::error!("Failed to toggle auto-start: {}", e);
                }
                None
            }
            IDM_QUIT => Some(TrayCommand::Quit),
            id if id >= IDM_TOGGLE_BASE && id < IDM_TOGGLE_BASE + 100 => {
                let index = (id - IDM_TOGGLE_BASE) as usize;
                let mut mount_ids: Vec<&String> = state.mounts.keys().collect();
                mount_ids.sort();
                mount_ids.get(index).and_then(|mid| {
                    state.mounts.get(*mid).map(|ms| {
                        let is_active = ms.state == "mounted" || ms.state == "mounting" || ms.state == "initializing";
                        let event = if is_active {
                            crate::state::MountEvent::Stop
                        } else {
                            crate::state::MountEvent::Start
                        };
                        TrayCommand::MountEvent((*mid).clone(), event)
                    })
                })
            }
            id if id >= IDM_RESTART_BASE && id < IDM_RESTART_BASE + 100 => {
                let index = (id - IDM_RESTART_BASE) as usize;
                let mut mount_ids: Vec<&String> = state.mounts.keys().collect();
                mount_ids.sort();
                mount_ids.get(index).map(|mid| {
                    TrayCommand::MountEvent((*mid).clone(), crate::state::MountEvent::Restart)
                })
            }
            _ => None,
        };

        if let Some(cmd) = cmd {
            let tx = state.cmd_tx.clone();
            drop(lock);
            let _ = tx.blocking_send(cmd);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_tray {
    use super::TrayCommand;
    use crate::messages::AgentToUfb;
    use tokio::sync::mpsc;

    /// Linux tray using tray-icon + muda crates with GTK main loop.
    pub fn run_tray(
        cmd_tx: mpsc::Sender<TrayCommand>,
        mut state_rx: mpsc::Receiver<AgentToUfb>,
        mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        use tray_icon::TrayIconBuilder;
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, MenuEvent, IsMenuItem};

        // Initialize GTK (required by tray-icon on Linux)
        gtk::init().expect("Failed to init GTK for tray");

        // Build menu. Mount items are inserted dynamically between
        // mount_insert_pos (after the title separator) and the bottom section.
        let menu = Menu::new();
        let item_title = MenuItem::new("MediaMount", false, None);
        let sep_top = PredefinedMenuItem::separator();
        let sep_mounts = PredefinedMenuItem::separator();
        let item_open_ufb = MenuItem::new("Open UFB", true, None);
        let item_open_log = MenuItem::new("Open log", true, None);
        let item_autostart = MenuItem::new(
            if crate::platform::is_auto_start_enabled() { "Disable auto-start" } else { "Start at login" },
            true,
            None,
        );
        let item_quit = MenuItem::new("Quit", true, None);

        let _ = menu.append(&item_title);
        let _ = menu.append(&sep_top);
        // mount items go here (index 2+)
        let _ = menu.append(&sep_mounts);
        let _ = menu.append(&item_open_ufb);
        let _ = menu.append(&item_open_log);
        let _ = menu.append(&item_autostart);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_quit);

        // Keep a clone — muda Menu uses interior mutability so both
        // the tray and our loop see the same underlying menu.
        let menu_handle = menu.clone();

        let icon = load_icon();

        let _tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("MediaMount Agent")
            .with_icon(icon)
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                log::error!("Failed to create tray icon: {}", e);
                return;
            }
        };

        log::info!("Linux tray icon created");

        let mut mounts: std::collections::HashMap<String, crate::messages::MountStateUpdateMsg> =
            std::collections::HashMap::new();

        // Per-mount menu items keyed by mount_id: (status_item, toggle_item, restart_item)
        let mut mount_items: Vec<(String, MenuItem, MenuItem, MenuItem)> = Vec::new();
        // Position in the menu where mount items start (after title + sep_top)
        const MOUNT_INSERT_BASE: usize = 2;

        let menu_event_rx = MenuEvent::receiver();

        loop {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }

            if let Ok(()) = cancel_rx.try_recv() {
                break;
            }

            // Drain state updates
            let mut changed = false;
            while let Ok(msg) = state_rx.try_recv() {
                if let AgentToUfb::MountStateUpdate(update) = msg {
                    mounts.insert(update.mount_id.clone(), update);
                    changed = true;
                }
            }

            if changed {
                // Update tooltip
                let total = mounts.len();
                let mounted_count = mounts.values().filter(|m| m.state == "mounted").count();
                let tip = if total == 0 {
                    "MediaMount".to_string()
                } else if mounted_count == total {
                    format!("MediaMount \u{2014} all mounted ({}/{})", mounted_count, total)
                } else {
                    format!("MediaMount \u{2014} {}/{} mounted", mounted_count, total)
                };
                let _ = _tray.set_tooltip(Some(&tip));

                // Update or create per-mount menu items
                let mut sorted_ids: Vec<String> = mounts.keys().cloned().collect();
                sorted_ids.sort();

                for mount_id in &sorted_ids {
                    let ms = &mounts[mount_id];
                    let dot = if ms.state == "mounted" { "\u{25CF}" } else { "\u{2715}" };
                    let label = format!("{} {} {}", ms.mount_id, dot, ms.state_detail);
                    let is_active = ms.state == "mounted" || ms.state == "mounting" || ms.state == "initializing";
                    let toggle_label = if is_active {
                        format!("  Disconnect {}", mount_id)
                    } else {
                        format!("  Connect {}", mount_id)
                    };

                    if let Some(entry) = mount_items.iter().find(|(id, _, _, _)| id == mount_id) {
                        // Update existing
                        entry.1.set_text(&label);
                        entry.2.set_text(&toggle_label);
                    } else {
                        // Create new items and insert into menu
                        let status_item = MenuItem::new(&label, false, None);
                        let toggle_item = MenuItem::new(&toggle_label, true, None);
                        let restart_item = MenuItem::new(&format!("  Restart {}", mount_id), true, None);

                        // Insert before the mounts separator. Each mount adds 3 items,
                        // so the position grows as we add more.
                        let pos = MOUNT_INSERT_BASE + mount_items.len() * 3;
                        let _ = menu_handle.insert(&status_item as &dyn IsMenuItem, pos);
                        let _ = menu_handle.insert(&toggle_item as &dyn IsMenuItem, pos + 1);
                        let _ = menu_handle.insert(&restart_item as &dyn IsMenuItem, pos + 2);

                        mount_items.push((mount_id.clone(), status_item, toggle_item, restart_item));
                    }
                }
            }

            // Process menu events
            if let Ok(event) = menu_event_rx.try_recv() {
                let cmd = if event.id == item_open_ufb.id() {
                    Some(TrayCommand::OpenUfb)
                } else if event.id == item_open_log.id() {
                    Some(TrayCommand::OpenLog)
                } else if event.id == item_autostart.id() {
                    let currently_enabled = crate::platform::is_auto_start_enabled();
                    if let Err(e) = crate::platform::set_auto_start(!currently_enabled) {
                        log::error!("Failed to toggle auto-start: {}", e);
                    }
                    item_autostart.set_text(
                        if !currently_enabled { "Disable auto-start" } else { "Start at login" }
                    );
                    None
                } else if event.id == item_quit.id() {
                    Some(TrayCommand::Quit)
                } else if let Some(cmd) = mount_items.iter()
                    .find(|(_, _, toggle, _)| event.id == toggle.id())
                    .map(|(mount_id, _, _, _)| {
                        let ms = &mounts[mount_id];
                        let is_active = ms.state == "mounted" || ms.state == "mounting" || ms.state == "initializing";
                        let mount_event = if is_active {
                            crate::state::MountEvent::Stop
                        } else {
                            crate::state::MountEvent::Start
                        };
                        TrayCommand::MountEvent(mount_id.clone(), mount_event)
                    })
                {
                    Some(cmd)
                } else {
                    // Check per-mount restart items
                    mount_items.iter()
                        .find(|(_, _, _, restart)| event.id == restart.id())
                        .map(|(mount_id, _, _, _)| {
                            TrayCommand::MountEvent(mount_id.clone(), crate::state::MountEvent::Restart)
                        })
                };

                if let Some(cmd) = cmd {
                    let _ = cmd_tx.blocking_send(cmd);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        log::info!("Linux tray icon removed");
    }

    fn load_icon() -> tray_icon::Icon {
        // Try to load icon from file next to executable
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for name in &["icon.png", "../../src-tauri/icons/icon.png"] {
                    let path = dir.join(name);
                    if let Ok(img) = image::open(&path) {
                        let rgba = img.into_rgba8();
                        let (w, h) = rgba.dimensions();
                        if let Ok(icon) = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h) {
                            return icon;
                        }
                    }
                }
            }
        }

        // Fallback: 16x16 transparent icon
        tray_icon::Icon::from_rgba(vec![0u8; 16 * 16 * 4], 16, 16)
            .expect("Failed to create fallback icon")
    }
}

/// macOS tray — DEPRECATED: now handled by companion Swift MenuBarExtra app.
/// This module is kept for reference but not compiled on macOS.
#[cfg(all(target_os = "macos", feature = "_unused_macos_tray"))]
mod macos_tray {
    use super::TrayCommand;
    use crate::messages::AgentToUfb;
    use tokio::sync::mpsc;

    pub fn run_tray(
        cmd_tx: mpsc::Sender<TrayCommand>,
        mut state_rx: mpsc::Receiver<AgentToUfb>,
        mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        use tray_icon::TrayIconBuilder;
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, MenuEvent, IsMenuItem};

        let menu = Menu::new();
        let item_title = MenuItem::new("MediaMount", false, None);
        let sep_top = PredefinedMenuItem::separator();
        let sep_mounts = PredefinedMenuItem::separator();
        let item_open_ufb = MenuItem::new("Open UFB", true, None);
        let item_open_log = MenuItem::new("Open log", true, None);
        let item_autostart = MenuItem::new(
            if crate::platform::is_auto_start_enabled() { "Disable auto-start" } else { "Start at login" },
            true,
            None,
        );
        let item_quit = MenuItem::new("Quit", true, None);

        let _ = menu.append(&item_title);
        let _ = menu.append(&sep_top);
        let _ = menu.append(&sep_mounts);
        let _ = menu.append(&item_open_ufb);
        let _ = menu.append(&item_open_log);
        let _ = menu.append(&item_autostart);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_quit);

        let menu_handle = menu.clone();
        let icon = load_icon();

        let _tray = match TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("MediaMount Agent")
            .with_icon(icon)
            .build()
        {
            Ok(t) => t,
            Err(e) => {
                log::error!("Failed to create tray icon: {}", e);
                return;
            }
        };

        log::info!("macOS tray icon created");

        let mut mounts: std::collections::HashMap<String, crate::messages::MountStateUpdateMsg> =
            std::collections::HashMap::new();
        let mut mount_items: Vec<(String, MenuItem, MenuItem)> = Vec::new();
        const MOUNT_INSERT_BASE: usize = 2;

        let menu_event_rx = MenuEvent::receiver();

        loop {
            if let Ok(()) = cancel_rx.try_recv() {
                break;
            }

            // Drain state updates
            let mut changed = false;
            while let Ok(msg) = state_rx.try_recv() {
                if let AgentToUfb::MountStateUpdate(update) = msg {
                    mounts.insert(update.mount_id.clone(), update);
                    changed = true;
                }
            }

            if changed {
                let total = mounts.len();
                let mounted_count = mounts.values().filter(|m| m.state == "mounted").count();
                let tip = if total == 0 {
                    "MediaMount".to_string()
                } else if mounted_count == total {
                    format!("MediaMount \u{2014} all mounted ({}/{})", mounted_count, total)
                } else {
                    format!("MediaMount \u{2014} {}/{} mounted", mounted_count, total)
                };
                let _ = _tray.set_tooltip(Some(&tip));

                let mut sorted_ids: Vec<String> = mounts.keys().cloned().collect();
                sorted_ids.sort();

                for mount_id in &sorted_ids {
                    let ms = &mounts[mount_id];
                    let dot = if ms.state == "mounted" { "\u{25CF}" } else { "\u{2715}" };
                    let label = format!("{} {} {}", ms.mount_id, dot, ms.state_detail);

                    if let Some(entry) = mount_items.iter().find(|(id, _, _)| id == mount_id) {
                        entry.1.set_text(&label);
                    } else {
                        let status_item = MenuItem::new(&label, false, None);
                        let restart_item = MenuItem::new(&format!("  Restart {}", mount_id), true, None);

                        let pos = MOUNT_INSERT_BASE + mount_items.len() * 2;
                        let _ = menu_handle.insert(&status_item as &dyn IsMenuItem, pos);
                        let _ = menu_handle.insert(&restart_item as &dyn IsMenuItem, pos + 1);

                        mount_items.push((mount_id.clone(), status_item, restart_item));
                    }
                }
            }

            // Process menu events
            if let Ok(event) = menu_event_rx.try_recv() {
                let cmd = if event.id == item_open_ufb.id() {
                    Some(TrayCommand::OpenUfb)
                } else if event.id == item_open_log.id() {
                    Some(TrayCommand::OpenLog)
                } else if event.id == item_autostart.id() {
                    let currently_enabled = crate::platform::is_auto_start_enabled();
                    if let Err(e) = crate::platform::set_auto_start(!currently_enabled) {
                        log::error!("Failed to toggle auto-start: {}", e);
                    }
                    item_autostart.set_text(
                        if !currently_enabled { "Disable auto-start" } else { "Start at login" }
                    );
                    None
                } else if event.id == item_quit.id() {
                    Some(TrayCommand::Quit)
                } else {
                    mount_items.iter()
                        .find(|(_, _, restart)| event.id == restart.id())
                        .map(|(mount_id, _, _)| {
                            TrayCommand::MountEvent(mount_id.clone(), crate::state::MountEvent::Restart)
                        })
                };

                if let Some(cmd) = cmd {
                    let _ = cmd_tx.blocking_send(cmd);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        log::info!("macOS tray icon removed");
    }

    fn load_icon() -> tray_icon::Icon {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                for name in &["icon.png", "../../src-tauri/icons/icon.png", "../Resources/icon.png"] {
                    let path = dir.join(name);
                    if let Ok(img) = image::open(&path) {
                        let rgba = img.into_rgba8();
                        let (w, h) = rgba.dimensions();
                        if let Ok(icon) = tray_icon::Icon::from_rgba(rgba.into_raw(), w, h) {
                            return icon;
                        }
                    }
                }
            }
        }

        tray_icon::Icon::from_rgba(vec![0u8; 16 * 16 * 4], 16, 16)
            .expect("Failed to create fallback icon")
    }
}
