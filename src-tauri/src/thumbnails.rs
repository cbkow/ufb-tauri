use crate::db::Database;
use image::imageops::FilterType;
use image::GenericImageView;
use rusqlite::OptionalExtension;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Semaphore;

/// Max thumbnail dimension (always square-fit).
const THUMB_SIZE: u32 = 512;

/// Max concurrent thumbnail extractions (matches old app's 12 worker threads).
const MAX_CONCURRENT: usize = 12;

// ── Extension categories ──

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tiff", "tif", "webp", "ico", "avif",
];

const SVG_EXTENSIONS: &[&str] = &["svg", "svgz"];

const PSD_EXTENSIONS: &[&str] = &["psd", "psb"];

const EXR_EXTENSIONS: &[&str] = &["exr"];

const BLEND_EXTENSIONS: &[&str] = &["blend"];

const PDF_EXTENSIONS: &[&str] = &["pdf", "ai", "eps"];

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "avi", "mkv", "wmv", "flv", "webm", "m4v", "mpg", "mpeg", "3gp", "mxf",
    "mts", "m2ts",
];

fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&ext)
}
fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext)
}
fn is_svg_ext(ext: &str) -> bool {
    SVG_EXTENSIONS.contains(&ext)
}
fn is_psd_ext(ext: &str) -> bool {
    PSD_EXTENSIONS.contains(&ext)
}
fn is_exr_ext(ext: &str) -> bool {
    EXR_EXTENSIONS.contains(&ext)
}
fn is_blend_ext(ext: &str) -> bool {
    BLEND_EXTENSIONS.contains(&ext)
}
fn is_pdf_ext(ext: &str) -> bool {
    PDF_EXTENSIONS.contains(&ext)
}

// ── Helpers ──

fn path_hash(path: &str) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_lowercase().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Encode RGBA pixels to PNG thumbnail bytes, resizing if needed.
fn rgba_to_png_thumb(rgba: Vec<u8>, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "failed to create image from pixels".to_string())?;

    let thumb = if width <= THUMB_SIZE && height <= THUMB_SIZE {
        image::DynamicImage::ImageRgba8(img)
    } else {
        image::DynamicImage::ImageRgba8(img).resize(THUMB_SIZE, THUMB_SIZE, FilterType::Triangle)
    };

    let mut buf = Cursor::new(Vec::new());
    thumb
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {}", e))?;

    Ok(buf.into_inner())
}

// ── ThumbnailManager ──

pub struct ThumbnailManager {
    db: Arc<Database>,
    semaphore: Arc<Semaphore>,
    in_flight: Arc<Mutex<HashSet<String>>>,
}

impl ThumbnailManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn ensure_unique_index(db: &Database) -> Result<(), String> {
        db.with_conn(|conn| {
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_thumbnail_hash_unique
                 ON thumbnail_cache(file_path_hash);",
            )?;
            Ok(())
        })
        .map_err(|e| format!("index creation error: {}", e))
    }

    pub async fn get_or_generate_async(
        &self,
        file_path: String,
    ) -> Result<Option<Vec<u8>>, String> {
        let ext = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext.is_empty() {
            return Ok(None);
        }

        // In-flight dedup
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if in_flight.contains(&file_path) {
                return Ok(None);
            }
            in_flight.insert(file_path.clone());
        }

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| "semaphore closed".to_string())?;

        let db = Arc::clone(&self.db);
        let in_flight = Arc::clone(&self.in_flight);
        let path = file_path.clone();

        let result = tokio::task::spawn_blocking(move || {
            let result = get_or_generate_sync(&db, &path);
            in_flight.lock().unwrap().remove(&path);
            result
        })
        .await
        .map_err(|e| format!("task join error: {}", e))?;

        result
    }
}

// ── Sync generation ──

