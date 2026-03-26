//! Show the native Windows Explorer context menu for a file or folder.
//!
//! Uses the IContextMenu COM interface to display the same right-click menu
//! that Explorer shows.

#[cfg(windows)]
pub fn show_shell_context_menu(path: &str) -> Result<(), String> {
    use std::path::Path;

    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    log::info!("show_shell_context_menu: {}", path);

    // Capture cursor position NOW (before the thread spawn delay)
    let (cursor_x, cursor_y) = unsafe {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        (pt.x, pt.y)
    };

    // Run on a dedicated STA thread. TrackPopupMenuEx blocks (pumps its
    // own message loop) so we must not call it on Tauri's async runtime.
    let path_owned = path.to_string();
    let handle = std::thread::spawn(move || {
        show_menu_blocking(&path_owned, cursor_x, cursor_y)
    });
    handle.join().map_err(|_| "Shell context menu thread panicked".to_string())?
}

#[cfg(windows)]
fn show_menu_blocking(path: &str, cursor_x: i32, cursor_y: i32) -> Result<(), String> {
    use windows::core::{w, HSTRING, PCSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        IContextMenu, IShellItem, SHCreateItemFromParsingName, BHID_SFUIObject,
        CMINVOKECOMMANDINFO, CMF_NORMAL, CMF_EXPLORE,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    // Minimal wndproc for the hidden helper window
    unsafe extern "system" fn wnd_proc(
        hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
    ) -> LRESULT {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    unsafe {
        // Initialise COM (STA) for this thread
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let we_init = hr.is_ok();

        let result = (|| -> Result<(), String> {
            // Create a hidden popup window on THIS thread (not message-only).
            // TrackPopupMenuEx requires an hwnd whose message queue belongs
            // to the calling thread.  Message-only windows (HWND_MESSAGE)
            // don't participate in the desktop z-order and cause dismiss
            // problems, so we use a zero-size WS_POPUP instead.
            let class_name = w!("UFBShellCtxMenuHost");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassExW(&wc); // OK if already registered

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_POPUP,       // regular popup, not message-only
                0, 0, 0, 0,    // zero-size, invisible
                None,           // no parent
                None,
                None,
                None,
            ).map_err(|e| format!("CreateWindowExW failed: {}", e))?;

            // Get IShellItem for the path
            let wide_path = HSTRING::from(path);
            let shell_item: IShellItem = SHCreateItemFromParsingName(&wide_path, None)
                .map_err(|e| format!("SHCreateItemFromParsingName failed: {}", e))?;

            // Get IContextMenu from the shell item
            let context_menu: IContextMenu = shell_item
                .BindToHandler(None, &BHID_SFUIObject)
                .map_err(|e| format!("BindToHandler for IContextMenu failed: {}", e))?;

            // Create a popup menu and populate it
            let hmenu = CreatePopupMenu()
                .map_err(|e| format!("CreatePopupMenu failed: {}", e))?;

            context_menu.QueryContextMenu(hmenu, 0, 1, 0x7FFF, CMF_NORMAL | CMF_EXPLORE)
                .map_err(|e| format!("QueryContextMenu failed: {}", e))?;

            // MSDN requirement: the owner window must be foreground before
            // calling TrackPopupMenuEx, otherwise the menu won't dismiss
            // when the user clicks outside it.
            let _ = SetForegroundWindow(hwnd);

            // Show the menu at the captured cursor position (blocking call)
            let cmd = TrackPopupMenuEx(
                hmenu,
                (TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN).0,
                cursor_x,
                cursor_y,
                hwnd,
                None,
            );

            // MSDN requirement: post WM_NULL so the internal menu-tracking
            // state machine finishes cleanly when the menu is dismissed.
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));

            if cmd.0 > 0 {
                let verb_index = (cmd.0 as u32).wrapping_sub(1);
                let ici = CMINVOKECOMMANDINFO {
                    cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                    hwnd,
                    lpVerb: PCSTR(verb_index as usize as *const u8),
                    nShow: 1, // SW_SHOWNORMAL
                    ..Default::default()
                };

                if let Err(e) = context_menu.InvokeCommand(&ici) {
                    log::warn!("InvokeCommand failed: {}", e);
                }
            }

            // Explicitly drop COM objects before destroying the window
            drop(context_menu);
            drop(shell_item);

            let _ = DestroyMenu(hmenu);
            let _ = DestroyWindow(hwnd);
            Ok(())
        })();

        if we_init {
            CoUninitialize();
        }

        result
    }
}

#[cfg(not(windows))]
pub fn show_shell_context_menu(_path: &str) -> Result<(), String> {
    Err("Shell context menu is only available on Windows".to_string())
}
