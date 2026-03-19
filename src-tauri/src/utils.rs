use std::path::PathBuf;

/// Get the app data directory (platform-specific).
/// Windows: %LOCALAPPDATA%/ufb/
/// macOS: ~/Library/Application Support/ufb/
/// Linux: ~/.config/ufb/
pub fn get_app_data_dir() -> PathBuf {
    let base = dirs::config_local_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("ufb");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Get or create a persistent device ID.
pub fn get_device_id() -> String {
    let id_path = get_app_data_dir().join("device_id.txt");
    if let Ok(id) = std::fs::read_to_string(&id_path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::write(&id_path, &id);
    id
}

/// Get the database file path.
pub fn get_database_path() -> PathBuf {
    get_app_data_dir().join("ufb_v2.db")
}

/// Get the settings file path.
pub fn get_settings_path() -> PathBuf {
    get_app_data_dir().join("settings.json")
}

/// Get current time in milliseconds since epoch.
pub fn current_time_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Detect current OS tag for URI construction.
pub fn current_os_tag() -> &'static str {
    #[cfg(target_os = "windows")]
    { "win" }
    #[cfg(target_os = "macos")]
    { "mac" }
    #[cfg(target_os = "linux")]
    { "lin" }
}

/// Build a UFB URI with OS prefix: ufb:///{os}/{path}
/// Example: ufb:///win/C:/Users/Chris/Desktop
pub fn build_path_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let encoded = urlencoding::encode(&normalized);
    format!("ufb:///{}/{}", current_os_tag(), encoded)
}

/// Build a Union URI with OS prefix: union:///{os}/{path}
pub fn build_union_uri(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let encoded = urlencoding::encode(&normalized);
    format!("union:///{}/{}", current_os_tag(), encoded)
}

/// Parsed result from a ufb:// or union:// URI.
pub struct ParsedUri {
    pub source_os: String,
    pub path: String,
}

/// Parse a UFB or Union URI back to its source OS + path.
/// Input: "ufb:///win/C%3A/Users/Chris" → ParsedUri { source_os: "win", path: "C:/Users/Chris" }
/// Also handles legacy URIs without OS prefix for backwards compat.
pub fn parse_path_uri(uri: &str) -> Option<ParsedUri> {
    let stripped = uri
        .strip_prefix("ufb:///")
        .or_else(|| uri.strip_prefix("union:///"))?;

    // Check for OS prefix: win/, mac/, lin/
    let (source_os, encoded_path) = if stripped.starts_with("win/") {
        ("win".to_string(), &stripped[4..])
    } else if stripped.starts_with("mac/") {
        ("mac".to_string(), &stripped[4..])
    } else if stripped.starts_with("lin/") {
        ("lin".to_string(), &stripped[4..])
    } else {
        // Legacy URI without OS prefix — assume current OS
        (current_os_tag().to_string(), stripped)
    };

    let decoded = urlencoding::decode(encoded_path).ok()?;
    Some(ParsedUri {
        source_os,
        path: decoded.to_string(),
    })
}

/// Translate a path from source_os format to target_os format using mapping rules.
pub fn translate_path_to(
    source_os: &str,
    target_os: &str,
    path: &str,
    mappings: &[crate::settings::PathMapping],
) -> String {
    // If same OS, no translation needed
    if source_os == target_os {
        return to_native_path(path, target_os);
    }

    // Try each mapping rule
    for mapping in mappings {
        let source_prefix = match source_os {
            "win" => &mapping.win,
            "mac" => &mapping.mac,
            "lin" => &mapping.lin,
            _ => continue,
        };
        let target_prefix = match target_os {
            "win" => &mapping.win,
            "mac" => &mapping.mac,
            "lin" => &mapping.lin,
            _ => continue,
        };

        if source_prefix.is_empty() || target_prefix.is_empty() {
            continue;
        }

        // Normalize for comparison (forward slashes, case-insensitive on Windows)
        let norm_path = path.replace('\\', "/");
        let norm_source = source_prefix.replace('\\', "/");

        let matches = if source_os == "win" {
            norm_path.to_lowercase().starts_with(&norm_source.to_lowercase())
        } else {
            norm_path.starts_with(&norm_source)
        };

        if matches {
            let remainder = &norm_path[norm_source.len()..];
            let translated = format!("{}{}", target_prefix, remainder);
            return to_native_path(&translated, target_os);
        }
    }

    // No mapping found — just convert to native path separators
    to_native_path(path, target_os)
}

/// Translate a path from one OS to the local OS using path mapping rules.
/// Each rule is a triplet (win_pattern, mac_pattern, lin_pattern).
/// We find which rule matches the source OS prefix, then swap to the local OS prefix.
pub fn translate_path(
    source_os: &str,
    path: &str,
    mappings: &[crate::settings::PathMapping],
) -> String {
    translate_path_to(source_os, current_os_tag(), path, mappings)
}

/// Convert a native OS path to Windows-canonical format for DB storage.
/// On Windows this is a no-op. On Linux, translates /mnt/nas/... → R:\...
pub fn to_canonical_path(native_path: &str, mappings: &[crate::settings::PathMapping]) -> String {
    translate_path_to(current_os_tag(), "win", native_path, mappings)
}

/// Convert a Windows-canonical DB path to the local native format.
/// On Windows this is a no-op. On Linux, translates R:\... → /mnt/nas/...
pub fn from_canonical_path(db_path: &str, mappings: &[crate::settings::PathMapping]) -> String {
    translate_path_to("win", current_os_tag(), db_path, mappings)
}

/// Convert forward-slash path to native OS path separators.
fn to_native_path(path: &str, os: &str) -> String {
    if os == "win" {
        path.replace('/', "\\")
    } else {
        path.replace('\\', "/")
    }
}
