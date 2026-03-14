use super::RcloneSignal;

/// Parse a single rclone log line and return a signal if it matches a known pattern.
pub fn parse_log_line(line: &str) -> Option<RcloneSignal> {
    // Successful mount indicators
    if line.contains("Serving") || line.contains("Local file system at") {
        return Some(RcloneSignal::Started);
    }

    // Fatal errors
    if line.contains("Fatal") || line.contains("mount helper error") {
        let msg = line.to_string();
        return Some(RcloneSignal::Fatal(msg));
    }

    // WinFSP-specific errors
    if line.contains("winfsp") && line.contains("not found") {
        return Some(RcloneSignal::Fatal("WinFSP not found".into()));
    }

    // Mount point busy
    if line.contains("mount point") && line.contains("already mounted") {
        return Some(RcloneSignal::Fatal(
            "mount point already mounted".into(),
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serving_signal() {
        let signal = parse_log_line("2024/01/15 10:00:00 Serving remote control on http://127.0.0.1:5572/");
        assert!(matches!(signal, Some(RcloneSignal::Started)));
    }

    #[test]
    fn test_local_filesystem_signal() {
        let signal = parse_log_line("2024/01/15 10:00:00 Local file system at R:");
        assert!(matches!(signal, Some(RcloneSignal::Started)));
    }

    #[test]
    fn test_fatal_signal() {
        let signal = parse_log_line("2024/01/15 10:00:00 Fatal error: failed to mount FUSE fs");
        assert!(matches!(signal, Some(RcloneSignal::Fatal(_))));
    }

    #[test]
    fn test_mount_helper_error() {
        let signal = parse_log_line("mount helper error: mount failed");
        assert!(matches!(signal, Some(RcloneSignal::Fatal(_))));
    }

    #[test]
    fn test_winfsp_not_found() {
        let signal = parse_log_line("ERROR: winfsp not found, please install WinFSP");
        assert!(matches!(signal, Some(RcloneSignal::Fatal(_))));
    }

    #[test]
    fn test_already_mounted() {
        let signal = parse_log_line("mount point R: already mounted");
        assert!(matches!(signal, Some(RcloneSignal::Fatal(_))));
    }

    #[test]
    fn test_noise_lines() {
        assert!(parse_log_line("2024/01/15 10:00:00 INFO  : Starting background cache cleaner").is_none());
        assert!(parse_log_line("2024/01/15 10:00:00 INFO  : vfs cache: used 100M, free 900M").is_none());
        assert!(parse_log_line("").is_none());
        assert!(parse_log_line("just some random text").is_none());
    }

    #[test]
    fn test_mixed_case() {
        // Our parser is case-sensitive, matching rclone's actual output
        assert!(parse_log_line("fatal error").is_none()); // lowercase 'f'
        assert!(parse_log_line("Fatal error").is_some()); // uppercase 'F'
    }
}
