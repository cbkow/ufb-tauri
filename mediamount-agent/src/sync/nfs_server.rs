//! NFS3 loopback server — presents an SMB mount to Finder via macOS's native
//! kernel NFS client, sidestepping FileProvider's per-op framework overhead.
//!
//! Phase 1 (this file, current): read-only passthrough to the backing SMB
//! mount path. Identical behaviour to the standalone `mediamount-nfs-spike`
//! crate — this module exists to prove the NFS server integrates cleanly into
//! the agent process.
//!
//! Phase 1 (next iteration): swap the live `fs::read_dir` / `stat` calls for
//! SQLite-backed lookups against `MacosCache`. The request path becomes a
//! single indexed SELECT; maintenance (NAS polling, drift detection, cache
//! refresh) runs in decoupled worker tasks.
//!
//! One NFS server per sync-enabled mount. Each binds a distinct loopback port
//! (base + offset) so multiple mounts don't collide. The client mounts at
//! `~/ufb/vfs/<share>`; for now the mount is invoked manually (see README in
//! `mediamount-nfs-spike/`). Lifecycle integration with mount state changes
//! comes in Phase 1.5.

use crate::sync::macos_cache::{self, CachedAttr, MacosCache, CHUNK_SIZE};
use async_trait::async_trait;
use nfsserve::{
    nfs::{
        fattr3, fileid3, filename3, ftype3, mode3, nfspath3, nfsstat3, nfstime3, sattr3,
        set_size3, specdata3,
    },
    tcp::{NFSTcp, NFSTcpListener},
    vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use std::{
    os::unix::fs::FileExt,
    path::PathBuf,
    sync::Arc,
};

/// Base loopback port. Each enabled mount is assigned `BASE_PORT + offset`.
pub const BASE_PORT: u16 = 12345;

/// Read-only cache-backed filesystem.
///
/// Metadata is served from `MacosCache` when warm. Cold folders trigger a live
/// `fs::read_dir` that populates the cache, then serves from it — subsequent
/// visits are all SQLite.
///
/// Paths in the cache are relative to the share root (`""` = root, no leading
/// slash). We translate between relative cache paths and absolute
/// filesystem paths (rooted at `nas_root`) at the I/O boundary.
pub struct PassthroughFs {
    domain: String,
    nas_root: PathBuf,
    cache: Arc<MacosCache>,
}

impl PassthroughFs {
    pub fn new(
        domain: String,
        nas_root: PathBuf,
        cache: Arc<MacosCache>,
    ) -> Result<Self, String> {
        let canon = nas_root
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize {}: {}", nas_root.display(), e))?;
        // Root always has an fh (seeded at schema init; ensure idempotently).
        cache.ensure_fh("");
        Ok(Self { domain, nas_root: canon, cache })
    }

    fn absolute(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.nas_root.clone()
        } else {
            self.nas_root.join(rel)
        }
    }

    fn rel_path(&self, fh: fileid3) -> Result<String, nfsstat3> {
        match self.cache.path_for_fh(fh) {
            Some(p) => Ok(p),
            None => {
                let total = self.cache.nfs_handles_count();
                log::warn!(
                    "[nfs-server] {}: STALE — no nfs_handles row for fh={} (table has {} rows total)",
                    self.domain,
                    fh,
                    total
                );
                Err(nfsstat3::NFS3ERR_STALE)
            }
        }
    }

    /// Cold-path populate: do a single `fs::read_dir` on `parent_rel` and
    /// push every entry into `known_files` via `record_enumeration`. Cheap to
    /// call repeatedly (idempotent upsert).
    fn populate_folder(&self, parent_rel: &str) -> Result<(), nfsstat3> {
        let abs = self.absolute(parent_rel);
        let rd = std::fs::read_dir(&abs).map_err(io_to_nfsstat)?;
        let mut entries: Vec<crate::messages::DirEntry> = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.starts_with('@') || name.starts_with('#') {
                continue;
            }
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mtime = meta.modified().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64()).unwrap_or(0.0);
            let created = meta.created().ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64()).unwrap_or(0.0);
            entries.push(crate::messages::DirEntry {
                name,
                is_dir: meta.is_dir(),
                size: meta.len(),
                modified: mtime,
                created,
            });
        }
        self.cache.record_enumeration(parent_rel, &entries);
        Ok(())
    }

    /// Build an `fattr3` for the share root (no known_files row for "").
    fn root_attr(&self) -> Result<fattr3, nfsstat3> {
        let meta = std::fs::metadata(&self.nas_root).map_err(io_to_nfsstat)?;
        Ok(nfsserve::fs_util::metadata_to_fattr3(1, &meta))
    }

    /// Common tail for CREATE / MKDIR: stat the freshly-made entry, register
    /// it in `known_files` (trigger assigns an `fh`), return `(fh, attr)`.
    fn register_new_entry(
        &self,
        child_rel: &str,
        name: &str,
        abs: &std::path::Path,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let meta = std::fs::metadata(abs).map_err(io_to_nfsstat)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let created = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(mtime);

        self.cache.record_new_entry(
            child_rel,
            name,
            meta.is_dir(),
            meta.len(),
            mtime,
            created,
        );

        let fh = self
            .cache
            .fh_for_path(child_rel)
            .ok_or(nfsstat3::NFS3ERR_IO)?;

        log::debug!(
            "[nfs-server] {}: registered new entry {} → fh={}",
            self.domain,
            child_rel,
            fh
        );

        let attr = CachedAttr {
            is_dir: meta.is_dir(),
            size: meta.len(),
            mtime,
            created,
            is_hydrated: false,
            hydrated_size: 0,
        };
        Ok((fh, attr_from_cache(fh, &attr)))
    }

    /// Serve a read from the local cache file (fully-hydrated fast path).
    fn read_from_cache(
        &self,
        fh: fileid3,
        offset: u64,
        len: usize,
        size: u64,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let cache_path = self.cache.cache_file_path(fh);
        let f = std::fs::File::open(&cache_path).map_err(io_to_nfsstat)?;
        let mut buf = vec![0u8; len];
        let n = f.read_at(&mut buf, offset).map_err(io_to_nfsstat)?;
        buf.truncate(n);
        let eof = offset + n as u64 >= size;
        Ok((buf, eof))
    }

    /// Chunk-aware read: for each chunk covered by the request, serve from
    /// the cache if the bitmap bit is set, else pull bytes from SMB and
    /// write them to the cache file (persisting the bitmap as we go).
    ///
    /// The cache file is sparse — we only write chunks we actually fetch —
    /// and the bitmap is authoritative for "is this chunk valid" because
    /// sparse holes would otherwise read as zeros.
    async fn read_with_bitmap(
        &self,
        fh: fileid3,
        rel: &str,
        attr: &CachedAttr,
        offset: u64,
        len: usize,
        mut bitmap: Vec<u8>,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let size = attr.size;
        let first_chunk = offset / CHUNK_SIZE;
        let last_chunk = (offset + len as u64 - 1) / CHUNK_SIZE;
        let total_chunks = macos_cache::num_chunks(size);

        let cache_path = self.cache.cache_file_path(fh);
        // Open (create) the cache file. `OpenOptions::truncate(false)` so we
        // don't wipe previously-cached chunks from a prior session.
        let cache_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&cache_path)
            .map_err(io_to_nfsstat)?;
        // Ensure the sparse file is at least `size` bytes long so pread on
        // the tail chunks doesn't short-read. No disk cost — sparse holes
        // don't consume blocks until written.
        if cache_file.metadata().map(|m| m.len()).unwrap_or(0) < size {
            cache_file.set_len(size).map_err(io_to_nfsstat)?;
        }

        let abs = self.absolute(rel);
        let mut smb_file: Option<std::fs::File> = None;
        let mut result = vec![0u8; len];
        let mut bitmap_dirty = false;

        let mut chunk = first_chunk;
        while chunk <= last_chunk {
            // Expand contiguous runs of cached-or-missing chunks for one
            // pread/SMB call instead of per-chunk round-trips.
            let cached = macos_cache::bit_is_set(&bitmap, chunk);
            let mut run_end = chunk;
            while run_end < last_chunk
                && macos_cache::bit_is_set(&bitmap, run_end + 1) == cached
            {
                run_end += 1;
            }

            let run_start_byte = (chunk * CHUNK_SIZE).max(offset);
            let run_end_byte = ((run_end + 1) * CHUNK_SIZE).min(offset + len as u64);
            let run_len = (run_end_byte - run_start_byte) as usize;
            let result_offset = (run_start_byte - offset) as usize;

            if cached {
                let n = cache_file
                    .read_at(&mut result[result_offset..result_offset + run_len], run_start_byte)
                    .map_err(io_to_nfsstat)?;
                if n < run_len {
                    // Shouldn't happen (file len >= size, bit was set), but
                    // guard: treat as corruption → invalidate bits + refetch.
                    for c in chunk..=run_end {
                        bit_unset(&mut bitmap, c);
                    }
                    bitmap_dirty = true;
                    log::warn!(
                        "[nfs-server] {}: short read from cache for {} run {}..{}, invalidating",
                        self.domain, rel, chunk, run_end
                    );
                    // Fall through to the missing-path for these chunks next iter.
                    continue;
                }
            } else {
                // Missing: fetch from SMB (reuse an opened file handle across
                // multiple missing runs in this read).
                let f = match smb_file {
                    Some(ref f) => f,
                    None => {
                        smb_file = Some(
                            std::fs::File::open(&abs).map_err(io_to_nfsstat)?,
                        );
                        smb_file.as_ref().unwrap()
                    }
                };

                // We need to write whole chunks to the cache file (so bitmap
                // bits are honest). That means fetching from SMB for the full
                // chunk range, not just the user's offset window.
                let fetch_start = chunk * CHUNK_SIZE;
                let fetch_end = ((run_end + 1) * CHUNK_SIZE).min(size);
                let fetch_len = (fetch_end - fetch_start) as usize;
                let mut buf = vec![0u8; fetch_len];
                let n = f.read_at(&mut buf, fetch_start).map_err(io_to_nfsstat)?;
                if n < fetch_len {
                    buf.truncate(n);
                }
                cache_file
                    .write_at(&buf, fetch_start)
                    .map_err(io_to_nfsstat)?;
                for c in chunk..=run_end {
                    macos_cache::set_bit(&mut bitmap, c);
                }
                bitmap_dirty = true;

                // Copy the user-requested slice out of the freshly-fetched buffer.
                let user_start = (run_start_byte - fetch_start) as usize;
                let user_end = user_start + run_len.min(buf.len() - user_start);
                result[result_offset..result_offset + (user_end - user_start)]
                    .copy_from_slice(&buf[user_start..user_end]);
            }

            chunk = run_end + 1;
        }

        if bitmap_dirty {
            if macos_cache::bitmap_is_complete(&bitmap, total_chunks) {
                // Whole file is cached — flip the fast-path flag and null the bitmap.
                let _ = cache_file.sync_all();
                self.cache.mark_fully_hydrated(rel, size);
                log::info!("[nfs-server] {}: fully hydrated {}", self.domain, rel);
            } else {
                self.cache.update_chunk_bitmap(rel, &bitmap);
            }
        }

        let eof = offset + len as u64 >= size;
        Ok((result, eof))
    }
}

