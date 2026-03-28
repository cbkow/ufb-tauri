/// Native OLE drag-out on Windows.
/// Builds a CF_HDROP-based IDataObject and calls DoDragDrop.
/// Using CF_HDROP directly (rather than shell item array) ensures compatibility
/// with apps that don't handle delayed-rendering shell data objects.

#[cfg(target_os = "windows")]
pub fn start_native_drag(_window: &tauri::WebviewWindow, paths: &[String]) -> std::result::Result<String, String> {
    use std::path::PathBuf;
    use std::ptr;
    use std::sync::Once;
    use windows::core::*;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Ole::*;

    if paths.is_empty() {
        return Ok("cancelled".to_string());
    }

    // OleInitialize once for this thread
    static mut OLE_RESULT: Result<()> = Ok(());
    static OLE_INIT: Once = Once::new();
    OLE_INIT.call_once(|| unsafe {
        OLE_RESULT = OleInitialize(Some(ptr::null_mut()));
    });
    unsafe {
        #[allow(static_mut_refs)]
        if let Err(e) = &OLE_RESULT {
            return Err(format!("OleInitialize failed: {}", e));
        }
    }

    // Use paths as-is (preserve junction paths). Do NOT canonicalize —
    // that resolves junctions to their target, breaking path consistency.
    let canonical: Vec<PathBuf> = paths
        .iter()
        .map(|p| PathBuf::from(p))
        .filter(|p| p.exists())
        .collect();

    if canonical.is_empty() {
        return Ok("cancelled".to_string());
    }

    // Build DROPFILES + wide-string file list
    let hmem = build_cf_hdrop(&canonical)
        .map_err(|e| format!("Failed to build CF_HDROP: {}", e))?;

    unsafe {
        let data_object: windows::Win32::System::Com::IDataObject =
            HDropDataObject { hmem }.into();
        let drop_source: IDropSource = DropSource.into();

        let mut effect = DROPEFFECT(0);
        let hr = DoDragDrop(
            &data_object,
            &drop_source,
            DROPEFFECT_COPY | DROPEFFECT_MOVE,
            &mut effect,
        );

        match hr {
            DRAGDROP_S_DROP => {
                if effect.contains(DROPEFFECT_MOVE) {
                    Ok("moved".to_string())
                } else {
                    Ok("copied".to_string())
                }
            }
            _ => Ok("cancelled".to_string()),
        }
    }
}

/// CF_HDROP = 15
#[cfg(target_os = "windows")]
const CF_HDROP: u16 = 15;

/// Build an HGLOBAL containing a DROPFILES struct followed by null-terminated
/// wide-string paths, double-null terminated.
#[cfg(target_os = "windows")]
fn build_cf_hdrop(
    paths: &[std::path::PathBuf],
) -> std::result::Result<windows::Win32::Foundation::HGLOBAL, String> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows::Win32::System::Memory::*;

    #[repr(C, packed(1))]
    struct DROPFILES {
        p_files: u32,
        pt_x: i32,
        pt_y: i32,
        f_nc: i32,
        f_wide: i32,
    }
    const DROPFILES_SIZE: usize = std::mem::size_of::<DROPFILES>();

    // Encode all paths as wide strings (each null-terminated)
    let wide_paths: Vec<Vec<u16>> = paths
        .iter()
        .map(|p| p.as_os_str().encode_wide().chain(once(0)).collect())
        .collect();

    // Total size: DROPFILES header + all wide chars + final null terminator (u16)
    let total_chars: usize = wide_paths.iter().map(|w| w.len()).sum();
    let mem_size = DROPFILES_SIZE + (total_chars + 1) * 2; // +1 for final null u16

    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, mem_size)
            .map_err(|e| format!("GlobalAlloc failed: {}", e))?;
        let lock: *mut std::ffi::c_void = GlobalLock(hmem);
        if lock.is_null() {
            return Err("GlobalLock failed".to_string());
        }

        // Write DROPFILES header
        let df = DROPFILES {
            p_files: DROPFILES_SIZE as u32,
            pt_x: 0,
            pt_y: 0,
            f_nc: 0,
            f_wide: 1, // Unicode
        };
        ptr::write(lock as *mut DROPFILES, df);

        // Write paths
        let mut dst = (lock as *mut u8).add(DROPFILES_SIZE) as *mut u16;
        for wide in &wide_paths {
            ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
            dst = dst.add(wide.len());
        }
        // Final null terminator (end of list)
        *dst = 0;

        let _ = GlobalUnlock(hmem);
        Ok(hmem)
    }
}

