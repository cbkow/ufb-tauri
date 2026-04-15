//! NFS3 loopback passthrough — throwaway spike.
//!
//! Exposes any local directory (including an SMB mount point) as a loopback NFS3 share.
//! The filesystem implementation is deliberately minimal — we just map NFS ops to the
//! matching `std::fs` calls. No caching, no SQLite, no UFB integration. The only goal
//! is to measure whether the kernel NFS client + our user-space server is dramatically
//! faster than FileProvider for navigation-heavy workloads.
//!
//! See README.md for the full three-way measurement recipe.

use async_trait::async_trait;
use clap::Parser;
use nfsserve::{
    nfs::{fattr3, fileid3, filename3, nfspath3, nfsstat3, sattr3},
    tcp::{NFSTcp, NFSTcpListener},
    vfs::{DirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities},
};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::RwLock;

#[derive(Parser, Debug)]
struct Args {
    /// Local directory to expose over NFS (can be an SMB mount point).
    #[arg(long)]
    root: PathBuf,
    /// Bind address + port for the NFS server.
    #[arg(long, default_value = "127.0.0.1:12345")]
    bind: String,
}

/// Minimal id<->path map. Root is fileid 1 (0 is reserved by NFS).
struct IdTable {
    next: AtomicU64,
    by_id: HashMap<fileid3, PathBuf>,
    by_path: HashMap<PathBuf, fileid3>,
}

impl IdTable {
    fn new(root: PathBuf) -> Self {
        let mut by_id = HashMap::new();
        let mut by_path = HashMap::new();
        by_id.insert(1, root.clone());
        by_path.insert(root, 1);
        Self { next: AtomicU64::new(2), by_id, by_path }
    }

    fn get_or_insert(&mut self, path: PathBuf) -> fileid3 {
        if let Some(&id) = self.by_path.get(&path) {
            return id;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.by_id.insert(id, path.clone());
        self.by_path.insert(path, id);
        id
    }

    fn path(&self, id: fileid3) -> Option<&Path> {
        self.by_id.get(&id).map(|p| p.as_path())
    }
}

struct PassthroughFs {
    table: RwLock<IdTable>,
}

impl PassthroughFs {
    fn new(root: PathBuf) -> Self {
        let canon = root.canonicalize().expect("root must exist and be canonicalizable");
        Self { table: RwLock::new(IdTable::new(canon)) }
    }

    async fn resolve(&self, id: fileid3) -> Result<PathBuf, nfsstat3> {
        let t = self.table.read().await;
        t.path(id).map(|p| p.to_path_buf()).ok_or(nfsstat3::NFS3ERR_STALE)
    }

    async fn intern(&self, path: PathBuf) -> fileid3 {
        let mut t = self.table.write().await;
        t.get_or_insert(path)
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

fn fname_to_osstr(f: &filename3) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(f.0.as_ref()).to_os_string()
}

#[async_trait]
impl NFSFileSystem for PassthroughFs {
    fn capabilities(&self) -> VFSCapabilities {
        // Read-only for the spike — we're measuring nav + read throughput, not writes.
        VFSCapabilities::ReadOnly
    }

    fn root_dir(&self) -> fileid3 {
        1
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let dir = self.resolve(dirid).await?;
        let child = dir.join(fname_to_osstr(filename));
        if !child.exists() {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        Ok(self.intern(child).await)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let path = self.resolve(id).await?;
        let meta = std::fs::metadata(&path).map_err(io_to_nfsstat)?;
        Ok(nfsserve::fs_util::metadata_to_fattr3(id, &meta))
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
        let path = self.resolve(id).await?;
        let mut f = std::fs::File::open(&path).map_err(io_to_nfsstat)?;
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
        let dir = self.resolve(dirid).await?;
        let mut entries: Vec<(PathBuf, std::ffi::OsString)> = std::fs::read_dir(&dir)
            .map_err(io_to_nfsstat)?
            .filter_map(|e| e.ok())
            .map(|e| (e.path(), e.file_name()))
            .collect();
        // Deterministic order — the trait contract requires it for pagination.
        entries.sort_by(|a, b| a.1.cmp(&b.1));

        let mut result = ReadDirResult::default();
        let mut passed_start = start_after == 0;

        for (path, name) in entries {
            let id = self.intern(path.clone()).await;
            if !passed_start {
                if id == start_after {
                    passed_start = true;
                }
                continue;
            }
            if result.entries.len() >= max_entries {
                return Ok(result);
            }
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let attr = nfsserve::fs_util::metadata_to_fattr3(id, &meta);
            let name_bytes: Vec<u8> = {
                use std::os::unix::ffi::OsStrExt;
                name.as_os_str().as_bytes().to_vec()
            };
            result.entries.push(DirEntry {
                fileid: id,
                name: name_bytes.into(),
                attr,
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
        let path = self.resolve(id).await?;
        let target = std::fs::read_link(&path).map_err(io_to_nfsstat)?;
        use std::os::unix::ffi::OsStrExt;
        let bytes: Vec<u8> = target.as_os_str().as_bytes().to_vec();
        Ok(bytes.into())
    }

    /// Single-export server: the mount path the client sent is irrelevant — we
    /// always hand back the root. Without this override, the default impl walks
    /// the path components via lookup(), so a client mounting `localhost:/spike`
    /// would try to lookup("spike") under the root and get NFS3ERR_NOENT.
    async fn path_to_id(&self, _path: &[u8]) -> Result<fileid3, nfsstat3> {
        Ok(self.root_dir())
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nfsserve=info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();
    tracing::info!("NFS spike: root={} bind={}", args.root.display(), args.bind);

    let fs = PassthroughFs::new(args.root);
    let listener = NFSTcpListener::bind(&args.bind, fs).await?;
    let port = args.bind.rsplit(':').next().unwrap_or("12345");
    tracing::info!("listening — mount with:");
    tracing::info!(
        "  mkdir -p ~/ufb/vfs-spike && \\\n  mount -t nfs -o \"port={port},mountport={port},nolocks,vers=3,tcp,nobrowse,actimeo=1\" localhost:/spike ~/ufb/vfs-spike"
    );
    listener.handle_forever().await?;
    Ok(())
}
