use crate::platform::FallbackMount;
use windows::Win32::Foundation::WIN32_ERROR;

/// Windows SMB mapping via WNetAddConnection2W / WNetCancelConnection2W.
pub struct WindowsFallbackMount;

impl WindowsFallbackMount {
    pub fn new() -> Self {
        Self
    }
}

impl FallbackMount for WindowsFallbackMount {
    fn map(
        &self,
        share_path: &str,
        drive_letter: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::NetworkManagement::WNet::{
            WNetAddConnection2W, NETRESOURCEW, RESOURCETYPE_DISK,
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

        use windows::Win32::NetworkManagement::WNet::NET_CONNECT_FLAGS;
        let result = unsafe { WNetAddConnection2W(&nr, pass_ptr, user_ptr, NET_CONNECT_FLAGS(0)) };

        if result != WIN32_ERROR(0) {
            // Error 85 = ERROR_ALREADY_ASSIGNED — drive letter already mapped
            if result == WIN32_ERROR(85) {
                log::info!("Drive {}:\\ already mapped, reusing", drive_letter);
                return Ok(());
            }
            return Err(format!(
                "WNetAddConnection2W failed for {}:\\ → {}: error {:?}",
                drive_letter, share_path, result
            ));
        }

        log::info!("Mapped {}:\\ → {}", drive_letter, share_path);
        Ok(())
    }

    fn unmap(&self, drive_letter: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::NetworkManagement::WNet::{
            WNetCancelConnection2W, NET_CONNECT_FLAGS,
        };

        let local_name: Vec<u16> = format!("{}:\0", drive_letter).encode_utf16().collect();

        let result = unsafe {
            WNetCancelConnection2W(PCWSTR(local_name.as_ptr()), NET_CONNECT_FLAGS(0), true)
        };

        if result != WIN32_ERROR(0) {
            // Error 2250 = not connected — that's fine
            if result == WIN32_ERROR(2250) {
                log::debug!("Drive {}:\\ was not mapped", drive_letter);
                return Ok(());
            }
            return Err(format!(
                "WNetCancelConnection2W failed for {}:\\: error {:?}",
                drive_letter, result
            ));
        }

        log::info!("Unmapped {}:\\", drive_letter);
        Ok(())
    }

    fn is_mapped(&self, drive_letter: &str) -> Result<bool, String> {
        let path = format!("{}:\\", drive_letter);
        Ok(std::path::Path::new(&path).exists())
    }
}