/// macOS: use the `drag` crate to initiate a native drag session.
/// Called on the main thread via app.run_on_main_thread() from the command handler.
/// The drag session is non-blocking on macOS — beginDraggingSession returns immediately
/// and macOS manages the drag lifecycle.
#[cfg(target_os = "macos")]
pub fn start_native_drag(window: &tauri::WebviewWindow, paths: &[String]) -> std::result::Result<String, String> {
    use std::path::PathBuf;

    if paths.is_empty() {
        return Ok("cancelled".to_string());
    }

    // Canonicalize paths to resolve symlinks (e.g. /opt/ufb/mounts/nas → /Volumes/share)
    let resolved: Vec<PathBuf> = paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok().or_else(|| Some(PathBuf::from(p))))
        .collect();

    if resolved.is_empty() {
        return Ok("cancelled".to_string());
    }

    let item = drag::DragItem::Files(resolved);
    // 1x1 transparent PNG as drag preview — macOS will show file icons
    let image = drag::Image::Raw(vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
        0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x9C, 0x62, 0x00, 0x00, 0x00, 0x02,
        0x00, 0x01, 0xE5, 0x27, 0xDE, 0xFC, 0x00, 0x00,
        0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
        0x60, 0x82,
    ]);

    // start_drag on macOS is non-blocking — it starts the session and returns.
    // The callback fires when the drag ends.
    let result = drag::start_drag(
        window,
        item,
        image,
        move |result, _cursor_pos| {
            log::info!("macOS drag result: {:?}", result);
        },
        drag::Options::default(),
    );

    match result {
        Ok(()) => Ok("started".to_string()),
        Err(e) => Err(format!("Drag failed: {}", e)),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn start_native_drag(_window: &tauri::WebviewWindow, _paths: &[String]) -> std::result::Result<String, String> {
    Err("Native drag-out not implemented on this platform".to_string())
}

// ── IDropSource ──

#[cfg(target_os = "windows")]
use windows::Win32::System::Ole::*;

#[cfg(target_os = "windows")]
#[windows::core::implement(IDropSource)]
struct DropSource;

#[cfg(target_os = "windows")]
impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: windows::Win32::Foundation::BOOL,
        grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    ) -> windows::core::HRESULT {
        use windows::Win32::System::SystemServices::MK_LBUTTON;
        use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;

        if fescapepressed.as_bool() {
            windows::Win32::Foundation::DRAGDROP_S_CANCEL
        } else if (grfkeystate & MK_LBUTTON) == MODIFIERKEYS_FLAGS(0) {
            windows::Win32::Foundation::DRAGDROP_S_DROP
        } else {
            windows::Win32::Foundation::S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        windows::Win32::Foundation::DRAGDROP_S_USEDEFAULTCURSORS
    }
}

// ── Minimal IDataObject that serves CF_HDROP ──

#[cfg(target_os = "windows")]
use windows::Win32::System::Com::{
    IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IAdviseSink,
    FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};

#[cfg(target_os = "windows")]
fn is_cf_hdrop_request(fmt: *const FORMATETC) -> bool {
    unsafe {
        let f = &*fmt;
        f.cfFormat == CF_HDROP && (f.tymed & TYMED_HGLOBAL.0 as u32) != 0
    }
}

#[cfg(target_os = "windows")]
#[windows::core::implement(IDataObject)]
struct HDropDataObject {
    hmem: windows::Win32::Foundation::HGLOBAL,
}

#[cfg(target_os = "windows")]
impl IDataObject_Impl for HDropDataObject_Impl {
    fn GetData(
        &self,
        pformatetcin: *const FORMATETC,
    ) -> windows::core::Result<STGMEDIUM> {
        use windows::Win32::System::Memory::*;

        if !is_cf_hdrop_request(pformatetcin) {
            return Err(windows::core::Error::from(
                windows::Win32::Foundation::DV_E_FORMATETC,
            ));
        }

        unsafe {
            // Duplicate the HGLOBAL so the caller can free their copy
            let size = GlobalSize(self.hmem);
            let src: *mut std::ffi::c_void = GlobalLock(self.hmem);
            let copy = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size)
                .map_err(|_| {
                    let _ = GlobalUnlock(self.hmem);
                    windows::core::Error::from(windows::Win32::Foundation::E_OUTOFMEMORY)
                })?;
            let dst: *mut std::ffi::c_void = GlobalLock(copy);
            std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, size);
            let _ = GlobalUnlock(self.hmem);
            let _ = GlobalUnlock(copy);

            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: copy },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
        if is_cf_hdrop_request(pformatetc) {
            windows::Win32::Foundation::S_OK
        } else {
            windows::Win32::Foundation::DV_E_FORMATETC
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> windows::core::HRESULT {
        unsafe {
            (*pformatetcout).ptd = std::ptr::null_mut();
        }
        windows::Win32::Foundation::E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: windows::Win32::Foundation::BOOL,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn EnumFormatEtc(
        &self,
        _dwdirection: u32,
    ) -> windows::core::Result<IEnumFORMATETC> {
        use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;

        let fmt = FORMATETC {
            cfFormat: CF_HDROP,
            ptd: std::ptr::null_mut(),
            dwAspect: 1, // DVASPECT_CONTENT
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };

        unsafe { SHCreateStdEnumFmtEtc(&[fmt]) }
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Option<&IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED,
        ))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED,
        ))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(windows::core::Error::from(
            windows::Win32::Foundation::OLE_E_ADVISENOTSUPPORTED,
        ))
    }
}
