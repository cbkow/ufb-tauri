/// OS-native file type icon extraction with in-process cache.
///
/// Windows: SHGetFileInfoW → HICON → BGRA bitmap → PNG
/// macOS: NSWorkspace.icon(forFileType:) → PNG (TODO)
/// Linux: returns None (Material Symbols fallback)

use std::collections::HashMap;
use std::sync::RwLock;

/// Maps a requested pixel size to Windows' nearest native icon bucket.
/// Windows only stores icons at a fixed set of sizes — requesting 100px
/// rounds up to the 256px SHIL_JUMBO. Keeping the cache bucketed avoids
/// storing near-duplicate entries for every nuanced pixel request.
fn size_bucket(size: u32) -> u32 {
    match size {
        0..=16 => 16,
        17..=32 => 32,
        33..=48 => 48,
        _ => 256,
    }
}

/// Cached system icon provider. Thread-safe, keyed by lowercase extension.
pub struct SystemIconCache {
    cache: RwLock<HashMap<String, Option<Vec<u8>>>>,
}

impl SystemIconCache {
    pub fn new() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get the system icon for a file extension as PNG bytes.
    /// Returns cached result if available, otherwise extracts from OS.
    ///
    /// Cache key includes a size bucket — a request for a 256px icon and
    /// one for a 32px icon on the same extension are independent entries,
    /// so the first small request doesn't pollute the cache and starve the
    /// grid view of a high-res icon.
    pub fn get_icon(&self, extension: &str, size: u32) -> Result<Option<Vec<u8>>, String> {
        let bucket = size_bucket(size);
        let key = format!("{}:{}", extension.to_lowercase(), bucket);

        // Check cache
        if let Some(cached) = self.cache.read().unwrap().get(&key) {
            return Ok(cached.clone());
        }

        // Extract from OS
        let result = Self::extract_icon(extension, size);
        let png = match result {
            Ok(png) => png,
            Err(e) => {
                log::debug!("Failed to extract icon for .{}: {}", key, e);
                None
            }
        };

        // Cache (including None — means "no icon for this extension at this size")
        self.cache.write().unwrap().insert(key, png.clone());
        Ok(png)
    }

