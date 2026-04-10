/// Elevation helper — launches the agent exe with admin privileges via ShellExecuteW "runas".

/// Launch the current agent exe elevated with --create-symlinks flag.
/// Shows a single UAC prompt. The elevated instance creates symlinks and exits.
/// Returns immediately — the elevated process runs asynchronously.
pub fn launch_elevated_symlink_creation() -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::Foundation::HWND;

    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe path: {}", e))?;

    let exe_str = exe.to_string_lossy().to_string();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = format!("{}\0", exe_str).encode_utf16().collect();
    let params: Vec<u16> = "--create-symlinks\0".encode_utf16().collect();

    log::info!("[elevation] Requesting UAC elevation for symlink creation");

    let result = unsafe {
        ShellExecuteW(
            HWND::default(),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
        )
    };

    // ShellExecuteW returns HINSTANCE > 32 on success
    let code = result.0 as usize;
    if code > 32 {
        log::info!("[elevation] Elevated process launched successfully");
        Ok(())
    } else {
        Err(format!(
            "ShellExecuteW failed with code {} (user may have cancelled UAC)",
            code
        ))
    }
}
