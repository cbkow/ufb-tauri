use windows::Win32::Foundation::WIN32_ERROR;

/// Map a drive letter to an SMB share via WNetAddConnection2W.
/// This creates a proper Windows network drive visible in Explorer.
pub fn connect_drive(
    drive_letter: &str,
    share_path: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::NetworkManagement::WNet::{
        WNetAddConnection2W, NETRESOURCEW, RESOURCETYPE_DISK, NET_CONNECT_FLAGS,
    };

    let local_name: Vec<u16> = format!("{}:\0", drive_letter).encode_utf16().collect();
    let remote_name: Vec<u16> = format!("{}\0", share_path).encode_utf16().collect();
    let user: Vec<u16> = format!("{}\0", username).encode_utf16().collect();
    let pass: Vec<u16> = format!("{}\0", password).encode_utf16().collect();

    let nr = NETRESOURCEW {
        dwType: RESOURCETYPE_DISK,
        lpLocalName: windows::core::PWSTR(local_name.as_ptr() as *mut _),
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
        log::info!("Mapped {}:\\ → {}", drive_letter, share_path);
        return Ok(());
    }

    // ERROR_ALREADY_ASSIGNED (85) — drive letter already mapped
    if result == WIN32_ERROR(85) {
        log::info!("Drive {}:\\ already assigned, will disconnect and retry", drive_letter);
        let _ = disconnect_drive(drive_letter);
        let retry = unsafe { WNetAddConnection2W(&nr, pass_ptr, user_ptr, NET_CONNECT_FLAGS(0)) };
        if retry == WIN32_ERROR(0) {
            log::info!("Mapped {}:\\ → {} (after disconnect)", drive_letter, share_path);
            return Ok(());
        }
        return Err(format!(
            "WNetAddConnection2W retry failed for {}:\\ → {}: error {:?}",
            drive_letter, share_path, retry
        ));
    }

    // ERROR_SESSION_CREDENTIAL_CONFLICT (1219) — session exists with different creds
    if result == WIN32_ERROR(1219) {
        return Err(format!(
            "Credential conflict for {}: an existing session uses different credentials. \
             Disconnect existing connections to {} first.",
            share_path, share_path
        ));
    }

    Err(format!(
        "WNetAddConnection2W failed for {}:\\ → {}: error {:?}",
        drive_letter, share_path, result
    ))
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
