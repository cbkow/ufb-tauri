use std::path::Path;
use std::time::Duration;

/// Two-level health probe:
/// 1. Check if mount path exists (metadata check)
/// 2. Read the .healthcheck file (read verification)
///
/// Returns true if both checks pass within the timeout.
pub async fn run_probe(mount_path: &Path, healthcheck_file: &str, timeout: Duration) -> bool {
    let result = tokio::time::timeout(timeout, async {
        // Level 1: existence check
        match tokio::fs::metadata(mount_path).await {
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "Probe L1 failed for {}: {}",
                    mount_path.display(),
                    e
                );
                return false;
            }
        }

        // Level 2: read healthcheck file
        let healthcheck_path = mount_path.join(healthcheck_file);
        match tokio::fs::read_to_string(&healthcheck_path).await {
            Ok(_) => true,
            Err(e) => {
                log::debug!(
                    "Probe L2 failed for {}: {}",
                    healthcheck_path.display(),
                    e
                );
                false
            }
        }
    })
    .await;

    match result {
        Ok(ok) => ok,
        Err(_) => {
            log::debug!("Probe timed out for {}", mount_path.display());
            false
        }
    }
}
