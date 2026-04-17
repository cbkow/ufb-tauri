//! WinFsp backend — Slice 1 of `docs/windows-winfsp-port-plan.md`.
//!
//! Windows analog of `sync/nfs_server.rs`: mounts a per-domain virtual
//! filesystem at `C:\Volumes\ufb\{share}` and serves each I/O op as a
//! callback. Uses SnowflakePowered's `winfsp` crate (native
//! `FSP_FILE_SYSTEM_INTERFACE`, not the FUSE compatibility layer).
//!
//! Slice 1 is passthrough-only: reads go straight to SMB per op, no
//! cache consultation, writes/cleanup are unimplemented. Metadata cache
//! lands in Slice 2; block-level content cache in Slice 3; write path in
//! Slice 4. See `docs/windows-winfsp-port-plan.md`.
//!
//! License: `winfsp` crate + WinFsp DLL are GPL-3.0. UFB is AGPL-3.0 /
//! GPL-3.0-or-later in `LICENSE` — FLOSS-exception compliant.

use crate::messages::AgentToUfb;
use crate::sync::nas_health::NasHealth;
use crate::sync::windows_cache::CacheIndex;
use std::ffi::{c_void, OsStr};
use std::fs;
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};
use tokio::sync::mpsc;
use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo,
    VolumeInfo, WideNameInfo,
};
use winfsp::host::{FileSystemHost, VolumeParams};
use winfsp::{winfsp_init, FspError, U16CStr};

// NTSTATUS values — hardcoded to avoid windows-crate version collision between
// this codebase (windows 0.61) and winfsp's internal (windows 0.58).
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC0000034u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC0000022u32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x80000005u32 as i32;

fn err_not_found() -> FspError { FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND) }
fn err_access_denied() -> FspError { FspError::NTSTATUS(STATUS_ACCESS_DENIED) }
fn err_buffer_overflow() -> FspError { FspError::NTSTATUS(STATUS_BUFFER_OVERFLOW) }

/// Canonical permissive security descriptor applied to every file. Protected
/// DACL with Allow-FullAccess to Everyone ("WD" = World). Computed once at
/// first use via `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
///
/// Slice 1: one SD for everything. Slice 4+ may fetch real NAS ACLs.
static PERMISSIVE_SD: OnceLock<Vec<u8>> = OnceLock::new();

fn permissive_sd() -> &'static [u8] {
    PERMISSIVE_SD
        .get_or_init(|| {
            use windows_sys::Win32::Foundation::LocalFree;
            use windows_sys::Win32::Security::Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            };
            use windows_sys::Win32::Security::GetSecurityDescriptorLength;

            // SDDL: Owner=BuiltinAdmins, Group=BuiltinAdmins, Protected
            // DACL with Allow File-All to System / BuiltinAdmins / World.
            // Matches the canonical permissive SD used in ntptfs-winfsp-rs.
            let sddl: Vec<u16> = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)\0"
                .encode_utf16()
                .collect();
            let mut sd_ptr: *mut c_void = std::ptr::null_mut();
            unsafe {
                let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1 as u32,
                    &mut sd_ptr,
                    std::ptr::null_mut(),
                );
                if ok == 0 || sd_ptr.is_null() {
                    panic!("ConvertStringSecurityDescriptorToSecurityDescriptorW failed");
                }
                let len = GetSecurityDescriptorLength(sd_ptr) as usize;
                let slice = std::slice::from_raw_parts(sd_ptr as *const u8, len);
                let v = slice.to_vec();
                LocalFree(sd_ptr as *mut _);
                v
            }
        })
        .as_slice()
}

/// Per-open context. Files hold a handle; dirs hold a cached DirBuffer.
pub enum OpenCtx {
    File {
        abs: PathBuf,
        handle: fs::File,
    },
    Dir {
        abs: PathBuf,
        dir_buffer: DirBuffer,
    },
}

impl OpenCtx {
    fn abs(&self) -> &PathBuf {
        match self {
            OpenCtx::File { abs, .. } => abs,
            OpenCtx::Dir { abs, .. } => abs,
        }
    }
}

