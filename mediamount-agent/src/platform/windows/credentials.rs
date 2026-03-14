use crate::platform::CredentialStore;

/// Windows Credential Manager implementation.
pub struct WindowsCredentialStore;

impl WindowsCredentialStore {
    pub fn new() -> Self {
        Self
    }
}

impl CredentialStore for WindowsCredentialStore {
    fn store(&self, key: &str, username: &str, password: &str) -> Result<(), String> {
        use windows::Win32::Security::Credentials::{
            CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        };

        let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();
        let user: Vec<u16> = format!("{}\0", username).encode_utf16().collect();
        let pass_bytes: Vec<u8> = password.as_bytes().to_vec();

        let cred = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: windows::core::PWSTR(target.as_ptr() as *mut _),
            UserName: windows::core::PWSTR(user.as_ptr() as *mut _),
            CredentialBlobSize: pass_bytes.len() as u32,
            CredentialBlob: pass_bytes.as_ptr() as *mut _,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };

        unsafe {
            CredWriteW(&cred, 0)
                .map_err(|e| format!("CredWriteW failed for {}: {}", key, e))?;
        }

        log::info!("Stored credentials for {}", key);
        Ok(())
    }

    fn retrieve(&self, key: &str) -> Result<(String, String), String> {
        use windows::Win32::Security::Credentials::{
            CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
        };

        let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();
        let mut cred_ptr: *mut CREDENTIALW = std::ptr::null_mut();

        unsafe {
            CredReadW(
                windows::core::PCWSTR(target.as_ptr()),
                CRED_TYPE_GENERIC,
                0,
                &mut cred_ptr,
            )
            .map_err(|e| format!("CredReadW failed for {}: {}", key, e))?;

            let cred = &*cred_ptr;

            let username = if cred.UserName.is_null() {
                String::new()
            } else {
                cred.UserName.to_string().unwrap_or_default()
            };

            let password = if cred.CredentialBlob.is_null() || cred.CredentialBlobSize == 0 {
                String::new()
            } else {
                let slice = std::slice::from_raw_parts(
                    cred.CredentialBlob,
                    cred.CredentialBlobSize as usize,
                );
                String::from_utf8_lossy(slice).to_string()
            };

            CredFree(cred_ptr as *const _);

            Ok((username, password))
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

        let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();

        unsafe {
            CredDeleteW(windows::core::PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, 0)
                .map_err(|e| format!("CredDeleteW failed for {}: {}", key, e))?;
        }

        log::info!("Deleted credentials for {}", key);
        Ok(())
    }
}