    #[cfg(windows)]
    fn extract_icon(extension: &str, size: u32) -> Result<Option<Vec<u8>>, String> {
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
        use windows::Win32::UI::Shell::{
            SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_SYSICONINDEX,
            SHGFI_USEFILEATTRIBUTES, SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL,
        };
        use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo};
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};

        // Pick the closest SHIL_* bucket Windows offers. SHIL_JUMBO is 256×256
        // on Vista+ — the one we actually want for grid thumbnails. The
        // previous implementation used SHGFI_LARGEICON (32×32), which is why
        // grid thumbnails looked tiny when scaled up.
        let shil = match size {
            0..=16 => SHIL_SMALL,        // 16×16
            17..=32 => SHIL_LARGE,       // 32×32
            33..=48 => SHIL_EXTRALARGE,  // 48×48
            _ => SHIL_JUMBO,             // 256×256
        } as i32;

        // Folders: use FILE_ATTRIBUTE_DIRECTORY with a bare path
        let is_folder = extension == "folder";
        let fake_path: Vec<u16> = if is_folder {
            "C:\\fake\0".encode_utf16().collect()
        } else {
            format!("C:\\fake.{}\0", extension).encode_utf16().collect()
        };
        let file_attrs = if is_folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };

        // Step 1: get the system icon index for this extension/folder.
        let mut sfi = SHFILEINFOW::default();
        let flags = SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES;
        let result = unsafe {
            SHGetFileInfoW(
                PCWSTR(fake_path.as_ptr()),
                file_attrs,
                Some(&mut sfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };
        if result == 0 {
            return Ok(None);
        }

        // Step 2: get the system image list at the requested resolution and
        // extract the HICON from it.
        let image_list: IImageList = match unsafe { SHGetImageList(shil) } {
            Ok(l) => l,
            Err(_) => return Ok(None),
        };
        let hicon = match unsafe { image_list.GetIcon(sfi.iIcon, ILD_TRANSPARENT.0 as u32) } {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };
        if hicon.is_invalid() {
            return Ok(None);
        }

        // Get icon bitmap info
        let mut icon_info = windows::Win32::UI::WindowsAndMessaging::ICONINFO::default();
        if unsafe { GetIconInfo(hicon, &mut icon_info) }.is_err() {
            unsafe { DestroyIcon(hicon).ok() };
            return Ok(None);
        }

        let hbm_color = icon_info.hbmColor;
        if hbm_color.is_invalid() {
            if !icon_info.hbmMask.is_invalid() {
                unsafe { DeleteObject(icon_info.hbmMask) };
            }
            unsafe { DestroyIcon(hicon).ok() };
            return Ok(None);
        }

        // Get bitmap dimensions
        let mut bmp = windows::Win32::Graphics::Gdi::BITMAP::default();
        let got = unsafe {
            windows::Win32::Graphics::Gdi::GetObjectW(
                hbm_color,
                std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAP>() as i32,
                Some(&mut bmp as *mut _ as *mut _),
            )
        };
        if got == 0 {
            unsafe {
                let _ = DeleteObject(hbm_color);
                if !icon_info.hbmMask.is_invalid() { let _ = DeleteObject(icon_info.hbmMask); }
                DestroyIcon(hicon).ok();
            }
            return Ok(None);
        }

        let width = bmp.bmWidth as u32;
        let height = bmp.bmHeight as u32;

        // Extract BGRA pixel data via GetDIBits
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let hdc = unsafe { CreateCompatibleDC(None) };
        let old = unsafe { SelectObject(hdc, hbm_color) };

        let lines = unsafe {
            GetDIBits(
                hdc,
                hbm_color,
                0,
                height,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            )
        };

        unsafe {
            SelectObject(hdc, old);
            DeleteDC(hdc);
            let _ = DeleteObject(hbm_color);
            if !icon_info.hbmMask.is_invalid() { let _ = DeleteObject(icon_info.hbmMask); }
            DestroyIcon(hicon).ok();
        }

        if lines == 0 {
            return Ok(None);
        }

        // Convert BGRA → RGBA
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2); // B ↔ R
        }

        // Encode as PNG via the image crate
        let img = image::RgbaImage::from_raw(width, height, pixels)
            .ok_or("Failed to create image from pixel data")?;

        let mut png_buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png_buf, image::ImageFormat::Png)
            .map_err(|e| format!("PNG encode failed: {}", e))?;

        Ok(Some(png_buf.into_inner()))
    }

    #[cfg(target_os = "macos")]
    fn extract_icon(extension: &str, size: u32) -> Result<Option<Vec<u8>>, String> {
        use objc2::rc::Retained;
        use objc2_foundation::NSString;
        use objc2_app_kit::{NSWorkspace, NSImage, NSBitmapImageRep};
        use objc2::msg_send;

        let workspace = unsafe { NSWorkspace::sharedWorkspace() };

        // Get the system icon for this file type
        // iconForFileType: accepts extensions ("pdf") or UTI strings ("public.folder")
        let type_str = if extension == "folder" || extension.is_empty() {
            NSString::from_str("public.folder")
        } else {
            NSString::from_str(extension)
        };

        let icon: Retained<NSImage> = unsafe {
            msg_send![&workspace, iconForFileType: &*type_str]
        };

        // Set the desired size
        let sz = size as f64;
        unsafe {
            let ns_size = objc2_foundation::NSSize::new(sz, sz);
            icon.setSize(ns_size);
        }

        // Convert NSImage to PNG via NSBitmapImageRep
        let png_data: Option<Vec<u8>> = unsafe {
            // Lock focus and draw into a bitmap
            let tiff_data = icon.TIFFRepresentation();
            let tiff = match tiff_data {
                Some(d) => d,
                None => return Ok(None),
            };

            let bitmap = NSBitmapImageRep::imageRepWithData(&tiff);
            let bitmap = match bitmap {
                Some(b) => b,
                None => return Ok(None),
            };

            // Get PNG representation
            let png: Option<Retained<objc2_foundation::NSData>> = msg_send![
                &bitmap,
                representationUsingType: 4u64, // NSBitmapImageRepFileTypePNG = 4
                properties: std::ptr::null::<objc2_foundation::NSDictionary>() as *const _
            ];

            match png {
                Some(data) => {
                    let len: usize = msg_send![&data, length];
                    let ptr: *const u8 = msg_send![&data, bytes];
                    if ptr.is_null() || len == 0 {
                        None
                    } else {
                        Some(std::slice::from_raw_parts(ptr, len).to_vec())
                    }
                }
                None => None,
            }
        };

        Ok(png_data)
    }

    #[cfg(target_os = "linux")]
    fn extract_icon(_extension: &str, _size: u32) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
}
