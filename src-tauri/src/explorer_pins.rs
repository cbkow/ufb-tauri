//! Register folders as Explorer navigation pane entries via CLSID.
//!
//! Creates per-user registry entries (HKCU) that make folders appear in
//! Explorer's nav pane with a custom name and icon. No admin rights needed.

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Collect all nav pin entries from bookmarks (Jobs folders) and mount configs.
pub fn collect_nav_pins(state: &crate::app_state::AppState) -> Vec<NavPinEntry> {
    let mut entries = Vec::new();

    // 1. Mount configs — volume paths (C:\Volumes\ufb\{shareName})
    let mount_cfg = crate::mount_client::load_mount_config();
    for m in &mount_cfg.mounts {
        if !m.enabled {
            continue;
        }
        // Use volume path: derive share name from NAS path (last component)
        let share_name = m.nas_share_path
            .trim_end_matches('\\')
            .split('\\')
            .last()
            .filter(|s| !s.is_empty())
            .unwrap_or(&m.id);
        let volume_path = format!(r"C:\Volumes\ufb\{}", share_name);
        entries.push(NavPinEntry {
            name: m.display_name.clone(),
            target_path: volume_path,
        });
    }

    // 2. Bookmarks marked as project/Jobs folders
    if let Ok(bookmarks) = state.bookmark_manager.get_all_bookmarks() {
        for bm in bookmarks {
            if bm.is_project_folder {
                entries.push(NavPinEntry {
                    name: bm.display_name,
                    target_path: bm.path,
                });
            }
        }
    }

    entries
}

/// Our app icon filename (expected next to the exe).
const ICON_FILENAME: &str = "icon.ico";

/// Represents a folder to pin in Explorer's nav pane.
pub struct NavPinEntry {
    pub name: String,
    pub target_path: String,
}

/// Sync all nav pane pins: register entries for the given list, remove stale ones.
pub fn sync_nav_pins(entries: &[NavPinEntry]) -> Result<(), String> {
    let icon_path = find_icon_path();

    // Read existing UFB pins from registry so we can remove stale ones
    let existing_clsids = list_registered_clsids();

    let mut wanted_clsids = Vec::new();

    for entry in entries {
        if entry.target_path.is_empty() {
            continue;
        }
        let clsid = deterministic_clsid(&entry.target_path);
        wanted_clsids.push(clsid.clone());
        register_nav_pin(&clsid, &entry.name, &entry.target_path, icon_path.as_deref())?;
    }

    // Remove CLSIDs that are registered but no longer wanted
    for old_clsid in &existing_clsids {
        if !wanted_clsids.contains(old_clsid) {
            let _ = unregister_nav_pin(old_clsid);
        }
    }

    Ok(())
}

/// Remove all UFB nav pane pins.
pub fn remove_all_nav_pins() {
    for clsid in list_registered_clsids() {
        let _ = unregister_nav_pin(&clsid);
    }
}

/// Generate a deterministic CLSID from a path string.
/// Uses a simple hash to fill a valid hex GUID.
/// We use `0FB` in the first segment as a marker to identify our CLSIDs
/// (looks like "UFB" but is valid hex: 0FB...).
fn deterministic_clsid(path: &str) -> String {
    // Simple FNV-1a-like hash to get 128 bits from the path
    let normalized = path.replace('/', "\\").to_lowercase();
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x100000001b3;
    for b in normalized.bytes() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 ^= b as u64;
        h2 = h2.wrapping_mul(0xcbf29ce484222325);
    }

    // Format as {0FBXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}
    // Prefix 0FB (valid hex) so we can identify our CLSIDs in the registry
    format!(
        "{{0FB{:05X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        (h1 >> 44) & 0xFFFFF,
        (h1 >> 28) & 0xFFFF,
        (h1 >> 12) & 0xFFFF,
        h2 & 0xFFFF,
        (h2 >> 16) & 0xFFFFFFFFFFFF,
    )
}

/// List all CLSID values under Desktop\NameSpace that look like ours (contain "UFB" prefix).
fn list_registered_clsids() -> Vec<String> {
    let output = reg_cmd(&[
        "query",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace",
    ]);

    let mut clsids = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(clsid_part) = trimmed.strip_prefix(
            r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace\",
        ) {
            if clsid_part.contains("0FB") {
                clsids.push(clsid_part.to_string());
            }
        }
    }
    clsids
}

