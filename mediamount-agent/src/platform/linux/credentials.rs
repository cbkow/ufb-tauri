use crate::platform::CredentialStore;
use std::collections::HashMap;
use std::path::PathBuf;

/// File-based credential store for Linux.
/// Stores credentials in ~/.local/share/ufb/credentials.json (chmod 600).
/// Each key maps to {"u":"username","p":"password"}.
pub struct LinuxCredentialStore;

impl LinuxCredentialStore {
    pub fn new() -> Self {
        Self
    }
}

fn cred_file_path() -> PathBuf {
    let dir = if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local/share/ufb")
    } else {
        std::path::PathBuf::from("/tmp/ufb")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir.join("credentials.json")
}

fn load_cred_file() -> HashMap<String, serde_json::Value> {
    let path = cred_file_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        HashMap::new()
    }
}

fn save_cred_file(data: &HashMap<String, serde_json::Value>) -> Result<(), String> {
    let path = cred_file_path();
    let json = serde_json::to_string_pretty(data)
        .map_err(|e| format!("Failed to serialize credentials: {}", e))?;
    std::fs::write(&path, &json)
        .map_err(|e| format!("Failed to write credentials file: {}", e))?;

    // chmod 600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

impl CredentialStore for LinuxCredentialStore {
    fn store(&self, key: &str, username: &str, password: &str) -> Result<(), String> {
        let mut data = load_cred_file();
        data.insert(
            key.to_string(),
            serde_json::json!({ "u": username, "p": password }),
        );
        save_cred_file(&data)?;
        log::info!("Stored credentials for {}", key);
        Ok(())
    }

    fn retrieve(&self, key: &str) -> Result<(String, String), String> {
        let data = load_cred_file();
        let entry = data.get(key)
            .ok_or_else(|| format!("No credentials found for {}", key))?;

        let username = entry["u"].as_str().unwrap_or("").to_string();
        let password = entry["p"].as_str().unwrap_or("").to_string();
        Ok((username, password))
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let mut data = load_cred_file();
        data.remove(key);
        save_cred_file(&data)?;
        log::info!("Deleted credentials for {}", key);
        Ok(())
    }
}