/// User-facing mount point for a domain. Mirrors the non-sync drive-letter
/// path, so toggling sync on a mount preserves bookmarks.
pub fn mount_point_for(share: &str) -> PathBuf {
    crate::config::MountConfig::volumes_base().join(share)
}

/// Ensure `mount_path` is available for a fresh WinFsp mount. Removes
/// stale symlinks (common when the mount transitions from non-sync to
/// sync) and stale reparse points (from a prior WinFsp run that didn't
/// unmount cleanly). Creates the parent directory. Idempotent.
fn prepare_mount_point(mount_path: &std::path::Path) {
    if let Some(parent) = mount_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::symlink_metadata(mount_path) {
        Ok(md) => {
            let attrs = md.file_type();
            if attrs.is_symlink() {
                let _ = fs::remove_file(mount_path);
            } else if md.file_type().is_dir() {
                // Could be a stale WinFsp reparse point from a prior run
                // that didn't clean up (process killed mid-flight).
                // `remove_dir` works on reparse points and empty dirs;
                // it bails on non-empty real dirs (which we want).
                let _ = fs::remove_dir(mount_path);
            }
        }
        Err(_) => { /* fresh — nothing to do */ }
    }
}

/// Slice 2: enumeration + metadata are served from `CacheIndex` when
/// warm; cold paths fall back to live SMB and populate the cache. File
/// content still passes through (Slice 3 adds block-level hydration).
/// Write callbacks + ipc_tx ConflictDetected land in Slice 4.
pub struct PassthroughFs {
    #[allow(dead_code)]
    domain: String,
    nas_root: PathBuf,
    cache: Arc<CacheIndex>,
    /// Agent → UFB channel for out-of-band events (e.g. ConflictDetected).
    #[allow(dead_code)]
    ipc_tx: mpsc::Sender<AgentToUfb>,
    /// Rolling NAS reachability. Handlers will consult `is_online()` in
    /// Slices 3+ to short-circuit SMB ops when the share is unreachable.
    #[allow(dead_code)]
    health: Arc<NasHealth>,
}

impl PassthroughFs {
    /// Convert the ProjFS-style file name (e.g. `"\\261283_Breyers\\3d"`)
    /// to an absolute path on the NAS backing store.
    fn resolve(&self, name: &U16CStr) -> PathBuf {
        let s = name.to_string_lossy();
        let trimmed = s.trim_start_matches('\\').trim_start_matches('/');
        if trimmed.is_empty() {
            self.nas_root.clone()
        } else {
            self.nas_root.join(trimmed.replace('/', "\\"))
        }
    }

    /// Convert the file name to a cache-normalized relative path
    /// (forward-slash separators, no leading slash — root is the empty
    /// string). Must be kept in sync with `CacheIndex::cached_attr_by_path`
    /// and `cached_children_by_parent` semantics.
    fn rel_path(name: &U16CStr) -> String {
        let s = name.to_string_lossy();
        s.trim_start_matches('\\')
            .trim_start_matches('/')
            .replace('\\', "/")
    }