fn register_nav_pin(
    clsid: &str,
    name: &str,
    target_path: &str,
    icon_path: Option<&str>,
) -> Result<(), String> {
    let clsid_key = format!(r"HKCU\Software\Classes\CLSID\{}", clsid);

    // 1. CLSID base key with name
    reg_add(&clsid_key, None, name)?;
    reg_add_dword(&clsid_key, "System.IsPinnedToNameSpaceTree", 1)?;
    reg_add_dword(&clsid_key, "SortOrderIndex", 0x42)?;

    // 2. Default icon
    let icon_value = icon_path.map_or_else(
        || "%SystemRoot%\\System32\\shell32.dll,3".to_string(),
        |p| format!("{},0", p),
    );
    reg_add(&format!(r"{}\DefaultIcon", clsid_key), None, &icon_value)?;

    // 3. InProcServer32 — delegate to shell32
    reg_add(
        &format!(r"{}\InProcServer32", clsid_key),
        None,
        "%SystemRoot%\\System32\\shell32.dll",
    )?;
    reg_add(
        &format!(r"{}\InProcServer32", clsid_key),
        Some("ThreadingModel"),
        "Both",
    )?;

    // 4. Instance — point to real folder
    reg_add(
        &format!(r"{}\Instance", clsid_key),
        Some("CLSID"),
        "{0E5AAE11-A475-4c5b-AB00-C66DE400274E}",
    )?;
    let init_bag = format!(r"{}\Instance\InitPropertyBag", clsid_key);
    reg_add_dword(&init_bag, "Attributes", 0x11)?;
    reg_add(&init_bag, Some("TargetFolderPath"), target_path)?;

    // 5. ShellFolder attributes
    let sf_key = format!(r"{}\ShellFolder", clsid_key);
    reg_add_dword(&sf_key, "Attributes", 0xF080004D)?;
    reg_add_dword(&sf_key, "FolderValueFlags", 0x28)?;

    // 6. Register in Desktop\NameSpace
    let ns_key = format!(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace\{}",
        clsid
    );
    reg_add(&ns_key, None, name)?;

    // 7. Hide from desktop surface (only show in nav pane)
    reg_add_dword(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\HideDesktopIcons\NewStartPanel",
        clsid,
        1,
    )?;

    log::info!("Registered nav pin: {} → {}", name, target_path);
    Ok(())
}

fn unregister_nav_pin(clsid: &str) -> Result<(), String> {
    // Remove CLSID registration
    let _ = reg_delete(&format!(r"HKCU\Software\Classes\CLSID\{}", clsid));

    // Remove from Desktop\NameSpace
    let _ = reg_delete(&format!(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace\{}",
        clsid
    ));

    log::info!("Unregistered nav pin: {}", clsid);
    Ok(())
}

fn find_icon_path() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;

    // Next to the app exe
    let icon = dir.join(ICON_FILENAME);
    if icon.exists() {
        return Some(icon.to_string_lossy().to_string());
    }

    // Dev mode: check src-tauri/icons/
    let dev_icon = dir.join("../../icons/icon.ico");
    if let Ok(canon) = std::fs::canonicalize(&dev_icon) {
        if canon.exists() {
            return Some(canon.to_string_lossy().to_string());
        }
    }

    None
}

// ── Registry helpers ──

fn reg_cmd(args: &[&str]) -> String {
    #[cfg(windows)]
    {
        let output = Command::new("reg")
            .args(args)
            .creation_flags(0x08000000)
            .output();
        match output {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => String::new(),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        String::new()
    }
}

fn reg_add(key: &str, value_name: Option<&str>, data: &str) -> Result<(), String> {
    let mut args = vec!["add", key];
    if let Some(vn) = value_name {
        args.extend_from_slice(&["/v", vn]);
    } else {
        args.push("/ve");
    }
    args.extend_from_slice(&["/t", "REG_SZ", "/d", data, "/f"]);

    run_reg(&args)
}

fn reg_add_dword(key: &str, value_name: &str, data: u32) -> Result<(), String> {
    let data_str = format!("{}", data);
    let args = vec![
        "add", key, "/v", value_name, "/t", "REG_DWORD", "/d", &data_str, "/f",
    ];
    run_reg(&args)
}

fn reg_delete(key: &str) -> Result<(), String> {
    run_reg(&["delete", key, "/f"])
}

fn run_reg(args: &[&str]) -> Result<(), String> {
    #[cfg(windows)]
    {
        let output = Command::new("reg")
            .args(args)
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| format!("Failed to run reg: {}", e))?;

        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "reg failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        Ok(())
    }
}
