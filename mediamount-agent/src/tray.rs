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

        #[cfg(not(any(windows, target_os = "linux")))]
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
    const IDM_RESTART: u32 = 1001;
    const IDM_OPEN_UFB: u32 = 2001;
    const IDM_OPEN_LOG: u32 = 2002;
    const IDM_TOGGLE_AUTOSTART: u32 = 2003;
    const IDM_QUIT: u32 = 9001;

    struct TrayState {
        cmd_tx: mpsc::Sender<TrayCommand>,
        mount_id: Option<String>,
        status_text: String,
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
                    mount_id: None,
                    status_text: "Initializing...".into(),
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
                    while let Ok(msg) = receivers.0.try_recv() {
                        if let AgentToUfb::MountStateUpdate(update) = msg {
                            let mut lock = get_tray_state().lock().unwrap();
                            if let Some(ref mut state) = *lock {
                                state.mount_id = Some(update.mount_id.clone());
                                state.status_text = update.state_detail.clone();

                                // Update tooltip
                                let tip = format!("MediaMount — {}", update.state_detail);
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

        // Status
        append_menu_item(menu, 0, &state.status_text, MF_DISABLED | MF_GRAYED);

        AppendMenuW(menu, MF_SEPARATOR, 0, None).ok();

        // Mount controls
        if state.mount_id.is_some() {
            append_menu_item(menu, IDM_RESTART, "Restart", MF_STRING);
            AppendMenuW(menu, MF_SEPARATOR, 0, None).ok();
        }

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
            IDM_RESTART => state
                .mount_id
                .as_ref()
                .map(|id| TrayCommand::MountEvent(id.clone(), crate::state::MountEvent::Restart)),
            IDM_OPEN_UFB => Some(TrayCommand::OpenUfb),
            IDM_OPEN_LOG => Some(TrayCommand::OpenLog),
            IDM_TOGGLE_AUTOSTART => {
                let currently_enabled = crate::platform::is_auto_start_enabled();
                if let Err(e) = crate::platform::set_auto_start(!currently_enabled) {
                    log::error!("Failed to toggle auto-start: {}", e);
                }
                None // No command needed back to tokio
            }
            IDM_QUIT => Some(TrayCommand::Quit),
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
        use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem, MenuEvent};

        // Initialize GTK (required by tray-icon on Linux)
        gtk::init().expect("Failed to init GTK for tray");

        // Build menu
        let menu = Menu::new();
        let item_status = MenuItem::new("Initializing...", false, None);
        let item_restart = MenuItem::new("Restart", true, None);
        let item_open_ufb = MenuItem::new("Open UFB", true, None);
        let item_open_log = MenuItem::new("Open log", true, None);
        let item_autostart = MenuItem::new(
            if crate::platform::is_auto_start_enabled() { "Disable auto-start" } else { "Start at login" },
            true,
            None,
        );
        let item_quit = MenuItem::new("Quit", true, None);

        let _ = menu.append(&item_status);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_restart);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_open_ufb);
        let _ = menu.append(&item_open_log);
        let _ = menu.append(&item_autostart);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&item_quit);

        // Try to load icon
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

        let mut mount_id: Option<String> = None;
        let menu_event_rx = MenuEvent::receiver();

        // GTK main loop — poll for events
        loop {
            // Process GTK events
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }

            // Check for cancellation
            if let Ok(()) = cancel_rx.try_recv() {
                break;
            }

            // Drain state updates
            while let Ok(msg) = state_rx.try_recv() {
                if let AgentToUfb::MountStateUpdate(update) = msg {
                    mount_id = Some(update.mount_id.clone());
                    item_status.set_text(&update.state_detail);
                }
            }

            // Process menu events
            if let Ok(event) = menu_event_rx.try_recv() {
                let cmd = if event.id == item_restart.id() {
                    mount_id.as_ref().map(|id| {
                        TrayCommand::MountEvent(id.clone(), crate::state::MountEvent::Restart)
                    })
                } else if event.id == item_open_ufb.id() {
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
                    None
                };

                if let Some(cmd) = cmd {
                    let _ = cmd_tx.blocking_send(cmd);
                }
            }

            // Sleep briefly to avoid busy-polling
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