    /// Cache-normalized relative path for an absolute NAS path. Used in
    /// handlers that already have the `OpenCtx::abs` rather than a
    /// U16CStr. Returns the empty string if `abs` is the NAS root itself.
    fn rel_from_abs(&self, abs: &Path) -> String {
        abs.strip_prefix(&self.nas_root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }
}

const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILETIME_EPOCH_OFFSET: u64 = 116_444_736_000_000_000;

fn st_to_filetime(t: std::time::SystemTime) -> u64 {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    secs.saturating_mul(10_000_000)
        .saturating_add(FILETIME_EPOCH_OFFSET)
}

fn populate_file_info(meta: &fs::Metadata, info: &mut FileInfo) {
    info.file_attributes = if meta.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    info.file_size = meta.len();
    info.allocation_size = meta.len();
    let created = meta.created().unwrap_or(UNIX_EPOCH);
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    let accessed = meta.accessed().unwrap_or(UNIX_EPOCH);
    info.creation_time = st_to_filetime(created);
    info.last_write_time = st_to_filetime(modified);
    info.change_time = st_to_filetime(modified);
    info.last_access_time = st_to_filetime(accessed);
}

fn f64_secs_to_filetime(secs: f64) -> u64 {
    (secs.max(0.0) as u64)
        .saturating_mul(10_000_000)
        .saturating_add(FILETIME_EPOCH_OFFSET)
}

/// Populate a `FileInfo` from a cached `CachedAttr`. Mirror of
/// `populate_file_info` but for the warm-cache path — avoids the SMB
/// round-trip for attributes we already have.
fn populate_file_info_from_cached(
    attr: &crate::sync::cache_core::CachedAttr,
    info: &mut FileInfo,
) {
    info.file_attributes = if attr.is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    info.file_size = attr.size;
    info.allocation_size = attr.size;
    let ft_created = f64_secs_to_filetime(attr.created);
    let ft_modified = f64_secs_to_filetime(attr.mtime);
    info.creation_time = ft_created;
    info.last_write_time = ft_modified;
    info.change_time = ft_modified;
    info.last_access_time = ft_modified;
}

impl FileSystemContext for PassthroughFs {
    type FileContext = Box<OpenCtx>;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let rel = Self::rel_path(file_name);
        // Warm path: cache hit → skip the SMB stat entirely.
        let attrs = if let Some(attr) = self.cache.cached_attr_by_path(&rel) {
            if attr.is_dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            }
        } else {
            // Cold path: stat SMB. Caching the result is deferred to the
            // `open` / `read_directory` flow — this callback fires for
            // non-existence checks too and we don't want to write rows
            // for paths that don't exist.
            let abs = self.resolve(file_name);
            let meta = match fs::metadata(&abs) {
                Ok(m) => m,
                Err(_) => return Err(err_not_found()),
            };
            if meta.is_dir() {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            }
        };
        let sd = permissive_sd();
        // If the caller provided a buffer, fill it (size permitting). The
        // trampoline writes `sz_security_descriptor` back either way, so
        // Windows can allocate a bigger buffer on STATUS_BUFFER_OVERFLOW.
        if let Some(buf) = security_descriptor {
            if buf.len() >= sd.len() {
                // SAFETY: c_void slice of ≥ sd.len() bytes.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        sd.as_ptr(),
                        buf.as_mut_ptr() as *mut u8,
                        sd.len(),
                    );
                }
            } else {
                return Err(err_buffer_overflow());
            }
        }
        Ok(FileSecurity {
            attributes: attrs,
            sz_security_descriptor: sd.len() as u64,
            reparse: false,
        })
    }

    fn get_security(
        &self,
        _ctx: &Self::FileContext,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        let sd = permissive_sd();
        if let Some(buf) = security_descriptor {
            if buf.len() >= sd.len() {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        sd.as_ptr(),
                        buf.as_mut_ptr() as *mut u8,
                        sd.len(),
                    );
                }
            } else {
                return Err(err_buffer_overflow());
            }
        }
        Ok(sd.len() as u64)
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let abs = self.resolve(file_name);
        let meta = fs::metadata(&abs).map_err(|_| err_not_found())?;
        populate_file_info(&meta, file_info.as_mut());
        let ctx = if meta.is_dir() {
            OpenCtx::Dir {
                abs,
                dir_buffer: DirBuffer::new(),
            }
        } else {
            let h = fs::File::open(&abs).map_err(|_| err_access_denied())?;
            OpenCtx::File { abs, handle: h }
        };
        Ok(Box::new(ctx))
    }

    fn close(&self, _ctx: Self::FileContext) {
        // Box drops automatically.
    }

    fn get_file_info(
        &self,
        ctx: &Self::FileContext,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        // Warm path: serve from cache if we have the entry.
        let rel = self.rel_from_abs(ctx.abs());
        if let Some(attr) = self.cache.cached_attr_by_path(&rel) {
            populate_file_info_from_cached(&attr, file_info);
            return Ok(());
        }
        // Cold: stat SMB. We don't write to cache here — row creation
        // happens via `read_directory` / `populate_folder`.
        let meta = fs::metadata(ctx.abs()).map_err(|_| err_not_found())?;
        populate_file_info(&meta, file_info);
        Ok(())
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        log::info!("[winfsp] get_volume_info");
        out_volume_info.total_size = 1u64 << 40;
        out_volume_info.free_size = 1u64 << 39;
        out_volume_info.set_volume_label("ufb-winfsp-spike");
        Ok(())
    }

    fn read(
        &self,
        ctx: &Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> winfsp::Result<u32> {
        let handle = match &**ctx {
            OpenCtx::File { handle, .. } => handle,
            OpenCtx::Dir { .. } => return Err(err_access_denied()),
        };
        log::info!(
            "[winfsp] read abs={} offset={} length={}",
            ctx.abs().display(),
            offset,
            buffer.len()
        );
        match handle.seek_read(buffer, offset) {
            Ok(n) => Ok(n as u32),
            Err(_) => Err(err_access_denied()),
        }
    }

    fn read_directory(
        &self,
        ctx: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        let (abs, dir_buffer) = match &**ctx {
            OpenCtx::Dir { abs, dir_buffer } => (abs, dir_buffer),
            OpenCtx::File { .. } => return Err(err_access_denied()),
        };

        if marker.is_none() {
            let rel = self.rel_from_abs(abs);

            // Warm path: folder already enumerated — serve from SQLite.
            // Cold path: live `fs::read_dir` + populate the cache, then
            // serve from cache. Either way, everything we write into the
            // DirBuffer comes from the cache so the two paths produce
            // identical output.
            if !self.cache.folder_is_enumerated_by_rel(&rel) {
                self.cold_populate(abs, &rel);
            }

            let lock = dir_buffer.acquire(true, None)?;

            // "." and ".." synthesized from cache where possible.
            if let Some(self_attr) = self.cache.cached_attr_by_path(&rel) {
                write_dir_entry(&lock, ".", &self_attr);
                if !rel.is_empty() {
                    let parent_rel = match rel.rfind('/') {
                        Some(i) => rel[..i].to_string(),
                        None => String::new(),
                    };
                    if let Some(parent_attr) = self.cache.cached_attr_by_path(&parent_rel) {
                        write_dir_entry(&lock, "..", &parent_attr);
                    } else {
                        // Root has no row in the cache (it's not in known_files)
                        // — synthesize a minimal dir attr for "..".
                        let fake = crate::sync::cache_core::CachedAttr {
                            is_dir: true,
                            size: 0,
                            mtime: 0.0,
                            created: 0.0,
                            is_hydrated: false,
                            hydrated_size: 0,
                        };
                        write_dir_entry(&lock, "..", &fake);
                    }
                }
            }

            // Real children.
            let children = self.cache.cached_children_by_parent(&rel);
            for (name, attr) in &children {
                write_dir_entry(&lock, name, attr);
            }

            drop(lock);
        }
        Ok(dir_buffer.read(marker, buffer))
    }
}