fn get_or_generate_sync(db: &Database, file_path: &str) -> Result<Option<Vec<u8>>, String> {
    let path = Path::new(file_path);
    if !path.exists() || path.is_dir() {
        return Ok(None);
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext.is_empty() {
        return Ok(None);
    }

    let hash = path_hash(file_path);

    let meta = fs::metadata(path).map_err(|e| format!("metadata error: {}", e))?;
    let file_size = meta.len() as i64;
    let file_mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    if let Some(cached) = check_cache(db, &hash, file_size, file_mtime)? {
        return Ok(Some(cached));
    }

    let png_data = try_extract(file_path, &ext)?;

    match png_data {
        Some(data) => {
            if let Err(e) = store_cache(db, &hash, file_path, file_size, file_mtime, &data) {
                log::warn!("Thumbnail cache store failed: {}", e);
            }
            Ok(Some(data))
        }
        None => Ok(None),
    }
}

/// Try each extractor in priority order until one succeeds.
fn try_extract(file_path: &str, ext: &str) -> Result<Option<Vec<u8>>, String> {
    // 1. image crate — basic image formats (fast, no external deps)
    if is_image_ext(ext) {
        match extract_image(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("image extractor failed for {}: {}", file_path, e),
        }
    }

    // 2. resvg — SVG files
    if is_svg_ext(ext) {
        match extract_svg(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("svg extractor failed for {}: {}", file_path, e),
        }
    }

    // 3. psd crate — Photoshop PSD/PSB
    if is_psd_ext(ext) {
        match extract_psd(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("psd extractor failed for {}: {}", file_path, e),
        }
    }

    // 4. EXR — OpenEXR HDR images (pure Rust)
    if is_exr_ext(ext) {
        match extract_exr(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("exr extractor failed for {}: {}", file_path, e),
        }
    }

    // 5. Blender — embedded thumbnail from .blend binary
    if is_blend_ext(ext) {
        match extract_blend(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("blend extractor failed for {}: {}", file_path, e),
        }
    }

    // 6. PDF/AI/EPS — pdfium renderer
    if is_pdf_ext(ext) {
        match extract_pdf(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("pdf extractor failed for {}: {}", file_path, e),
        }
    }

    // 7. FFmpeg — video formats
    if is_video_ext(ext) {
        match extract_video_ffmpeg(file_path) {
            Ok(data) => return Ok(Some(data)),
            Err(e) => log::debug!("ffmpeg extractor failed for {}: {}", file_path, e),
        }
    }

    // 8. Windows Shell — catch-all (Office docs, RAW camera, etc.)
    #[cfg(target_os = "windows")]
    {
        let handled = is_video_ext(ext)
            || is_image_ext(ext)
            || is_svg_ext(ext)
            || is_psd_ext(ext)
            || is_exr_ext(ext)
            || is_blend_ext(ext)
            || is_pdf_ext(ext);
        if !handled {
            match extract_windows_shell(file_path) {
                Ok(data) => return Ok(Some(data)),
                Err(e) => {
                    log::debug!("windows shell extractor failed for {}: {}", file_path, e)
                }
            }
        }
    }

    Ok(None)
}

// ── Cache operations ──

fn check_cache(
    db: &Database,
    hash: &str,
    file_size: i64,
    file_mtime: i64,
) -> Result<Option<Vec<u8>>, String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare_cached(
            "SELECT thumbnail_data FROM thumbnail_cache
             WHERE file_path_hash = ?1 AND file_size = ?2 AND file_mtime = ?3
             AND thumbnail_data IS NOT NULL",
        )?;

        let result = stmt
            .query_row(
                rusqlite::params![hash, file_size, file_mtime],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional();

        match result {
            Ok(data) => {
                if data.is_some() {
                    let _ = conn.execute(
                        "UPDATE thumbnail_cache
                         SET last_access_time = ?1, access_count = access_count + 1
                         WHERE file_path_hash = ?2",
                        rusqlite::params![now_epoch(), hash],
                    );
                }
                Ok(data)
            }
            Err(e) => Err(e),
        }
    })
    .map_err(|e| format!("cache check error: {}", e))
}

fn store_cache(
    db: &Database,
    hash: &str,
    file_path: &str,
    file_size: i64,
    file_mtime: i64,
    png_data: &[u8],
) -> Result<(), String> {
    let now = now_epoch();
    db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM thumbnail_cache WHERE file_path_hash = ?1",
            rusqlite::params![hash],
        )?;
        conn.execute(
            "INSERT INTO thumbnail_cache
                (file_path_hash, file_path, file_size, file_mtime,
                 thumbnail_width, thumbnail_height, thumbnail_data,
                 data_size, extracted_time, last_access_time, access_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?8, 1)",
            rusqlite::params![
                hash,
                file_path,
                file_size,
                file_mtime,
                THUMB_SIZE as i64,
                png_data,
                png_data.len() as i64,
                now,
            ],
        )?;
        Ok(())
    })
    .map_err(|e| format!("cache store error: {}", e))
}

