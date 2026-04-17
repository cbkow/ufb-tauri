//! WinFsp pass-through spike — Step 2 of `docs/windows-io-backend-evaluation.md`.
//!
//! Uses the SnowflakePowered `winfsp` crate (native `FSP_FILE_SYSTEM_INTERFACE`,
//! not FUSE). Read-only pass-through to an SMB backing store — no cache, no
//! partial-hydration logic; every callback reads from the NAS on demand.
//!
//! Entry point: `--winfsp-spike <mount_path> <nas_root>` CLI flag from
//! `main.rs`.
//!
//! License: `winfsp` crate is GPL-3.0, underlying WinFsp DLL is GPL-3.0.
//! UFB is planned to ship under GPL-3.0 to satisfy WinFsp's FLOSS exception.

use std::ffi::{c_void, OsStr};
use std::fs;
use std::os::windows::fs::FileExt;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
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

fn err_not_found() -> FspError { FspError::NTSTATUS(STATUS_OBJECT_NAME_NOT_FOUND) }
fn err_access_denied() -> FspError { FspError::NTSTATUS(STATUS_ACCESS_DENIED) }

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

pub struct PassthroughFs {
    nas_root: PathBuf,
}

impl PassthroughFs {
    fn resolve(&self, name: &U16CStr) -> PathBuf {
        let s = name.to_string_lossy();
        let trimmed = s.trim_start_matches('\\').trim_start_matches('/');
        if trimmed.is_empty() {
            self.nas_root.clone()
        } else {
            self.nas_root.join(trimmed.replace('/', "\\"))
        }
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

impl FileSystemContext for PassthroughFs {
    type FileContext = Box<OpenCtx>;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        _security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let abs = self.resolve(file_name);
        log::info!("[winfsp] get_security_by_name abs={}", abs.display());
        let meta = match fs::metadata(&abs) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("[winfsp] metadata failed {}: {}", abs.display(), e);
                return Err(err_not_found());
            }
        };
        let attrs = if meta.is_dir() {
            FILE_ATTRIBUTE_DIRECTORY
        } else {
            FILE_ATTRIBUTE_NORMAL
        };
        Ok(FileSecurity {
            attributes: attrs,
            sz_security_descriptor: 0,
            reparse: false,
        })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let abs = self.resolve(file_name);
        log::info!("[winfsp] open abs={}", abs.display());
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
        log::info!(
            "[winfsp] read_directory abs={} marker_none={}",
            abs.display(),
            marker.is_none()
        );

        if marker.is_none() {
            let lock = dir_buffer.acquire(true, None)?;
            let rd = match fs::read_dir(abs) {
                Ok(r) => r,
                Err(_) => return Err(err_not_found()),
            };
            let mut entries: Vec<(String, fs::Metadata)> = rd
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let meta = e.metadata().ok()?;
                    Some((name, meta))
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, meta) in &entries {
                let mut dir_info: DirInfo<255> = DirInfo::new();
                populate_file_info(meta, dir_info.file_info_mut());
                let name_os: &OsStr = name.as_ref();
                if dir_info.set_name(name_os).is_err() {
                    continue;
                }
                if lock.write(&mut dir_info).is_err() {
                    break;
                }
            }
            drop(lock);
        }
        Ok(dir_buffer.read(marker, buffer))
    }
}

pub fn start_spike(
    mount_path: PathBuf,
    nas_root: PathBuf,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        log::info!(
            "[winfsp] spike starting: mount_path={}, nas_root={}",
            mount_path.display(),
            nas_root.display()
        );

        if winfsp_init().is_err() {
            log::error!("[winfsp] winfsp_init failed — is WinFsp runtime installed?");
            return;
        }

        let mut volume_params = VolumeParams::new();
        volume_params
            .case_preserved_names(true)
            .case_sensitive_search(false)
            .unicode_on_disk(true)
            .persistent_acls(false)
            .read_only_volume(true)
            .post_cleanup_when_modified_only(true)
            .file_info_timeout(1000)
            .volume_info_timeout(1000);

        let mut host = match FileSystemHost::new(volume_params, PassthroughFs { nas_root }) {
            Ok(h) => h,
            Err(e) => {
                log::error!("[winfsp] FileSystemHost::new failed: {:?}", e);
                return;
            }
        };

        let mount_os: &OsStr = mount_path.as_os_str();
        if let Err(e) = host.mount(mount_os) {
            log::error!(
                "[winfsp] mount({}) failed: {:?}",
                mount_path.display(),
                e
            );
            return;
        }

        if let Err(e) = host.start() {
            log::error!("[winfsp] start() failed: {:?}", e);
            return;
        }

        log::info!("[winfsp] spike running at {}", mount_path.display());

        loop {
            std::thread::park();
        }
    })
}