/// Helper: build a DirInfo from a CachedAttr + name and write it to the
/// acquired DirBufferLock. Skips on name-too-long / buffer-full errors
/// (the latter stops the iterator; WinFsp's client retries with a marker).
fn write_dir_entry(
    lock: &winfsp::filesystem::DirBufferLock<'_>,
    name: &str,
    attr: &crate::sync::cache_core::CachedAttr,
) -> bool {
    let mut dir_info: DirInfo<255> = DirInfo::new();
    populate_file_info_from_cached(attr, dir_info.file_info_mut());
    let name_os: &OsStr = name.as_ref();
    if dir_info.set_name(name_os).is_err() {
        return true; // skip this entry, continue iteration
    }
    lock.write(&mut dir_info).is_ok()
}

impl PassthroughFs {
    /// Live `fs::read_dir` → populate the cache. Runs once per folder
    /// (the first time it's enumerated). No-ops on SMB error — the
    /// subsequent cache read will just return the empty set.
    fn cold_populate(&self, abs: &Path, rel: &str) {
        let rd = match fs::read_dir(abs) {
            Ok(r) => r,
            Err(e) => {
                log::warn!(
                    "[winfsp] cold read_dir {} failed: {}",
                    abs.display(),
                    e
                );
                return;
            }
        };
        let mut entries: Vec<(String, bool, u64, i64, i64)> = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let size = meta.len();
            let mtime_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let created_secs = meta
                .created()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            entries.push((name, meta.is_dir(), size, mtime_secs, created_secs));
        }
        self.cache.populate_folder(rel, &entries);
    }
}