// ══════════════════════════════════════════════════════════════════════
// Extractors
// ══════════════════════════════════════════════════════════════════════

// ── 1. image crate (JPEG, PNG, GIF, BMP, TIFF, WebP, ICO, AVIF) ──

fn extract_image(file_path: &str) -> Result<Vec<u8>, String> {
    let img = image::open(file_path).map_err(|e| format!("image open: {}", e))?;

    let (w, h) = img.dimensions();
    let thumb = if w <= THUMB_SIZE && h <= THUMB_SIZE {
        img
    } else {
        img.resize(THUMB_SIZE, THUMB_SIZE, FilterType::Triangle)
    };

    let mut buf = Cursor::new(Vec::new());
    thumb
        .write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("png encode: {}", e))?;

    Ok(buf.into_inner())
}

// ── 2. resvg (SVG) ──

fn extract_svg(file_path: &str) -> Result<Vec<u8>, String> {
    let svg_data = fs::read(file_path).map_err(|e| format!("svg read: {}", e))?;

    let tree = resvg::usvg::Tree::from_data(&svg_data, &resvg::usvg::Options::default())
        .map_err(|e| format!("svg parse: {}", e))?;

    let svg_size = tree.size();
    let (sw, sh) = (svg_size.width(), svg_size.height());

    let scale = (THUMB_SIZE as f32 / sw.max(sh)).min(1.0);
    let pw = (sw * scale).round() as u32;
    let ph = (sh * scale).round() as u32;

    if pw == 0 || ph == 0 {
        return Err("zero dimension svg".to_string());
    }

    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph)
        .ok_or_else(|| "failed to create pixmap".to_string())?;

    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    pixmap
        .encode_png()
        .map_err(|e| format!("svg png encode: {}", e))
}

// ── 3. psd crate (PSD/PSB) ──

fn extract_psd(file_path: &str) -> Result<Vec<u8>, String> {
    let file_data = fs::read(file_path).map_err(|e| format!("psd read: {}", e))?;

    if file_data.len() > 500 * 1024 * 1024 {
        return Err("psd file too large (>500MB)".to_string());
    }

    let psd = psd::Psd::from_bytes(&file_data).map_err(|e| format!("psd parse: {}", e))?;
    let (w, h) = (psd.width(), psd.height());

    if w == 0 || h == 0 {
        return Err("zero dimension psd".to_string());
    }

    rgba_to_png_thumb(psd.rgba(), w, h)
}

// ── 4. OpenEXR (pure Rust, ported from C++ extractor) ──

