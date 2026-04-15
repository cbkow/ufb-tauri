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

use crate::sync::macos_cache::{CachedAttr, MacosCache};
use async_trait::async_trait;
use nfsserve::{
    nfs::{fattr3, fileid3, filename3, ftype3, mode3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3},
    tcp::{NFSTcp, NFSTcpListener},
    vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use std::{
    io::{Read, Seek, SeekFrom},
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
        self.cache.path_for_fh(fh).ok_or(nfsstat3::NFS3ERR_STALE)
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
        VFSCapabilities::ReadOnly
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
                return Ok(fh);
            }
        }

        // Cold-path: populate the parent folder, then retry.
        self.populate_folder(&parent_rel)?;
        self.cache
            .fh_for_path(&child_rel)
            .ok_or(nfsstat3::NFS3ERR_NOENT)
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

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let rel = self.rel_path(id)?;
        let abs = self.absolute(&rel);
        let mut f = std::fs::File::open(&abs).map_err(io_to_nfsstat)?;
        f.seek(SeekFrom::Start(offset)).map_err(io_to_nfsstat)?;
        let mut buf = vec![0u8; count as usize];
        let n = f.read(&mut buf).map_err(io_to_nfsstat)?;
        buf.truncate(n);
        let meta = f.metadata().map_err(io_to_nfsstat)?;
        let eof = offset + n as u64 >= meta.len();
        Ok((buf, eof))
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
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
