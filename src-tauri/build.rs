use std::path::PathBuf;

fn main() {
    tauri_build::build();

    // Register custom cfg for conditional compilation
    println!("cargo::rustc-check-cfg=cfg(has_ffmpeg)");

    // Compile the C video thumbnail extractor and link FFmpeg
    build_video_thumb();
}

fn build_video_thumb() {
    let ffmpeg_dir = find_ffmpeg_dir();
    let Some(ffmpeg_dir) = ffmpeg_dir else {
        println!("cargo:warning=FFmpeg not found — video thumbnails disabled");
        return;
    };

    let include_dir = ffmpeg_dir.join("include");
    let lib_dir = ffmpeg_dir.join("lib");

    if !include_dir.exists() || !lib_dir.exists() {
        println!("cargo:warning=FFmpeg include/lib dirs not found at {:?}", ffmpeg_dir);
        return;
    }

    // Compile the C source
    cc::Build::new()
        .file("csrc/video_thumb.c")
        .include(&include_dir)
        .warnings(false)
        .compile("video_thumb");

    // Link FFmpeg libraries
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=avformat");
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-lib=dylib=avutil");
    println!("cargo:rustc-link-lib=dylib=swscale");

    // Tell Rust we have FFmpeg
    println!("cargo:rustc-cfg=has_ffmpeg");

    // Copy FFmpeg DLLs to the output directory so they're found at runtime
    let out_dir = std::env::var("OUT_DIR").unwrap();
    // OUT_DIR is something like target/debug/build/ufb-tauri-xxx/out
    // We need the target/debug or target/release directory
    let target_dir = PathBuf::from(&out_dir)
        .ancestors()
        .find(|p| p.ends_with("debug") || p.ends_with("release"))
        .map(|p| p.to_path_buf());

    if let Some(ref target_dir) = target_dir {
        let bin_dir = ffmpeg_dir.join("bin");
        if bin_dir.exists() {
            for entry in std::fs::read_dir(&bin_dir).unwrap() {
                let entry = entry.unwrap();
                let filename = entry.file_name();
                let name = filename.to_string_lossy();
                let is_lib = if cfg!(target_os = "windows") {
                    name.ends_with(".dll")
                } else if cfg!(target_os = "macos") {
                    name.contains(".dylib")
                } else {
                    name.contains(".so")
                };
                let is_executable = if cfg!(target_os = "windows") {
                    name == "ffmpeg.exe" || name == "ffprobe.exe"
                } else {
                    name == "ffmpeg" || name == "ffprobe"
                };
                if is_lib || is_executable {
                    let dest = target_dir.join(&filename);
                    if !dest.exists() {
                        let _ = std::fs::copy(entry.path(), &dest);
                    }
                }
            }
        }

        // Copy exiftool executable
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let exiftool_name = if cfg!(target_os = "windows") { "exiftool.exe" } else { "exiftool" };

        // Check local external/exiftool/ first, then UFB project's external/exiftool/
        let exiftool_candidates = vec![
            manifest_dir.join("external").join("exiftool").join(exiftool_name),
            manifest_dir.parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|github_dir| github_dir.join("UFB").join("external").join("exiftool").join(exiftool_name))
                .unwrap_or_default(),
        ];

        for src in exiftool_candidates {
            if src.exists() {
                let dest = target_dir.join(exiftool_name);
                if !dest.exists() {
                    let _ = std::fs::copy(&src, &dest);
                }
                // Also copy exiftool_files directory (Perl runtime + libs)
                let src_files_dir = src.parent().unwrap().join("exiftool_files");
                let dest_files_dir = target_dir.join("exiftool_files");
                if src_files_dir.is_dir() && !dest_files_dir.exists() {
                    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
                        std::fs::create_dir_all(dst)?;
                        for entry in std::fs::read_dir(src)? {
                            let entry = entry?;
                            let dest_path = dst.join(entry.file_name());
                            if entry.file_type()?.is_dir() {
                                copy_dir_recursive(&entry.path(), &dest_path)?;
                            } else {
                                std::fs::copy(entry.path(), dest_path)?;
                            }
                        }
                        Ok(())
                    }
                    let _ = copy_dir_recursive(&src_files_dir, &dest_files_dir);
                }
                break;
            }
        }

    }

    // Re-run if source changes
    println!("cargo:rerun-if-changed=csrc/video_thumb.c");
}

fn find_ffmpeg_dir() -> Option<PathBuf> {
    // 1. Check FFMPEG_DIR environment variable
    if let Ok(dir) = std::env::var("FFMPEG_DIR") {
        let path = PathBuf::from(dir);
        if path.exists() {
            return Some(path);
        }
    }

    // 2. Check local external directory
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("external").join("ffmpeg");
    if local.exists() {
        return Some(local);
    }

    // 3. Check the original UFB project's external directory (development convenience)
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // ufb-tauri/src-tauri -> ufb-tauri -> UnionFiles -> GitHub -> UFB/external/ffmpeg
    if let Some(github_dir) = manifest_dir.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
        let ufb_ffmpeg = github_dir.join("UFB").join("external").join("ffmpeg");
        if ufb_ffmpeg.exists() {
            return Some(ufb_ffmpeg);
        }
    }

    // 4. System FFmpeg via pkg-config (Linux)
    if let Ok(output) = std::process::Command::new("pkg-config")
        .args(["--variable=prefix", "libavformat"])
        .output()
    {
        if output.status.success() {
            let prefix = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
            if prefix.join("include").exists() {
                return Some(prefix);
            }
        }
    }

    None
}