/// Clear a bit in the chunk bitmap (used when a cached chunk turns out to
/// be invalid and we need to refetch).
#[inline]
fn bit_unset(bitmap: &mut [u8], chunk: u64) {
    let byte = (chunk / 8) as usize;
    let mask = !(1u8 << ((chunk % 8) as u8));
    if let Some(b) = bitmap.get_mut(byte) {
        *b &= mask;
    }
}

/// Convert a `CachedAttr` + fh into an `fattr3`. Used for all non-root entries.
fn attr_from_cache(fh: fileid3, a: &CachedAttr) -> fattr3 {
    let ftype = if a.is_dir { ftype3::NF3DIR } else { ftype3::NF3REG };
    let mode: mode3 = if a.is_dir { 0o755 } else { 0o644 };
    let size = if a.is_dir { 4096 } else { a.size };
    let atime = nfstime3 {
        seconds: a.mtime as u32,
        nseconds: ((a.mtime.fract()) * 1e9) as u32,
    };
    let mtime = atime;
    let ctime = nfstime3 {
        seconds: a.created as u32,
        nseconds: ((a.created.fract()) * 1e9) as u32,
    };
    fattr3 {
        ftype,
        mode,
        nlink: if a.is_dir { 2 } else { 1 },
        uid: 501,
        gid: 20,
        size,
        used: if a.is_hydrated { a.hydrated_size } else { size },
        rdev: specdata3::default(),
        fsid: 0,
        fileid: fh,
        atime,
        mtime,
        ctime,
    }
}

