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
use super::cache::{stat_and_refresh, CacheIndex, StatResult};
use super::connectivity::{is_network_error, NasConnectivity};
use super::placeholder::refresh_placeholder;
use super::write_through::EchoSuppressor;

const CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB hydration chunks

/// Stat-on-open TTL. An entry verified against NAS within this window is
/// trusted without re-statting — prevents stat-storms on bursty opens
/// (timeline load, project scan).
const STAT_VERIFY_TTL_SECS: i64 = 30;

/// CF API sync filter. Handles all OS callbacks for a single sync root.
pub struct NasSyncFilter {
    pub nas_root: PathBuf,
    pub client_root: PathBuf,
    pub watcher: Arc<NasWatcher>,
    pub echo: Arc<EchoSuppressor>,
    pub connectivity: Arc<NasConnectivity>,
    pub cache: Arc<CacheIndex>,
    pub open_handles: Arc<Mutex<HashMap<PathBuf, u32>>>,
}

impl NasSyncFilter {
    pub fn new(
        nas_root: PathBuf,
        client_root: PathBuf,
        watcher: Arc<NasWatcher>,
        echo: Arc<EchoSuppressor>,
        connectivity: Arc<NasConnectivity>,
        cache: Arc<CacheIndex>,
        open_handles: Arc<Mutex<HashMap<PathBuf, u32>>>,
    ) -> Self {
        Self {
            nas_root,
            client_root,
            watcher,
            echo,
            connectivity,
            cache,
            open_handles,
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

        // Get folder mtime for reconciliation tracking
        let folder_mtime = fs::metadata(&nas_dir)
            .and_then(|m| m.modified())
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
            .unwrap_or(0);
        self.cache
            .record_visited_folder(&nas_dir, &request_path, folder_mtime);

        let mut placeholders = Vec::new();
        let mut skip_count = 0;
        let mut refreshed_count = 0;
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

            // File-only: drift check + cache bookkeeping.
            if !meta.is_dir() {
                let entry_mtime = meta
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64
                    })
                    .unwrap_or(0);
                let client_path = request_path.join(&name_str);

                // If the placeholder already exists locally, check for drift
                // against our cached metadata. CF leaves existing placeholders
                // alone when we push new ones via `pass_with_placeholder`, so
                // drift here would otherwise be latent until the watcher fires
                // or the user opens the file.
                if client_path.exists() {
                    match self.cache.compare_nas_metadata(&client_path, meta.len(), entry_mtime) {
                        Some(true) => {
                            // Drift — rebuild placeholder from NAS state.
                            let display = client_path.to_string_lossy().to_string();
                            refresh_placeholder(
                                &entry.path(),
                                &request_path,
                                &name_str,
                                &display,
                                &*self.cache,
                            );
                            refreshed_count += 1;
                            // refresh_placeholder recreated the placeholder + DB row;
                            // don't push a duplicate in the ticket list.
                            continue;
                        }
                        Some(false) => {
                            // Match — stamp verified and skip push (already present).
                            self.cache.record_verification(&client_path);
                            continue;
                        }
                        None => {
                            // Known path but no row (shouldn't happen) — fall through.
                        }
                    }
                }

                // New placeholder path: record metadata, push to ticket.
                self.cache
                    .record_known_file(&client_path, meta.len(), entry_mtime);
            }

            placeholders.push(
                PlaceholderFile::new(&name_str)
                    .metadata(cf_meta)
                    .mark_in_sync()
                    .blob(blob),
            );
        }

        let elapsed = start.elapsed();
        log::info!(
            "[sync] Enumerated {:?}: {} placeholders, {} refreshed, {} skipped, {:.1}ms",
            relative,
            placeholders.len(),
            refreshed_count,
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

        // Retry NAS file open with backoff — handles transient disconnects.
        // Leave 10s margin before the CF API's 60-second callback timeout.
        let mut file = loop {
            match fs::File::open(&nas_path) {
                Ok(f) => break f,
                Err(e) if is_network_error(&e) && start.elapsed().as_secs() < 50 => {
                    self.connectivity.report_network_error();
                    log::warn!(
                        "[sync] Hydration waiting for NAS ({:.0}s): {:?}",
                        start.elapsed().as_secs_f64(),
                        nas_path
                    );
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    continue;
                }
                Err(e) => {
                    if is_network_error(&e) {
                        self.connectivity.report_network_error();
                    }
                    log::error!("[sync] Failed to open {:?}: {}", nas_path, e);
                    return Ok(());
                }
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
                    if is_network_error(&e) {
                        self.connectivity.report_network_error();
                    }
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

        // Track in cache index for LRU eviction
        self.cache.record_hydration(&request.path(), total_written);

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

        // Suppress so NAS watcher doesn't try to remove the placeholder too
        self.echo.suppress(nas_path.clone());

        if nas_path.is_dir() {
            log::info!("[sync] DELETE dir {:?}", relative);
            let _ = fs::remove_dir_all(&nas_path);
        } else if nas_path.exists() {
            log::info!("[sync] DELETE {:?}", relative);
            let _ = fs::remove_file(&nas_path);
        } else {
            log::debug!("[sync] DELETE {:?} (already gone)", relative);
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

        // Update cache index path
        self.cache.rename_entry(&request.path(), &info.target_path());

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
        // Remove from cache index
        self.cache.record_dehydration(&request.path());
        Ok(())
    }

    /// Track file opens for deferred NAS update handling.
    /// Track file opens for deferred NAS update handling.
    ///
    /// Also performs opportunistic stat-on-open: when we're the first opener
    /// and the entry's cached metadata hasn't been verified recently, stat
    /// NAS and refresh the placeholder if it has diverged.
    fn opened(&self, request: Request, _info: info::Opened) {
        let path = request.path();

        // Increment handle count; capture pre-increment value to know if we
        // raced in ahead of an existing opener.
        let count_before = {
            let mut handles = self.open_handles.lock().unwrap();
            let entry = handles.entry(path.clone()).or_insert(0);
            let before = *entry;
            *entry += 1;
            before
        };

        // LRU refresh.
        self.cache.touch(&path);

        // Skip freshness check if someone else already has the file open —
        // refreshing the placeholder under an exclusive handle would fail and
        // could race with in-flight I/O.
        if count_before > 0 {
            return;
        }

        // Resolve NAS path using the same blob-or-derive pattern as fetch_data.
        let blob = request.file_blob();
        let nas_path = if !blob.is_empty() {
            PathBuf::from(String::from_utf8_lossy(blob).to_string())
        } else {
            let relative = self.relative_path(&path);
            self.nas_path(&relative)
        };

        match stat_and_refresh(&self.cache, &path, &nas_path, STAT_VERIFY_TTL_SECS) {
            StatResult::Drifted { size, mtime } => {
                log::info!(
                    "[sync] Stat-on-open drift {:?}: nas=({}, {}) — refreshing placeholder",
                    path, size, mtime
                );
                if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                    let file_name = name.to_string_lossy().to_string();
                    let display = path.to_string_lossy().to_string();
                    refresh_placeholder(&nas_path, parent, &file_name, &display, &*self.cache);
                }
            }
            StatResult::Error(e) => {
                if is_network_error(&e) {
                    self.connectivity.report_network_error();
                }
                log::debug!("[sync] Stat-on-open skipped {:?}: {}", path, e);
            }
            // Skipped (within TTL), Fresh (match), Unknown (no baseline) — nothing to do.
            _ => {}
        }
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
