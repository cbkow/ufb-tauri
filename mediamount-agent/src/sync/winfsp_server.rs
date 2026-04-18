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

use crate::messages::{AgentToUfb, ConflictDetectedMsg};
use crate::sync::conflict;
use crate::sync::nas_health::NasHealth;
use crate::sync::windows_cache::CacheIndex;
use std::ffi::{c_void, OsStr};
use std::fs;
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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
const STATUS_OBJECT_NAME_COLLISION: i32 = 0xC0000035u32 as i32;
const STATUS_DISK_FULL: i32 = 0xC000007Fu32 as i32;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;

fn err_not_found() -> FspError { FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND) }
fn err_access_denied() -> FspError { FspError::NTSTATUS(STATUS_ACCESS_DENIED) }
fn err_buffer_overflow() -> FspError { FspError::NTSTATUS(STATUS_BUFFER_OVERFLOW) }
fn err_name_collision() -> FspError { FspError::NTSTATUS(STATUS_OBJECT_NAME_COLLISION) }
fn err_io() -> FspError { FspError::NTSTATUS(STATUS_DISK_FULL) }

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

/// Per-open context. Files hold a read handle; dirs hold a cached
/// DirBuffer. `pending_delete` is flipped by `set_delete` and honored by
/// `cleanup` — matches the Windows "mark for deletion, apply on close"
/// semantics.
pub enum OpenCtx {
    File {
        abs: PathBuf,
        handle: fs::File,
        pending_delete: AtomicBool,
    },
    Dir {
        abs: PathBuf,
        dir_buffer: DirBuffer,
        pending_delete: AtomicBool,
    },
}