fn extract_exr(file_path: &str) -> Result<Vec<u8>, String> {
    use exr::prelude::*;

    // Read the EXR file — read only the first layer, first level
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(file_path)
        .map_err(|e| format!("exr open: {}", e))?;

    let layer = &image.layer_data;
    let size = layer.size;
    let full_width = size.x();
    let full_height = size.y();

    if full_width == 0 || full_height == 0 {
        return Err("zero dimension exr".to_string());
    }

    // Downsampling: skip every Nth pixel (matches old C++ extractor)
    let max_dim = full_width.max(full_height);
    let skip = (max_dim / THUMB_SIZE as usize).max(1);
    let thumb_w = full_width / skip;
    let thumb_h = full_height / skip;

    if thumb_w == 0 || thumb_h == 0 {
        return Err("exr too small after downsampling".to_string());
    }

    // Find R, G, B channels — try flat first, then layered (Blender style)
    let channels = &layer.channel_data.list;

    // Convert channel names to strings once for searching
    let ch_names: Vec<String> = channels.iter().map(|c| c.name.to_string()).collect();

    let find_channel = |names: &[&str]| -> Option<usize> {
        for name in names {
            if let Some(idx) = ch_names.iter().position(|n| n == name) {
                return Some(idx);
            }
        }
        None
    };

    // Try flat R/G/B first
    let r_idx = find_channel(&["R", "r"]);
    let g_idx = find_channel(&["G", "g"]);
    let b_idx = find_channel(&["B", "b"]);
    let a_idx = find_channel(&["A", "a"]);

    // If flat channels not found, search for layered (e.g., ViewLayer.Combined.R)
    let (r_idx, g_idx, b_idx, a_idx) = if r_idx.is_some() && g_idx.is_some() && b_idx.is_some() {
        (r_idx.unwrap(), g_idx.unwrap(), b_idx.unwrap(), a_idx)
    } else {
        // Find first layer prefix that has R, G, B
        let mut found = None;
        for (i, name) in ch_names.iter().enumerate() {
            if let Some(dot_pos) = name.rfind('.') {
                let prefix = &name[..dot_pos];
                let suffix = &name[dot_pos + 1..];
                if suffix.eq_ignore_ascii_case("r") {
                    let g_name_upper = format!("{}.G", prefix);
                    let g_name_lower = format!("{}.g", prefix);
                    let b_name_upper = format!("{}.B", prefix);
                    let b_name_lower = format!("{}.b", prefix);
                    let a_name_upper = format!("{}.A", prefix);
                    let a_name_lower = format!("{}.a", prefix);

                    let g = ch_names
                        .iter()
                        .position(|n| n == &g_name_upper || n == &g_name_lower);
                    let b = ch_names
                        .iter()
                        .position(|n| n == &b_name_upper || n == &b_name_lower);
                    let a = ch_names
                        .iter()
                        .position(|n| n == &a_name_upper || n == &a_name_lower);

                    if let (Some(gi), Some(bi)) = (g, b) {
                        found = Some((i, gi, bi, a));
                        break;
                    }
                }
            }
        }
        match found {
            Some((r, g, b, a)) => (r, g, b, a),
            None => return Err("no RGB channels found in EXR".to_string()),
        }
    };

    // Extract pixels with downsampling
    let mut rgba = vec![0u8; thumb_w * thumb_h * 4];

    let get_pixel_f32 = |ch_idx: usize, x: usize, y: usize| -> f32 {
        let sample = &channels[ch_idx].sample_data;
        let idx = y * full_width + x;
        match sample {
            FlatSamples::F16(data) => data[idx].to_f32(),
            FlatSamples::F32(data) => data[idx],
            FlatSamples::U32(data) => data[idx] as f32 / u32::MAX as f32,
        }
    };

    for ty in 0..thumb_h {
        let sy = ty * skip;
        if sy >= full_height {
            break;
        }
        for tx in 0..thumb_w {
            let sx = tx * skip;
            if sx >= full_width {
                break;
            }

            let r = get_pixel_f32(r_idx, sx, sy).clamp(0.0, 1.0);
            let g = get_pixel_f32(g_idx, sx, sy).clamp(0.0, 1.0);
            let b = get_pixel_f32(b_idx, sx, sy).clamp(0.0, 1.0);
            let a = a_idx
                .map(|ai| get_pixel_f32(ai, sx, sy).clamp(0.0, 1.0))
                .unwrap_or(1.0);

            let dst = (ty * thumb_w + tx) * 4;
            rgba[dst] = (r * 255.0) as u8;
            rgba[dst + 1] = (g * 255.0) as u8;
            rgba[dst + 2] = (b * 255.0) as u8;
            rgba[dst + 3] = (a * 255.0) as u8;
        }
    }

    rgba_to_png_thumb(rgba, thumb_w as u32, thumb_h as u32)
}

// ── 5. Blender (.blend) — embedded thumbnail from binary ──

