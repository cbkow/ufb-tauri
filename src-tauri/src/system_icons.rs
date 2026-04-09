/// OS-native file type icon extraction with in-process cache.
///
/// Windows: SHGetFileInfoW → HICON → BGRA bitmap → PNG
/// macOS: NSWorkspace.icon(forFileType:) → PNG (TODO)
/// Linux: returns None (Material Symbols fallback)

use std::collections::HashMap;
use std::sync::RwLock;

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
    pub fn get_icon(&self, extension: &str, size: u32) -> Result<Option<Vec<u8>>, String> {
        let key = extension.to_lowercase();

        // Check cache
        if let Some(cached) = self.cache.read().unwrap().get(&key) {
            return Ok(cached.clone());
        }

        // Extract from OS
        let result = Self::extract_icon(&key, size);
        let png = match result {
            Ok(png) => png,
            Err(e) => {
                log::debug!("Failed to extract icon for .{}: {}", key, e);
                None
            }
        };

        // Cache (including None — means "no icon for this extension")
        self.cache.write().unwrap().insert(key, png.clone());
        Ok(png)
    }

    #[cfg(windows)]
    fn extract_icon(extension: &str, _size: u32) -> Result<Option<Vec<u8>>, String> {
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        };
        use windows::Win32::UI::Shell::{
            SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
        };
        use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo};
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};

        // Folders: use FILE_ATTRIBUTE_DIRECTORY with a bare path
        let is_folder = extension == "folder";
        let fake_path: Vec<u16> = if is_folder {
            "C:\\fake\0".encode_utf16().collect()
        } else {
            format!("C:\\fake.{}\0", extension).encode_utf16().collect()
        };
        let file_attrs = if is_folder { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };

        let mut sfi = SHFILEINFOW::default();
        let flags = SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES;

        let result = unsafe {
            SHGetFileInfoW(
                PCWSTR(fake_path.as_ptr()),
                file_attrs,
                Some(&mut sfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };

        if result == 0 || sfi.hIcon.is_invalid() {
            return Ok(None);
        }

        let hicon = sfi.hIcon;

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
    fn extract_icon(_extension: &str, _size: u32) -> Result<Option<Vec<u8>>, String> {
        // TODO: NSWorkspace.icon(forFileType:) → NSImage → PNG
        Ok(None)
    }

    #[cfg(target_os = "linux")]
    fn extract_icon(_extension: &str, _size: u32) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }
}