impl OpenCtx {
    fn abs(&self) -> &PathBuf {
        match self {
            OpenCtx::File { abs, .. } => abs,
            OpenCtx::Dir { abs, .. } => abs,
        }
    }
    fn pending_delete(&self) -> &AtomicBool {
        match self {
            OpenCtx::File { pending_delete, .. } => pending_delete,
            OpenCtx::Dir { pending_delete, .. } => pending_delete,
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
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
const FILETIME_EPOCH_OFFSET: u64 = 116_444_736_000_000_000;

/// Stable 64-bit ID for a relative path. `FSP_FSCTL_FILE_INFO.IndexNumber`
/// must be unique and non-zero for .NET's recursive enumerator to avoid
/// treating every entry as the same file and short-circuiting. We hash the
/// cache-normalized relative path (which is already our stable key across
/// restarts) with FNV-1a and force the MSB clear to keep the value below
/// Windows's reserved ranges.
fn path_to_index_number(rel: &str) -> u64 {
    // FNV-1a 64-bit — cheap, no dependency, deterministic across runs.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in rel.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Non-zero guarantee (hashing the empty string "" yields the FNV offset
    // basis itself, which is non-zero, but be explicit).
    if h == 0 {
        1
    } else {
        h & 0x7FFF_FFFF_FFFF_FFFF
    }
}

fn st_to_filetime(t: std::time::SystemTime) -> u64 {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    secs.saturating_mul(10_000_000)
        .saturating_add(FILETIME_EPOCH_OFFSET)
}

fn populate_file_info(meta: &fs::Metadata, info: &mut FileInfo, rel: &str) {
    info.file_attributes = if meta.is_dir() {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_ARCHIVE
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
    info.index_number = path_to_index_number(rel);
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
    rel: &str,
) {
    info.file_attributes = if attr.is_dir {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_ARCHIVE
    };
    info.file_size = attr.size;
    info.allocation_size = attr.size;
    let ft_created = f64_secs_to_filetime(attr.created);
    let ft_modified = f64_secs_to_filetime(attr.mtime);
    info.creation_time = ft_created;
    info.last_write_time = ft_modified;
    info.change_time = ft_modified;
    info.last_access_time = ft_modified;
    info.index_number = path_to_index_number(rel);
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
                FILE_ATTRIBUTE_ARCHIVE
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
                FILE_ATTRIBUTE_ARCHIVE
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
        let rel = Self::rel_path(file_name);
        let meta = fs::metadata(&abs).map_err(|_| err_not_found())?;
        populate_file_info(&meta, file_info.as_mut(), &rel);
        let ctx = if meta.is_dir() {
            OpenCtx::Dir {
                abs,
                dir_buffer: DirBuffer::new(),
                pending_delete: AtomicBool::new(false),
            }
        } else {
            let h = fs::File::open(&abs).map_err(|_| err_access_denied())?;
            OpenCtx::File {
                abs,
                handle: h,
                pending_delete: AtomicBool::new(false),
            }
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
        let rel = self.rel_from_abs(ctx.abs());
        if let Some(attr) = self.cache.cached_attr_by_path(&rel) {
            populate_file_info_from_cached(&attr, file_info, &rel);
            return Ok(());
        }
        let meta = fs::metadata(ctx.abs()).map_err(|_| err_not_found())?;
        populate_file_info(&meta, file_info, &rel);
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

        let abs = ctx.abs();
        let rel = self.rel_from_abs(abs);
        let attr = self.cache.cached_attr_by_path(&rel);

        // Fast path: fully hydrated — read straight from the cache file.
        if let Some(a) = attr.as_ref() {
            if a.is_hydrated {
                if let Some(rowid) = self.cache.rowid_for_path(&rel) {
                    let lock = self.cache.key_lock(rowid);
                    let _guard = lock.read().unwrap();
                    let cache_path = self.cache.cache_file_path(rowid);
                    if let Ok(f) = fs::File::open(&cache_path) {
                        match f.seek_read(buffer, offset) {
                            Ok(n) => {
                                self.cache.touch_by_rel(&rel);
                                return Ok(n as u32);
                            }
                            Err(_) => {
                                // Cache file missing/corrupt — fall through
                                // to block-walk which will refetch.
                            }
                        }
                    }
                }
            }
        }

        // Block-walk path: serve cached runs from disk, fetch missing runs
        // from SMB and persist them. Only available if the file has a
        // rowid in the cache — cold files (never enumerated) fall back to
        // plain passthrough.
        if let (Some(a), Some(rowid)) =
            (attr.as_ref(), self.cache.rowid_for_path(&rel))
        {
            match self.read_with_bitmap(handle, abs, &rel, rowid, a.size, buffer, offset) {
                Ok(n) => {
                    self.cache.touch_by_rel(&rel);
                    return Ok(n);
                }
                Err(e) => {
                    log::warn!(
                        "[winfsp] {} block-walk failed for {} — passthrough: {:?}",
                        self.domain, rel, e
                    );
                }
            }
        }

        // Plain passthrough: no cache metadata for this file yet.
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
            OpenCtx::Dir { abs, dir_buffer, .. } => (abs, dir_buffer),
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
                write_dir_entry(&lock, ".", &rel, &self_attr);
                if !rel.is_empty() {
                    let parent_rel = match rel.rfind('/') {
                        Some(i) => rel[..i].to_string(),
                        None => String::new(),
                    };
                    if let Some(parent_attr) = self.cache.cached_attr_by_path(&parent_rel) {
                        write_dir_entry(&lock, "..", &parent_rel, &parent_attr);
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
                        write_dir_entry(&lock, "..", &parent_rel, &fake);
                    }
                }
            }

            // Real children.
            let children = self.cache.cached_children_by_parent(&rel);
            for (name, attr) in &children {
                let child_rel = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", rel, name)
                };
                write_dir_entry(&lock, name, &child_rel, attr);
            }

            drop(lock);
        }
        Ok(dir_buffer.read(marker, buffer))
    }

    // ── Write path (Slice 4) ──
    //
    // All writes go synchronously to SMB (authoritative) and invalidate
    // any cached block-level data. No coordinator / worker queue — same
    // pattern as sync/nfs_server.rs on macOS.

    fn write(
        &self,
        ctx: &Self::FileContext,
        buffer: &[u8],
        offset: u64,
        write_to_eof: bool,
        _constrained_io: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        let abs = ctx.abs().clone();
        if matches!(&**ctx, OpenCtx::Dir { .. }) {
            return Err(err_access_denied());
        }

        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(false)
            .truncate(false)
            .open(&abs)
            .map_err(|_| err_access_denied())?;

        let write_offset = if write_to_eof {
            f.metadata().map(|m| m.len()).unwrap_or(offset)
        } else {
            offset
        };
        f.seek_write(buffer, write_offset).map_err(|_| err_io())?;
        f.sync_all().map_err(|_| err_io())?;

        let rel = self.rel_from_abs(&abs);
        if !rel.is_empty() {
            self.cache.invalidate_cache_by_path(&rel);
            if let Ok(meta) = fs::metadata(&abs) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                self.cache.update_metadata_by_rel(&rel, meta.len(), mtime);
                populate_file_info(&meta, file_info, &rel);
            }
        } else if let Ok(meta) = fs::metadata(&abs) {
            populate_file_info(&meta, file_info, &rel);
        }
        Ok(buffer.len() as u32)
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let abs = self.resolve(file_name);
        let rel = Self::rel_path(file_name);
        let is_dir = (create_options & FILE_DIRECTORY_FILE) != 0
            || (file_attributes & FILE_ATTRIBUTE_DIRECTORY) != 0;

        if is_dir {
            fs::create_dir(&abs).map_err(|e| match e.kind() {
                std::io::ErrorKind::AlreadyExists => err_name_collision(),
                _ => err_io(),
            })?;
        } else {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&abs)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => err_name_collision(),
                    _ => err_io(),
                })?;
        }

        let meta = fs::metadata(&abs).map_err(|_| err_io())?;
        populate_file_info(&meta, file_info.as_mut(), &rel);

        let name = abs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let created = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(mtime);
        self.cache.record_new_entry(&rel, &name, is_dir, meta.len(), mtime, created);

        let ctx = if is_dir {
            OpenCtx::Dir {
                abs,
                dir_buffer: DirBuffer::new(),
                pending_delete: AtomicBool::new(false),
            }
        } else {
            let h = fs::File::open(&abs).map_err(|_| err_access_denied())?;
            OpenCtx::File {
                abs,
                handle: h,
                pending_delete: AtomicBool::new(false),
            }
        };
        Ok(Box::new(ctx))
    }

    fn overwrite(
        &self,
        ctx: &Self::FileContext,
        _file_attributes: u32,
        _replace_file_attributes: bool,
        allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let abs = ctx.abs().clone();
        if matches!(&**ctx, OpenCtx::Dir { .. }) {
            return Err(err_access_denied());
        }
        let rel = self.rel_from_abs(&abs);

        // Conflict pre-check: if the NAS file drifted from our cached
        // metadata (a concurrent writer got there first), preserve the
        // current NAS content as a `.conflict-<host>-<ts>` sidecar so
        // both versions survive.
        if !rel.is_empty() {
            self.preserve_conflict_sidecar_if_drifted(&rel, &abs);
        }

        let f = fs::OpenOptions::new()
            .write(true)
            .create(false)
            .truncate(true)
            .open(&abs)
            .map_err(|_| err_access_denied())?;
        if allocation_size > 0 {
            f.set_len(allocation_size).map_err(|_| err_io())?;
        }
        f.sync_all().map_err(|_| err_io())?;
        drop(f);

        if !rel.is_empty() {
            self.cache.invalidate_cache_by_path(&rel);
            if let Ok(meta) = fs::metadata(&abs) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                self.cache.update_metadata_by_rel(&rel, meta.len(), mtime);
                populate_file_info(&meta, file_info, &rel);
            }
        }
        Ok(())
    }

    fn set_file_size(
        &self,
        ctx: &Self::FileContext,
        new_size: u64,
        _set_allocation_size: bool,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let abs = ctx.abs().clone();
        if matches!(&**ctx, OpenCtx::Dir { .. }) {
            return Err(err_access_denied());
        }
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&abs)
            .map_err(|_| err_access_denied())?;
        f.set_len(new_size).map_err(|_| err_io())?;
        drop(f);
        let rel = self.rel_from_abs(&abs);
        if !rel.is_empty() {
            self.cache.invalidate_cache_by_path(&rel);
            if let Ok(meta) = fs::metadata(&abs) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                self.cache.update_metadata_by_rel(&rel, meta.len(), mtime);
                populate_file_info(&meta, file_info, &rel);
            }
        }
        Ok(())
    }

    fn set_delete(
        &self,
        ctx: &Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> winfsp::Result<()> {
        ctx.pending_delete().store(delete_file, Ordering::Relaxed);
        Ok(())
    }

    fn cleanup(
        &self,
        ctx: &Self::FileContext,
        _file_name: Option<&U16CStr>,
        _flags: u32,
    ) {
        if !ctx.pending_delete().load(Ordering::Relaxed) {
            return;
        }
        let abs = ctx.abs().clone();
        let is_dir = matches!(&**ctx, OpenCtx::Dir { .. });
        let rel = self.rel_from_abs(&abs);
        let result = if is_dir {
            fs::remove_dir(&abs)
        } else {
            fs::remove_file(&abs)
        };
        if let Err(e) = result {
            log::warn!(
                "[winfsp] {}: cleanup delete {} failed: {}",
                self.domain, abs.display(), e
            );
            return;
        }
        if !rel.is_empty() {
            self.cache.invalidate_cache_by_path(&rel);
            self.cache.remove_known_file_by_rel(&rel);
        }
    }

    fn rename(
        &self,
        _ctx: &Self::FileContext,
        file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> winfsp::Result<()> {
        let src_abs = self.resolve(file_name);
        let dst_abs = self.resolve(new_file_name);
        if !replace_if_exists && dst_abs.exists() {
            return Err(err_name_collision());
        }
        fs::rename(&src_abs, &dst_abs).map_err(|_| err_io())?;
        let src_rel = Self::rel_path(file_name);
        let dst_rel = Self::rel_path(new_file_name);
        if !src_rel.is_empty() && !dst_rel.is_empty() {
            self.cache.rename_entry_by_rel(&src_rel, &dst_rel);
        }
        Ok(())
    }

    fn flush(
        &self,
        ctx: Option<&Self::FileContext>,
        file_info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let Some(ctx) = ctx else { return Ok(()) };
        if let OpenCtx::File { abs, .. } = &**ctx {
            if let Ok(meta) = fs::metadata(abs) {
                let rel = self.rel_from_abs(abs);
                populate_file_info(&meta, file_info, &rel);
            }
        }
        Ok(())
    }

}

/// Helper: build a DirInfo from a CachedAttr + name and write it to the
/// acquired DirBufferLock. Skips on name-too-long / buffer-full errors
/// (the latter stops the iterator; WinFsp's client retries with a marker).
fn write_dir_entry(
    lock: &winfsp::filesystem::DirBufferLock<'_>,
    name: &str,
    rel: &str,
    attr: &crate::sync::cache_core::CachedAttr,
) -> bool {
    let mut dir_info: DirInfo<255> = DirInfo::new();
    populate_file_info_from_cached(attr, dir_info.file_info_mut(), rel);
    let name_os: &OsStr = name.as_ref();
    if dir_info.set_name(name_os).is_err() {
        return true; // skip this entry, continue iteration
    }
    lock.write(&mut dir_info).is_ok()
}

impl PassthroughFs {
    /// Conflict pre-flight for a truncate-style write. If the NAS file
    /// drifted from our cached metadata (a concurrent writer got there
    /// first), COPY the current NAS content to a sidecar path before the
    /// truncate proceeds. Both versions survive: the user's write lands
    /// on the original path, the other writer's version is preserved as
    /// `.conflict-<host>-<YYYYMMDD-HHMMSS>.<ext>`. Emits a
    /// `ConflictDetected` IPC message to UFB.
    ///
    /// Always returns without error — we never block a write on conflict
    /// logistics failing. Worst case: log + proceed.
    fn preserve_conflict_sidecar_if_drifted(&self, rel: &str, abs: &Path) {
        let live_meta = match fs::metadata(abs) {
            Ok(m) => m,
            Err(_) => return,
        };
        let live_size = live_meta.len();
        let live_mtime = live_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as f64)
            .unwrap_or(0.0);

        let cached = match self.cache.cached_attr_by_path(rel) {
            Some(c) => c,
            None => return,
        };

        // mtime granularity on SMB is ~1 s; allow 2 s slop.
        let drifted = live_size != cached.size || (live_mtime - cached.mtime).abs() > 2.0;
        if !drifted {
            return;
        }

        let conflict_abs = conflict::make_conflict_path(abs);
        if let Err(e) = fs::copy(abs, &conflict_abs) {
            log::error!(
                "[winfsp] {}: conflict sidecar copy failed for {} -> {}: {}",
                self.domain, rel, conflict_abs.display(), e
            );
            return;
        }
        let sidecar_name = conflict_abs
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        log::warn!(
            "[winfsp] {}: conflict on write to {} — preserved remote version as {}",
            self.domain, rel, sidecar_name
        );
        let detected_at = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let msg = AgentToUfb::ConflictDetected(ConflictDetectedMsg {
            domain: self.domain.clone(),
            original_path: rel.to_string(),
            conflict_path: sidecar_name,
            host: conflict::hostname_short(),
            detected_at,
        });
        if let Err(e) = self.ipc_tx.try_send(msg) {
            log::warn!("[winfsp] {}: conflict event dropped: {}", self.domain, e);
        }
    }

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

    /// Chunk-aware read: for each chunk in the requested range, serve
    /// cached bytes from the sparse cache file if the bitmap bit is set,
    /// else fetch from SMB (via the already-open `smb` handle) and persist
    /// to the cache. Marks the file fully hydrated when the bitmap
    /// completes. Mirrors `nfs_server.rs::read_with_bitmap`.
    #[allow(clippy::too_many_arguments)]
    fn read_with_bitmap(
        &self,
        smb: &fs::File,
        abs: &Path,
        rel: &str,
        rowid: i64,
        file_size: u64,
        buffer: &mut [u8],
        offset: u64,
    ) -> std::io::Result<u32> {
        use crate::sync::cache_core::{self, CHUNK_SIZE};

        let len = buffer.len();
        if len == 0 || offset >= file_size {
            return Ok(0);
        }
        let effective_len = (file_size - offset).min(len as u64) as usize;
        let first_chunk = offset / CHUNK_SIZE;
        let last_chunk = (offset + effective_len as u64 - 1) / CHUNK_SIZE;
        let total_chunks = cache_core::num_chunks(file_size);

        let lock_arc = self.cache.key_lock(rowid);
        let _guard = lock_arc.read().unwrap();
        let cache_path = self.cache.cache_file_path(rowid);
        if let Some(parent) = cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let cache_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&cache_path)?;
        // Pre-extend to nas_size so pread near EOF doesn't short-read on
        // a sparse hole past the current file length.
        if cache_file.metadata().map(|m| m.len()).unwrap_or(0) < file_size {
            cache_file.set_len(file_size)?;
        }

        let _ = abs; // abs comes from ctx; we already have `smb` open against it
        let mut bitmap = self.cache.get_chunk_bitmap(rel);
        let mut bitmap_dirty = false;

        let mut chunk = first_chunk;
        while chunk <= last_chunk {
            let cached = cache_core::bit_is_set(&bitmap, chunk);
            // Coalesce contiguous run with the same cached/missing state.
            let mut run_end = chunk;
            while run_end < last_chunk
                && cache_core::bit_is_set(&bitmap, run_end + 1) == cached
            {
                run_end += 1;
            }

            let run_start_byte = (chunk * CHUNK_SIZE).max(offset);
            let run_end_byte = ((run_end + 1) * CHUNK_SIZE).min(offset + effective_len as u64);
            let run_len = (run_end_byte - run_start_byte) as usize;
            let buf_offset = (run_start_byte - offset) as usize;

            if cached {
                let n = cache_file
                    .seek_read(&mut buffer[buf_offset..buf_offset + run_len], run_start_byte)?;
                if n < run_len {
                    // Short read despite the bit being set — treat as
                    // corruption. Unset the bits and fall through to
                    // refetch on the next iteration.
                    for c in chunk..=run_end {
                        bit_unset(&mut bitmap, c);
                    }
                    bitmap_dirty = true;
                    log::warn!(
                        "[winfsp] {}: short read from cache {} chunks {}..{} — invalidating",
                        self.domain, rel, chunk, run_end
                    );
                    continue;
                }
            } else {
                // Missing: fetch full chunks from SMB so the bitmap stays
                // honest (a partial chunk write would leave later reads
                // within the same chunk unaware of the holes).
                let fetch_start = chunk * CHUNK_SIZE;
                let fetch_end = ((run_end + 1) * CHUNK_SIZE).min(file_size);
                let fetch_len = (fetch_end - fetch_start) as usize;
                let mut tmp = vec![0u8; fetch_len];
                let n = smb.seek_read(&mut tmp, fetch_start)?;
                if n < fetch_len {
                    tmp.truncate(n);
                }
                // Skip writing runs of zeros — the cache file is sparse,
                // so holes pread back as zeros anyway. Keeps on-disk size
                // honest for padded media containers (ProRes, some MOV,
                // DPX).
                if !is_all_zeros(&tmp) {
                    cache_file.seek_write(&tmp, fetch_start)?;
                }
                for c in chunk..=run_end {
                    cache_core::set_bit(&mut bitmap, c);
                }
                bitmap_dirty = true;

                // Copy the user-requested window out of the freshly-fetched
                // buffer.
                let user_start = (run_start_byte - fetch_start) as usize;
                let user_end = user_start + run_len.min(tmp.len().saturating_sub(user_start));
                buffer[buf_offset..buf_offset + (user_end - user_start)]
                    .copy_from_slice(&tmp[user_start..user_end]);
            }
            chunk = run_end + 1;
        }

        if bitmap_dirty {
            if cache_core::bitmap_is_complete(&bitmap, total_chunks) {
                let _ = cache_file.sync_all();
                self.cache.mark_fully_hydrated(rel, file_size);
                log::info!(
                    "[winfsp] {}: fully hydrated {}",
                    self.domain, rel
                );
            } else {
                self.cache.update_chunk_bitmap(rel, &bitmap);
            }
        }

        Ok(effective_len as u32)
    }
}

