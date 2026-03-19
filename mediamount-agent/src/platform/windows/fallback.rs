use crate::platform::SmbSession;
use windows::Win32::Foundation::WIN32_ERROR;

/// Windows SMB session via WNetAddConnection2W with no drive letter mapping.
pub struct WindowsSmbSession;

impl WindowsSmbSession {
    pub fn new() -> Self {
        Self
    }
}

impl SmbSession for WindowsSmbSession {
    fn ensure_session(
        &self,
        share_path: &str,
        _mount_point: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::NetworkManagement::WNet::{
            WNetAddConnection2W, NETRESOURCEW, RESOURCETYPE_DISK,
        };

        let remote_name: Vec<u16> = format!("{}\0", share_path).encode_utf16().collect();
        let user: Vec<u16> = format!("{}\0", username).encode_utf16().collect();
        let pass: Vec<u16> = format!("{}\0", password).encode_utf16().collect();

        let nr = NETRESOURCEW {
            dwType: RESOURCETYPE_DISK,
            lpLocalName: windows::core::PWSTR::null(), // session-only, no drive letter
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

        use windows::Win32::NetworkManagement::WNet::NET_CONNECT_FLAGS;
        let result = unsafe { WNetAddConnection2W(&nr, pass_ptr, user_ptr, NET_CONNECT_FLAGS(0)) };

        if result != WIN32_ERROR(0) {
            // ERROR_SESSION_CREDENTIAL_CONFLICT (1219) — session already exists, that's fine
            if result == WIN32_ERROR(1219) {
                log::info!("SMB session already exists for {}", share_path);
                return Ok(());
            }
            return Err(format!(
                "WNetAddConnection2W failed for {}: error {:?}",
                share_path, result
            ));
        }

        log::info!("SMB session established for {}", share_path);
        Ok(())
    }
}
