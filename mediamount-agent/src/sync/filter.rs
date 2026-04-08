/// SyncFilter implementation — translates CF API callbacks to SMB operations.

use cloud_filter::{
    error::CResult,
    filter::{info, ticket, Request, SyncFilter},
    metadata::Metadata,
    placeholder_file::PlaceholderFile,
    utility::WriteAt,
};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::watcher::NasWatcher;

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB hydration chunks

/// CF API sync filter. Handles all OS callbacks for a single sync root.
pub struct NasSyncFilter {
    pub nas_root: PathBuf,
    pub client_root: PathBuf,
    pub watcher: NasWatcher,
    /// Tracks open file handles for deferred NAS updates.
    pub open_handles: Arc<Mutex<HashMap<PathBuf, u32>>>,
}

impl NasSyncFilter {
    pub fn new(nas_root: PathBuf, client_root: PathBuf) -> Self {
        let watcher = NasWatcher::new(nas_root.clone());
        Self {
            nas_root,
            client_root,
            watcher,
            open_handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Convert an absolute request path to a path relative to the client root.
    fn relative_path(&self, request_path: &Path) -> PathBuf {
        request_path
            .strip_prefix(&self.client_root)
            .unwrap_or(Path::new(""))
            .to_path_buf()
    }

    /// Map a client-relative path to the corresponding NAS path.
    fn nas_path(&self, relative: &Path) -> PathBuf {
        self.nas_root.join(relative)
    }

    /// Check if a filename should be filtered (Synology internal paths).
    fn is_filtered(name: &str) -> bool {
        name.starts_with('#') || name.starts_with('@')
    }
}

impl SyncFilter for NasSyncFilter {
    /// Folder navigated to — enumerate NAS directory via SMB.
    fn fetch_placeholders(
        &self,
        request: Request,
        ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> CResult<()> {
        let request_path = request.path();
        let relative = self.relative_path(&request_path);
        let nas_dir = self.nas_path(&relative);

        let start = Instant::now();
        log::debug!("[sync] FETCH_PLACEHOLDERS {:?} -> {:?}", relative, nas_dir);

        if !nas_dir.is_dir() {
            log::warn!("[sync] Not a directory on NAS: {:?}", nas_dir);
            let _ = ticket.pass_with_placeholder(&mut []);
            return Ok(());
        }

        // Register this folder for live watching
        self.watcher.register(nas_dir.clone(), request_path.clone());

        let mut placeholders = Vec::new();
        let mut skip_count = 0;
        for entry in fs::read_dir(&nas_dir).into_iter().flatten() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let name = entry.file_name();
            let name_str = name.to_string_lossy().to_string();

            if Self::is_filtered(&name_str) {
                skip_count += 1;
                continue;
            }

            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let blob = entry.path().to_string_lossy().as_bytes().to_vec();
            let cf_meta = if meta.is_dir() {
                Metadata::directory()
            } else {
                Metadata::file().size(meta.len())
            };

            placeholders.push(
                PlaceholderFile::new(&name_str)
                    .metadata(cf_meta)
                    .mark_in_sync()
                    .blob(blob),
            );
        }

        let elapsed = start.elapsed();
        log::info!(
            "[sync] Enumerated {:?}: {} placeholders, {} skipped, {:.1}ms",
            relative,
            placeholders.len(),
            skip_count,
            elapsed.as_secs_f64() * 1000.0
        );

        let _ = ticket.pass_with_placeholder(&mut placeholders);
        Ok(())
    }

    /// File opened — stream from NAS via SMB in chunks.
    fn fetch_data(
        &self,
        request: Request,
        ticket: ticket::FetchData,
        info: info::FetchData,
    ) -> CResult<()> {
        let blob = request.file_blob();
        let nas_path = if !blob.is_empty() {
            PathBuf::from(String::from_utf8_lossy(blob).to_string())
        } else {
            let relative = self.relative_path(&request.path());
            self.nas_path(&relative)
        };

        let start = Instant::now();
        let range = info.required_file_range();
        let offset = range.start;
        let total_needed = range.end - range.start;

        log::info!(
            "[sync] FETCH_DATA {:?} ({:.1} MB)",
            nas_path,
            total_needed as f64 / (1024.0 * 1024.0)
        );

        let mut file = match fs::File::open(&nas_path) {
            Ok(f) => f,
            Err(e) => {
                log::error!("[sync] Failed to open {:?}: {}", nas_path, e);
                return Ok(());
            }
        };

        if offset > 0 {
            if let Err(e) = file.seek(SeekFrom::Start(offset)) {
                log::error!("[sync] Failed to seek {:?}: {}", nas_path, e);
                return Ok(());
            }
        }

        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut position = offset;
        let mut total_written: u64 = 0;

        while position < range.end {
            let remaining = (range.end - position) as usize;
            let to_read = remaining.min(CHUNK_SIZE);

            let bytes_read = match file.read(&mut buf[..to_read]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    log::error!("[sync] Read error at offset {}: {}", position, e);
                    return Ok(());
                }
            };

            if let Err(e) = ticket.write_at(&buf[..bytes_read], position) {
                log::error!("[sync] CF write error at offset {}: {}", position, e);
                return Ok(());
            }

            position += bytes_read as u64;
            total_written += bytes_read as u64;

            if total_needed > 1024 * 1024 {
                let _ = ticket.report_progress(total_needed, total_written);
            }
        }

        let elapsed = start.elapsed();
        let speed_mbps = if elapsed.as_secs_f64() > 0.0 {
            (total_written as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64()
        } else {
            0.0
        };

        log::info!(
            "[sync] Hydrated {:?}: {:.1} MB, {:.1}ms ({:.0} MB/s)",
            nas_path,
            total_written as f64 / (1024.0 * 1024.0),
            elapsed.as_secs_f64() * 1000.0,
            speed_mbps
        );

        Ok(())
    }

    /// File/directory deleted locally — propagate to NAS.
    fn delete(
        &self,
        request: Request,
        ticket: ticket::Delete,
        _info: info::Delete,
    ) -> CResult<()> {
        let relative = self.relative_path(&request.path());
        let nas_path = self.nas_path(&relative);
        log::info!("[sync] DELETE {:?}", relative);

        if nas_path.is_dir() {
            let _ = fs::remove_dir_all(&nas_path);
        } else if nas_path.exists() {
            let _ = fs::remove_file(&nas_path);
        }

        let _ = ticket.pass();
        Ok(())
    }

    /// File/directory renamed locally — propagate to NAS.
    fn rename(
        &self,
        request: Request,
        ticket: ticket::Rename,
        info: info::Rename,
    ) -> CResult<()> {
        let relative = self.relative_path(&request.path());
        let nas_src = self.nas_path(&relative);
        let target_relative = self.relative_path(&info.target_path());
        let nas_dst = self.nas_path(&target_relative);

        log::info!("[sync] RENAME {:?} -> {:?}", relative, target_relative);

        if nas_src.exists() {
            let _ = fs::rename(&nas_src, &nas_dst);
        }

        let _ = ticket.pass();
        Ok(())
    }

    /// Dehydration requested — always approve (OS reclaims cache space).
    fn dehydrate(
        &self,
        request: Request,
        ticket: ticket::Dehydrate,
        _info: info::Dehydrate,
    ) -> CResult<()> {
        let relative = self.relative_path(&request.path());
        log::debug!("[sync] DEHYDRATE {:?}", relative);
        let _ = ticket.pass();
        Ok(())
    }

    /// Track file opens for deferred NAS update handling.
    fn opened(&self, request: Request, _info: info::Opened) {
        let path = request.path();
        let mut handles = self.open_handles.lock().unwrap();
        *handles.entry(path).or_insert(0) += 1;
    }

    /// Track file closes. When refcount hits 0, apply any deferred NAS updates.
    fn closed(&self, request: Request, _info: info::Closed) {
        let path = request.path();
        let mut handles = self.open_handles.lock().unwrap();
        if let Some(count) = handles.get_mut(&path) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                handles.remove(&path);
                // TODO: check deferred update queue and apply pending NAS changes
            }
        }
    }
}
