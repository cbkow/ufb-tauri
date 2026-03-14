use crate::file_ops::FileEntry;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Search for files using platform-specific search tools.
/// Windows: Everything (es.exe), macOS: mdfind, Linux: locate/fd fallback to walkdir.
pub fn search_files(query: &str, scope_path: Option<&str>) -> Result<Vec<FileEntry>, String> {
    #[cfg(target_os = "windows")]
    {
        search_everything(query, scope_path)
    }
    #[cfg(target_os = "macos")]
    {
        search_mdfind(query, scope_path)
    }
    #[cfg(target_os = "linux")]
    {
        search_locate(query, scope_path)
    }
}

/// Windows: Use Everything's command-line interface (es.exe).
#[cfg(target_os = "windows")]
fn search_everything(query: &str, scope_path: Option<&str>) -> Result<Vec<FileEntry>, String> {
    // Build the search query
    let full_query = if let Some(scope) = scope_path {
        format!("{} {}", scope, query)
    } else {
        query.to_string()
    };

    // Try to find es.exe (Everything command-line)
    let es_paths = [
        r"C:\Program Files\Everything\es.exe",
        r"C:\Program Files (x86)\Everything\es.exe",
    ];

    let es_path = es_paths.iter().find(|p| Path::new(p).exists());

    if let Some(es) = es_path {
        let mut cmd = Command::new(es);
        cmd.args(["-max-results", "200", &full_query]);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = cmd.output()
            .map_err(|e| format!("Failed to run es.exe: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let entries: Vec<FileEntry> = stdout
            .lines()
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let path = Path::new(line.trim());
                let metadata = std::fs::metadata(path).ok()?;
                let name = path.file_name()?.to_string_lossy().to_string();
                let extension = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default();
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64);
                Some(FileEntry {
                    name,
                    path: line.trim().to_string(),
                    is_dir: metadata.is_dir(),
                    size: metadata.len(),
                    modified,
                    extension,
                })
            })
            .collect();

        Ok(entries)
    } else {
        // Fallback: recursive walkdir search
        search_walkdir(query, scope_path)
    }
}

/// macOS: Use Spotlight's mdfind.
#[cfg(target_os = "macos")]
fn search_mdfind(query: &str, scope_path: Option<&str>) -> Result<Vec<FileEntry>, String> {
    let mut cmd = Command::new("mdfind");
    if let Some(scope) = scope_path {
        cmd.args(["-onlyin", scope]);
    }
    cmd.arg(format!("kMDItemDisplayName == '*{}*'c", query));

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run mdfind: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_search_results(&stdout)
}

/// Linux: Use locate or fd.
#[cfg(target_os = "linux")]
fn search_locate(query: &str, scope_path: Option<&str>) -> Result<Vec<FileEntry>, String> {
    // Try fd first, then locate, then walkdir fallback
    let fd_result = Command::new("fd")
        .args(["--max-results", "200", query])
        .args(scope_path.map(|s| vec![s.to_string()]).unwrap_or_default())
        .output();

    if let Ok(output) = fd_result {
        if output.status.success() {
            return parse_search_results(&String::from_utf8_lossy(&output.stdout));
        }
    }

    // Fallback to locate
    let locate_result = Command::new("locate")
        .args(["-l", "200", "-i", query])
        .output();

    if let Ok(output) = locate_result {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let results: Vec<&str> = stdout.lines().collect();
            // Filter by scope if provided
            if let Some(scope) = scope_path {
                let filtered: String = results
                    .into_iter()
                    .filter(|l| l.starts_with(scope))
                    .collect::<Vec<_>>()
                    .join("\n");
                return parse_search_results(&filtered);
            }
            return parse_search_results(&stdout);
        }
    }

    // Final fallback
    search_walkdir(query, scope_path)
}

/// Fallback: recursive directory walk with name matching.
fn search_walkdir(query: &str, scope_path: Option<&str>) -> Result<Vec<FileEntry>, String> {
    let root = scope_path.unwrap_or(".");
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    fn walk(dir: &Path, query: &str, results: &mut Vec<FileEntry>, max: usize) {
        if results.len() >= max {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if results.len() >= max {
                    return;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.to_lowercase().contains(query) {
                    if let Ok(metadata) = std::fs::metadata(entry.path()).or_else(|_| entry.metadata()) {
                        let path_str = entry.path().to_string_lossy().to_string();
                        let extension = entry
                            .path()
                            .extension()
                            .map(|e| e.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let modified = metadata
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64);
                        results.push(FileEntry {
                            name,
                            path: path_str,
                            is_dir: metadata.is_dir(),
                            size: metadata.len(),
                            modified,
                            extension,
                        });
                    }
                }
                if entry.path().is_dir() {
                    walk(&entry.path(), query, results, max);
                }
            }
        }
    }

    walk(Path::new(root), &query_lower, &mut results, 200);
    Ok(results)
}

/// Parse newline-separated file paths into FileEntry list.
#[allow(dead_code)]
fn parse_search_results(output: &str) -> Result<Vec<FileEntry>, String> {
    let entries: Vec<FileEntry> = output
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let path = Path::new(line.trim());
            let metadata = std::fs::metadata(path).ok()?;
            let name = path.file_name()?.to_string_lossy().to_string();
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64);
            Some(FileEntry {
                name,
                path: line.trim().to_string(),
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                modified,
                extension,
            })
        })
        .collect();
    Ok(entries)
}