/// True if every byte in `buf` is 0. Fast-path check used to keep the
/// sparse cache file from materializing runs of zero-padded data.
#[inline]
fn is_all_zeros(buf: &[u8]) -> bool {
    const STRIDE: usize = 8;
    let chunks = buf.chunks_exact(STRIDE);
    let remainder = chunks.remainder();
    chunks.map(|c| u64::from_ne_bytes(c.try_into().unwrap())).all(|w| w == 0)
        && remainder.iter().all(|&b| b == 0)
}

/// Clear bit `chunk` in `bitmap` (used when a cached chunk turns out to
/// be short/invalid and we need to refetch).
#[inline]
fn bit_unset(bitmap: &mut [u8], chunk: u64) {
    let byte = (chunk / 8) as usize;
    let mask = !(1u8 << ((chunk % 8) as u8));
    if let Some(b) = bitmap.get_mut(byte) {
        *b &= mask;
    }
}

/// Start a WinFsp backend for `domain` serving `nas_root`. Non-blocking —
/// spawns (a) a tokio task for the NAS health probe, (b) a tokio task for
/// eviction if the cache has a budget, and (c) an OS thread running the
/// WinFsp dispatcher (can't be tokio because WinFsp callbacks are sync).
///
/// Mount point is derived via `mount_point_for(domain)`. Must be called
/// from within a tokio runtime.
///
/// Returns a `SyncServerHandle` whose `shutdown_and_wait()` fires the
/// shutdown signal, drops the `FileSystemHost` (unmounts), and joins the
/// dispatcher thread. Callers should hold the handle for the server's
/// lifetime or explicitly call `shutdown_and_wait` when tearing down
/// (e.g. on cache-root change). Dropping the handle alone does NOT
/// shut the server down — the signal must be sent explicitly.
pub fn start(
    domain: String,
    nas_root: PathBuf,
    cache: Arc<CacheIndex>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
) -> crate::sync::SyncServerHandle {
    // (a) NAS health probe — tokio task.
    let health = NasHealth::new(domain.clone(), nas_root.clone());
    Arc::clone(&health).spawn_probe_loop();

    // (b) Eviction tick — tokio task, skipped if no budget is set. We wire
    //     it to a CancellationToken derived from the same shutdown trigger
    //     as the dispatcher thread so the tick exits cleanly on teardown.
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
    let (evict_shutdown_tx, mut evict_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    if cache.cache_limit() > 0 {
        let cache_for_evict = Arc::clone(&cache);
        let domain_for_evict = domain.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.tick().await; // skip immediate tick
            loop {
                tokio::select! {
                    _ = tick.tick() => {
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
                    _ = &mut evict_shutdown_rx => {
                        log::debug!("[winfsp] {} evictor exiting", domain_for_evict);
                        break;
                    }
                }
            }
        });
    }

    // (c) WinFsp dispatcher — OS thread. FileSystemHost keeps the mount
    //     alive until drop; blocking-recv on `shutdown_rx` holds it open
    //     until the caller signals teardown. When signalled, we drop
    //     `host` (which unmounts C:\Volumes\ufb\{share}), fire the
    //     evict shutdown, and return — joining the thread is safe.
    let mount_path = mount_point_for(&domain);
    let domain_for_thread = domain.clone();
    let thread_handle = std::thread::spawn(move || {
        let domain = domain_for_thread;
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
            let _ = evict_shutdown_tx.send(());
            return;
        }

        prepare_mount_point(&mount_path);

        let mut volume_params = VolumeParams::new();
        volume_params
            .case_preserved_names(true)
            .case_sensitive_search(false)
            .unicode_on_disk(true)
            // Required for PowerShell / .NET `Get-ChildItem -Recurse`.
            // Without this WinFsp handles pattern matching itself on every
            // enum, which drops entries when the client iterates with a
            // filter and exploded recursion in the spike.
            .pass_query_directory_pattern(true)
            // Permissive SDDL is served via `get_security_by_name`; tell
            // Windows those SDs actually mean something so ACL checks take
            // our value instead of synthesizing defaults that mis-route.
            .persistent_acls(true)
            .reparse_points(true)
            .named_streams(true)
            .post_cleanup_when_modified_only(true)
            .flush_and_purge_on_cleanup(true)
            .allow_open_in_kernel_mode(true)
            .supports_posix_unlink_rename(true)
            .post_disposition_only_when_necessary(true)
            // Metadata cache is authoritative; let WinFsp hold it long
            // enough that recursive enumeration doesn't re-enter us for
            // every path under a subtree. Eviction invalidates rows
            // explicitly on write; reads stay consistent.
            .file_info_timeout(10_000)
            .volume_info_timeout(10_000);

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
                let _ = evict_shutdown_tx.send(());
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
            let _ = evict_shutdown_tx.send(());
            return;
        }

        if let Err(e) = host.start() {
            log::error!("[winfsp] {} dispatcher start() failed: {:?}", domain, e);
            let _ = evict_shutdown_tx.send(());
            return;
        }

        log::info!("[winfsp] {} running at {}", domain, mount_path.display());

        // Block until a shutdown signal arrives. Recv returns Err only
        // when the sender side is dropped — treat that identically to a
        // deliberate signal (both mean "tear this server down cleanly").
        let _ = shutdown_rx.recv();
        log::info!("[winfsp] {} shutdown requested — unmounting", domain);

        // Dropping `host` runs FileSystemHost::drop which calls
        // FspFileSystemRemoveMountPoint + FspFileSystemStopDispatcher in
        // the right order; no need to do it manually.
        drop(host);
        // Also signal the evictor to exit — otherwise it keeps a clone
        // of the cache Arc alive after the server is gone.
        let _ = evict_shutdown_tx.send(());

        log::info!("[winfsp] {} stopped", domain);
    });

    crate::sync::SyncServerHandle::new_windows(domain, shutdown_tx, thread_handle)
}