fn extract_blend(file_path: &str) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(file_path).map_err(|e| format!("blend open: {}", e))?;

    // Read 12-byte header
    let mut header = [0u8; 12];
    file.read_exact(&mut header)
        .map_err(|e| format!("blend header: {}", e))?;

    if &header[..7] != b"BLENDER" {
        return Err("not a .blend file".to_string());
    }

    let ptr_size: u64 = if header[7] == b'_' { 4 } else { 8 };
    let little_endian = header[8] == b'v';

    let read_i32 = |f: &mut fs::File| -> Result<i32, String> {
        let mut buf = [0u8; 4];
        f.read_exact(&mut buf)
            .map_err(|e| format!("blend read: {}", e))?;
        Ok(if little_endian {
            i32::from_le_bytes(buf)
        } else {
            i32::from_be_bytes(buf)
        })
    };

    let read_u32 = |f: &mut fs::File| -> Result<u32, String> {
        let mut buf = [0u8; 4];
        f.read_exact(&mut buf)
            .map_err(|e| format!("blend read: {}", e))?;
        Ok(if little_endian {
            u32::from_le_bytes(buf)
        } else {
            u32::from_be_bytes(buf)
        })
    };

    // Scan file blocks for TEST (thumbnail)
    loop {
        let mut code = [0u8; 4];
        if file.read_exact(&mut code).is_err() {
            break;
        }

        let block_size = read_i32(&mut file)?;

        if block_size < 0 || block_size > 100 * 1024 * 1024 {
            return Err("invalid block size".to_string());
        }

        // Skip: old memory address
        file.seek(SeekFrom::Current(ptr_size as i64))
            .map_err(|e| format!("blend seek: {}", e))?;
        // Skip: SDNA index + count
        let _sdna = read_i32(&mut file)?;
        let _count = read_i32(&mut file)?;

        if &code == b"TEST" {
            let width = read_u32(&mut file)?;
            let height = read_u32(&mut file)?;

            if width == 0 || height == 0 || width > 1024 || height > 1024 {
                return Err("invalid blend thumbnail dimensions".to_string());
            }

            let data_size = (width * height * 4) as usize;
            let mut rgba = vec![0u8; data_size];
            file.read_exact(&mut rgba)
                .map_err(|e| format!("blend thumb data: {}", e))?;

            // Blender stores bottom-up RGBA — flip vertically
            let stride = (width * 4) as usize;
            let mut flipped = vec![0u8; data_size];
            for y in 0..height as usize {
                let src_row = (height as usize - 1 - y) * stride;
                let dst_row = y * stride;
                flipped[dst_row..dst_row + stride]
                    .copy_from_slice(&rgba[src_row..src_row + stride]);
            }

            return rgba_to_png_thumb(flipped, width, height);
        } else if &code == b"ENDB" {
            break;
        } else {
            // Skip block data
            file.seek(SeekFrom::Current(block_size as i64))
                .map_err(|e| format!("blend skip: {}", e))?;
        }
    }

    Err("no TEST block found in .blend file".to_string())
}

// ── 6. PDF/AI/EPS — pdfium renderer ──

fn extract_pdf(file_path: &str) -> Result<Vec<u8>, String> {
    // AI files with PDF compatibility start with %PDF — read and check
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "ai" || ext == "eps" {
        // Check if file contains embedded PDF
        let mut header = [0u8; 1024];
        let mut f =
            fs::File::open(file_path).map_err(|e| format!("pdf open for header check: {}", e))?;
        use std::io::Read;
        let n = f
            .read(&mut header)
            .map_err(|e| format!("pdf header read: {}", e))?;
        let header_str = String::from_utf8_lossy(&header[..n]);
        if !header_str.contains("%PDF") {
            return Err(format!("{} file has no embedded PDF", ext.to_uppercase()));
        }
    }

    // Try to find pdfium library
    let pdfium = pdfium_render::prelude::Pdfium::new(
        pdfium_render::prelude::Pdfium::bind_to_system_library()
            .map_err(|e| format!("pdfium bind: {}", e))?,
    );

    let doc = pdfium
        .load_pdf_from_file(file_path, None)
        .map_err(|e| format!("pdf load: {}", e))?;

    let page = doc
        .pages()
        .first()
        .map_err(|e| format!("pdf first page: {}", e))?;

    // Calculate render size maintaining aspect ratio
    let pw = page.width().value;
    let ph = page.height().value;
    let scale = (THUMB_SIZE as f32 / pw.max(ph)).min(2.0); // allow slight upscale for tiny PDFs
    let render_w = (pw * scale) as u32;
    let render_h = (ph * scale) as u32;

    let bitmap = page
        .render_with_config(
            &pdfium_render::prelude::PdfRenderConfig::new()
                .set_target_width(render_w as i32)
                .set_target_height(render_h as i32),
        )
        .map_err(|e| format!("pdf render: {}", e))?;

    let img = bitmap.as_image();
    let rgba_img = img.to_rgba8();
    let (w, h) = rgba_img.dimensions();

    rgba_to_png_thumb(rgba_img.into_raw(), w, h)
}

// ── 7. FFmpeg C library (video formats) ──

#[cfg(has_ffmpeg)]
mod ffmpeg_ffi {
    use std::os::raw::{c_char, c_int, c_uchar};