/// Start a WinFsp backend for `domain` serving `nas_root`. Non-blocking —
/// spawns (a) a tokio task for the NAS health probe, (b) a tokio task for
/// eviction if the cache has a budget, and (c) an OS thread running the
/// WinFsp dispatcher (can't be tokio because WinFsp callbacks are sync).
///
/// Mount point is derived via `mount_point_for(domain)`. Must be called
/// from within a tokio runtime.
pub fn start(
    domain: String,
    nas_root: PathBuf,
    cache: Arc<CacheIndex>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
) {
    // (a) NAS health probe — tokio task.
    let health = NasHealth::new(domain.clone(), nas_root.clone());
    Arc::clone(&health).spawn_probe_loop();

    // (b) Eviction tick — tokio task, skipped if no budget is set.
    //     Evictor is a sync function; wrap in spawn_blocking so the tokio
    //     worker doesn't stall.
    if cache.cache_limit() > 0 {
        let cache_for_evict = Arc::clone(&cache);
        let domain_for_evict = domain.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.tick().await; // skip immediate tick
            loop {
                tick.tick().await;
                let cache = Arc::clone(&cache_for_evict);
                let (files, bytes) = tokio::task::spawn_blocking(move || {
                    cache.evict_over_budget_now()
                })
                .await
                .unwrap_or((0, 0));
                if files > 0 {
                    log::debug!(
                        "[winfsp] {} evictor freed {} files / {} bytes",
                        domain_for_evict, files, bytes,
                    );
                }
            }
        });
    }

    // (c) WinFsp dispatcher — OS thread. FileSystemHost keeps the mount
    //     alive until drop; parking the thread keeps the host alive for
    //     the life of the process.
    let mount_path = mount_point_for(&domain);
    std::thread::spawn(move || {
        log::info!(
            "[winfsp] {} starting: mount_path={}, nas_root={}",
            domain,
            mount_path.display(),
            nas_root.display()
        );

        if winfsp_init().is_err() {
            log::error!(
                "[winfsp] {} winfsp_init failed — is WinFsp runtime installed?",
                domain
            );
            return;
        }

        prepare_mount_point(&mount_path);

        let mut volume_params = VolumeParams::new();
        volume_params
            .case_preserved_names(true)
            .case_sensitive_search(false)
            .unicode_on_disk(true)
            .persistent_acls(false)
            .read_only_volume(true) // Slice 1: read-only. Slice 4 flips this.
            .post_cleanup_when_modified_only(true)
            .file_info_timeout(1000)
            .volume_info_timeout(1000);

        let provider = PassthroughFs {
            domain: domain.clone(),
            nas_root: nas_root.clone(),
            cache,
            ipc_tx,
            health,
        };

        let mut host = match FileSystemHost::new(volume_params, provider) {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[winfsp] {} FileSystemHost::new failed: {:?}",
                    domain, e
                );
                return;
            }
        };

        let mount_os: &OsStr = mount_path.as_os_str();
        if let Err(e) = host.mount(mount_os) {
            log::error!(
                "[winfsp] {} mount({}) failed: {:?}",
                domain,
                mount_path.display(),
                e
            );
            return;
        }

        if let Err(e) = host.start() {
            log::error!("[winfsp] {} dispatcher start() failed: {:?}", domain, e);
            return;
        }

        log::info!("[winfsp] {} running at {}", domain, mount_path.display());

        // Keep thread alive so `host` (and its owned FSP_FILE_SYSTEM) lives
        // for the life of the process. Agent shutdown drops the process,
        // which unmounts automatically.
        loop {
            std::thread::park();
        }
    });
}