fn io_to_nfsstat(e: std::io::Error) -> nfsstat3 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => nfsstat3::NFS3ERR_NOENT,
        PermissionDenied => nfsstat3::NFS3ERR_ACCES,
        AlreadyExists => nfsstat3::NFS3ERR_EXIST,
        _ => nfsstat3::NFS3ERR_IO,
    }
}

#[async_trait]
impl NFSFileSystem for PassthroughFs {
    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadWrite
    }

    fn root_dir(&self) -> fileid3 {
        // Root is seeded at schema init as path="". Its fh is always 1 on a
        // fresh DB; on an upgraded DB it's whatever AUTOINCREMENT assigned
        // the first time we inserted — so we look it up, not hardcode.
        self.cache.fh_for_path("").unwrap_or(1)
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let parent_rel = self.rel_path(dirid)?;
        let name = filename_to_string(filename)?;

        // Warm-path: straight SQLite lookup. The trigger on known_files has
        // already mirrored the path into nfs_handles, so fh_for_path hits.
        let child_rel = join_rel(&parent_rel, &name);
        if let Some(fh) = self.cache.fh_for_path(&child_rel) {
            // Still verify it's a known child of the parent (defence against
            // a stale handle lingering for a different folder).
            if self.cache.cached_attr(&child_rel).is_some() {
                log::debug!(
                    "[nfs-server] {}: lookup(warm) {} → fh={}",
                    self.domain, child_rel, fh
                );
                return Ok(fh);
            }
        }

        // Cold-path: populate the parent folder, then retry.
        self.populate_folder(&parent_rel)?;
        let result = self.cache.fh_for_path(&child_rel);
        match result {
            Some(fh) => {
                log::debug!(
                    "[nfs-server] {}: lookup(cold) {} → fh={}",
                    self.domain, child_rel, fh
                );
                Ok(fh)
            }
            None => {
                log::debug!(
                    "[nfs-server] {}: lookup {} → NOENT",
                    self.domain, child_rel
                );
                Err(nfsstat3::NFS3ERR_NOENT)
            }
        }
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let rel = self.rel_path(id)?;
        if rel.is_empty() {
            return self.root_attr();
        }
        if let Some(attr) = self.cache.cached_attr(&rel) {
            return Ok(attr_from_cache(id, &attr));
        }
        // Not in known_files — try populating the parent folder.
        let parent = crate::sync::macos_cache::parent_of(&rel).to_string();
        self.populate_folder(&parent)?;
        let attr = self
            .cache
            .cached_attr(&rel)
            .ok_or(nfsstat3::NFS3ERR_NOENT)?;
        Ok(attr_from_cache(id, &attr))
    }

    async fn setattr(&self, id: fileid3, setattr: sattr3) -> Result<fattr3, nfsstat3> {
        let rel = self.rel_path(id)?;
        if rel.is_empty() {
            return Err(nfsstat3::NFS3ERR_ROFS);
        }
        let abs = self.absolute(&rel);

        // We honour truncate (editors do save-as-truncate-then-write). Mode,
        // uid, gid, atime, mtime are passthrough no-ops — SMB doesn't expose
        // fine-grained control we could proxy here, and NFS clients treat
        // them as advisory.
        if let set_size3::size(new_size) = setattr.size {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&abs)
                .map_err(io_to_nfsstat)?;
            f.set_len(new_size).map_err(io_to_nfsstat)?;
            f.sync_all().map_err(io_to_nfsstat)?;
            drop(f);

            // Cached content is now stale — nuke it and refresh metadata.
            self.cache.invalidate_cache(&rel, id);
            if let Ok(meta) = std::fs::metadata(&abs) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                self.cache.update_nas_metadata(&rel, new_size, mtime);
            }
        }

        self.getattr(id).await
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let rel = self.rel_path(id)?;
        if rel.is_empty() {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        let attr = self.cache.cached_attr(&rel).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        if attr.is_dir {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        let size = attr.size;

        // Clamp request to file size — NFS convention.
        let read_end = (offset + count as u64).min(size);
        if offset >= size {
            return Ok((Vec::new(), true));
        }
        let read_len = (read_end - offset) as usize;

        // Fast path: fully hydrated.
        if attr.is_hydrated {
            return self.read_from_cache(id, offset, read_len, size);
        }

        // Chunk-level path: walk the requested range chunk-by-chunk, serving
        // each from cache if the bit is set, otherwise from SMB (and writing
        // to cache as we go).
        let bitmap = self.cache.get_chunk_bitmap(&rel);
        self.read_with_bitmap(id, &rel, &attr, offset, read_len, bitmap).await
    }

    async fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> Result<fattr3, nfsstat3> {
        let rel = self.rel_path(id)?;
        if rel.is_empty() {
            return Err(nfsstat3::NFS3ERR_ISDIR);
        }
        let abs = self.absolute(&rel);

        // Write-through to SMB — authoritative. Keep file alive (create=false)
        // so we don't clobber via open-truncate semantics; writes from NFS
        // always target an existing path (CREATE makes the initial file).
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(false)
            .truncate(false)
            .open(&abs)
            .map_err(io_to_nfsstat)?;
        f.write_at(data, offset).map_err(io_to_nfsstat)?;
        f.sync_all().map_err(io_to_nfsstat)?;

        // Invalidate our cache — SMB is truth. Subsequent reads will
        // re-hydrate on demand. (An in-place cache update is a Phase 3.1
        // optimisation; correctness first.)
        self.cache.invalidate_cache(&rel, id);

        // Refresh cached metadata from the post-write stat.
        let meta = std::fs::metadata(&abs).map_err(io_to_nfsstat)?;
        let new_size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.cache.update_nas_metadata(&rel, new_size, mtime);

        // Build fresh attr from the post-write metadata.
        let created = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(mtime);
        let attr = CachedAttr {
            is_dir: false,
            size: new_size,
            mtime,
            created,
            is_hydrated: false,
            hydrated_size: 0,
        };
        Ok(attr_from_cache(id, &attr))
    }

    async fn create(
        &self,
        dirid: fileid3,
        filename: &filename3,
        attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let parent_rel = self.rel_path(dirid)?;
        let name = filename_to_string(filename)?;
        let child_rel = join_rel(&parent_rel, &name);
        let abs = self.absolute(&child_rel);

        // CREATE (UNCHECKED) — overwrite is fine. `create(true)+truncate(true)`
        // matches NFS semantics: if the file exists, the content is wiped.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&abs)
            .map_err(io_to_nfsstat)?;
        // Honor `sattr3.size` if the client requested a non-zero initial size
        // (rare but the protocol allows it).
        if let set_size3::size(new_size) = attr.size {
            if new_size > 0 {
                f.set_len(new_size).map_err(io_to_nfsstat)?;
            }
        }
        drop(f);

        self.register_new_entry(&child_rel, &name, &abs)
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        let parent_rel = self.rel_path(dirid)?;
        let name = filename_to_string(filename)?;
        let child_rel = join_rel(&parent_rel, &name);
        let abs = self.absolute(&child_rel);

        // EXCLUSIVE — fail if exists. `create_new` surfaces `EEXIST` which
        // maps to `NFS3ERR_EXIST` via io_to_nfsstat.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&abs)
            .map_err(io_to_nfsstat)?;

        let (fh, _attr) = self.register_new_entry(&child_rel, &name, &abs)?;
        Ok(fh)
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        let name_preview = filename_to_string(dirname).unwrap_or_default();
        log::debug!(
            "[nfs-server] {}: mkdir(dirid={}, name={:?})",
            self.domain, dirid, name_preview
        );
        let parent_rel = self.rel_path(dirid)?;
        let name = filename_to_string(dirname)?;
        let child_rel = join_rel(&parent_rel, &name);
        let abs = self.absolute(&child_rel);

        std::fs::create_dir(&abs).map_err(io_to_nfsstat)?;

        self.register_new_entry(&child_rel, &name, &abs)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let parent_rel = self.rel_path(dirid)?;

        // If this folder hasn't been enumerated, do one cold read to populate.
        if !self.cache.folder_is_enumerated(&parent_rel) {
            self.populate_folder(&parent_rel)?;
        }

        let mut children = self.cache.cached_children(&parent_rel);
        children.sort_by(|a, b| a.1.cmp(&b.1));

        let mut result = ReadDirResult::default();
        let mut passed_start = start_after == 0;

        for (fh, name, attr) in children {
            if !passed_start {
                if fh == start_after {
                    passed_start = true;
                }
                continue;
            }
            if result.entries.len() >= max_entries {
                return Ok(result);
            }
            let attr3 = attr_from_cache(fh, &attr);
            result.entries.push(DirEntry {
                fileid: fh,
                name: name.into_bytes().into(),
                attr: attr3,
            });
        }
        result.end = true;
        Ok(result)
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let rel = self.rel_path(id)?;
        let abs = self.absolute(&rel);
        let target = std::fs::read_link(&abs).map_err(io_to_nfsstat)?;
        use std::os::unix::ffi::OsStrExt;
        let bytes: Vec<u8> = target.as_os_str().as_bytes().to_vec();
        Ok(bytes.into())
    }

    /// Single-export server — the client's mount path is ignored; we always
    /// hand back root. Without this override the default impl walks path
    /// components via lookup(), so `localhost:/anything` would ENOENT.
    async fn path_to_id(&self, _path: &[u8]) -> Result<fileid3, nfsstat3> {
        Ok(self.root_dir())
    }

    /// Persistent NFS file handle encoding. The default `nfsserve` impl
    /// prefixes with a generation number derived from server startup time,
    /// which invalidates every client-cached handle on agent restart. Our
    /// `fh` comes from `nfs_handles.fh` (SQLite AUTOINCREMENT — never reused)
    /// and persists across restarts, so we use a fixed zero generation:
    /// handles survive agent bounces without `umount`/`mount`.
    fn id_to_fh(&self, id: fileid3) -> nfsserve::nfs::nfs_fh3 {
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&[0u8; 8]);
        data.extend_from_slice(&id.to_le_bytes());
        nfsserve::nfs::nfs_fh3 { data }
    }

    fn fh_to_id(&self, fh: &nfsserve::nfs::nfs_fh3) -> Result<fileid3, nfsstat3> {
        if fh.data.len() != 16 {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        let bytes: [u8; 8] = fh.data[8..16]
            .try_into()
            .map_err(|_| nfsstat3::NFS3ERR_BADHANDLE)?;
        Ok(u64::from_le_bytes(bytes))
    }
}