    extern "C" {
        pub fn extract_video_frame(
            path: *const c_char,
            max_size: c_int,
            out_data: *mut *mut c_uchar,
            out_width: *mut c_int,
            out_height: *mut c_int,
        ) -> c_int;

        pub fn free_frame_data(data: *mut c_uchar);
    }
}

#[cfg(has_ffmpeg)]
fn extract_video_ffmpeg(file_path: &str) -> Result<Vec<u8>, String> {
    use std::ffi::CString;

    let c_path = CString::new(file_path).map_err(|_| "invalid path".to_string())?;

    let mut data_ptr: *mut u8 = std::ptr::null_mut();
    let mut width: i32 = 0;
    let mut height: i32 = 0;

    let ret = unsafe {
        ffmpeg_ffi::extract_video_frame(
            c_path.as_ptr(),
            THUMB_SIZE as i32,
            &mut data_ptr,
            &mut width,
            &mut height,
        )
    };

    if ret != 0 || data_ptr.is_null() || width <= 0 || height <= 0 {
        return Err("ffmpeg frame extraction failed".to_string());
    }

    let pixel_count = (width * height * 4) as usize;
    let rgba_data = unsafe {
        let slice = std::slice::from_raw_parts(data_ptr, pixel_count);
        let vec = slice.to_vec();
        ffmpeg_ffi::free_frame_data(data_ptr);
        vec
    };

    rgba_to_png_thumb(rgba_data, width as u32, height as u32)
}

#[cfg(not(has_ffmpeg))]
fn extract_video_ffmpeg(_file_path: &str) -> Result<Vec<u8>, String> {
    Err("FFmpeg not available".to_string())
}

// ── 8. Windows Shell (IShellItemImageFactory — catch-all) ──

#[cfg(target_os = "windows")]
fn extract_windows_shell(file_path: &str) -> Result<Vec<u8>, String> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::Foundation::SIZE;
    #[allow(unused_imports)]
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
        SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let wide: Vec<u16> = OsStr::new(file_path)
            .encode_wide()
            .chain(once(0))
            .collect();

        let shell_item: windows::Win32::UI::Shell::IShellItem =
            SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None)
                .map_err(|e| format!("SHCreateItemFromParsingName: {}", e))?;

        let factory: IShellItemImageFactory = shell_item
            .cast()
            .map_err(|e| format!("cast to IShellItemImageFactory: {}", e))?;

        let thumb_size = SIZE {
            cx: THUMB_SIZE as i32,
            cy: THUMB_SIZE as i32,
        };

        let hbitmap = factory
            .GetImage(
                thumb_size,
                SIIGBF_THUMBNAILONLY | SIIGBF_RESIZETOFIT | SIIGBF_BIGGERSIZEOK,
            )
            .map_err(|e| format!("GetImage: {}", e))?;

        hbitmap_to_png(hbitmap)
    }
}

#[cfg(target_os = "windows")]
unsafe fn hbitmap_to_png(
    hbitmap: windows::Win32::Graphics::Gdi::HBITMAP,
) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Gdi::*;

    let mut bmp = BITMAP::default();
    let ret = GetObjectW(
        hbitmap,
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bmp as *mut _ as *mut _),
    );
    if ret == 0 {
        let _ = DeleteObject(hbitmap);
        return Err("GetObject failed".to_string());
    }

    let width = bmp.bmWidth as u32;
    let height = bmp.bmHeight.unsigned_abs();

    if width == 0 || height == 0 {
        let _ = DeleteObject(hbitmap);
        return Err("zero dimension bitmap".to_string());
    }

    let hdc = CreateCompatibleDC(None);
    let old = SelectObject(hdc, hbitmap);

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0,
            ..Default::default()
        },
        ..Default::default()
    };

    let stride = (width * 4) as usize;
    let mut bits = vec![0u8; stride * height as usize];

    let lines = GetDIBits(
        hdc,
        hbitmap,
        0,
        height,
        Some(bits.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    SelectObject(hdc, old);
    let _ = DeleteDC(hdc);
    let _ = DeleteObject(hbitmap);

    if lines == 0 {
        return Err("GetDIBits failed".to_string());
    }

    // BGRA → RGBA
    for i in (0..bits.len()).step_by(4) {
        bits.swap(i, i + 2);
    }

    rgba_to_png_thumb(bits, width, height)
}
