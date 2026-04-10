use windows::Win32::Foundation::WIN32_ERROR;

/// Establish an authenticated SMB session to a NAS share without mapping a drive letter.
/// This enables UNC path access (for sync watcher/filter) using the provided credentials.
pub fn establish_smb_session(
    share_path: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::NetworkManagement::WNet::{
        WNetAddConnection2W, NETRESOURCEW, NET_CONNECT_FLAGS,
    };

    let remote_name: Vec<u16> = format!("{}\0", share_path).encode_utf16().collect();
    let user: Vec<u16> = format!("{}\0", username).encode_utf16().collect();
    let pass: Vec<u16> = format!("{}\0", password).encode_utf16().collect();

    let nr = NETRESOURCEW {
        dwType: windows::Win32::NetworkManagement::WNet::RESOURCETYPE_ANY,
        lpLocalName: windows::core::PWSTR::null(),
        lpRemoteName: windows::core::PWSTR(remote_name.as_ptr() as *mut _),
        ..Default::default()
    };

    let user_ptr = if username.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(user.as_ptr())
    };
    let pass_ptr = if password.is_empty() {
        PCWSTR::null()
    } else {
        PCWSTR(pass.as_ptr())
    };

    let result = unsafe { WNetAddConnection2W(&nr, pass_ptr, user_ptr, NET_CONNECT_FLAGS(0)) };

    if result == WIN32_ERROR(0) {
        log::info!("SMB session established for {}", share_path);
        return Ok(());
    }

    // ERROR_SESSION_CREDENTIAL_CONFLICT (1219)
    if result == WIN32_ERROR(1219) {
        // Session already exists (possibly from a drive mount or another process).
        // This is fine — the existing session will work for UNC path access.
        log::info!("SMB session already exists for {} (reusing)", share_path);
        return Ok(());
    }

    // ERROR_ALREADY_ASSIGNED (85) — shouldn't happen without a drive letter, but handle it
    if result == WIN32_ERROR(85) {
        log::info!("SMB session already established for {}", share_path);
        return Ok(());
    }

    Err(format!(
        "Failed to establish SMB session for {}: error {:?}",
        share_path, result
    ))
}

/// Disconnect a deviceless SMB session to a NAS share.
pub fn disconnect_smb_session(share_path: &str) -> Result<(), String> {
    use windows::Win32::NetworkManagement::WNet::{WNetCancelConnection2W, NET_CONNECT_FLAGS};

    let remote_name: Vec<u16> = format!("{}\0", share_path).encode_utf16().collect();

    let result = unsafe {
        WNetCancelConnection2W(
            windows::core::PCWSTR(remote_name.as_ptr()),
            NET_CONNECT_FLAGS(0),
            false, // don't force — other processes might be using this session
        )
    };

    if result == WIN32_ERROR(0) {
        log::info!("SMB session disconnected for {}", share_path);
        Ok(())
    } else if result == WIN32_ERROR(2250) {
        // ERROR_NOT_CONNECTED
        log::debug!("No SMB session to disconnect for {}", share_path);
        Ok(())
    } else {
        // Non-fatal — log but don't fail
        log::warn!(
            "Failed to disconnect SMB session for {}: error {:?}",
            share_path, result
        );
        Ok(())
    }
}

/// Disconnect a mapped drive letter via WNetCancelConnection2W.
pub fn disconnect_drive(drive_letter: &str) -> Result<(), String> {
    use windows::Win32::NetworkManagement::WNet::{WNetCancelConnection2W, NET_CONNECT_FLAGS};

    let local_name: Vec<u16> = format!("{}:\0", drive_letter).encode_utf16().collect();

    let result = unsafe {
        WNetCancelConnection2W(
            windows::core::PCWSTR(local_name.as_ptr()),
            NET_CONNECT_FLAGS(0),  // no persistent flag removal
            true,                  // force disconnect even if files open
        )
    };

    if result == WIN32_ERROR(0) {
        log::info!("Disconnected drive {}:\\", drive_letter);
        Ok(())
    } else if result == WIN32_ERROR(2250) {
        // ERROR_NOT_CONNECTED — already disconnected, that's fine
        log::debug!("Drive {}:\\ was not connected", drive_letter);
        Ok(())
    } else {
        Err(format!(
            "WNetCancelConnection2W failed for {}:\\: error {:?}",
            drive_letter, result
        ))
    }
}