fn filename_to_string(f: &filename3) -> Result<String, nfsstat3> {
    std::str::from_utf8(f.0.as_ref())
        .map(|s| s.to_string())
        .map_err(|_| nfsstat3::NFS3ERR_IO)
}

fn join_rel(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{}/{}", parent, child)
    }
}

/// Start one NFS server bound to `127.0.0.1:<port>`, serving `nas_root` as
/// the export root and using `cache` as the metadata authority. Spawns a
/// background tokio task; returns immediately. If bind or handshake fails,
/// logs the error and the task exits.
pub fn start(domain: String, nas_root: PathBuf, port: u16, cache: Arc<MacosCache>) {
    tokio::spawn(async move {
        let fs = match PassthroughFs::new(domain.clone(), nas_root.clone(), cache) {
            Ok(fs) => fs,
            Err(e) => {
                log::error!(
                    "[nfs-server] {} failed to create passthrough fs for {}: {}",
                    domain,
                    nas_root.display(),
                    e
                );
                return;
            }
        };

        let bind = format!("127.0.0.1:{}", port);
        let listener = match NFSTcpListener::bind(&bind, fs).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("[nfs-server] {} failed to bind {}: {}", domain, bind, e);
                return;
            }
        };

        log::info!(
            "[nfs-server] {} listening on {} (nas_root={}) — mount with: \
             mkdir -p ~/ufb/vfs/{} && mount -t nfs \
             -o \"port={port},mountport={port},nolocks,vers=3,tcp,nobrowse,actimeo=1,rsize=1048576,wsize=1048576\" \
             localhost:/{} ~/ufb/vfs/{}",
            domain,
            bind,
            nas_root.display(),
            domain,
            domain,
            domain,
            port = port,
        );

        if let Err(e) = listener.handle_forever().await {
            log::error!("[nfs-server] {} handle_forever exited: {}", domain, e);
        }
    });
}
