fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("../src-tauri/icons/icon.ico");
        res.compile().expect("Failed to compile Windows resources");

        // Copy icon.ico next to the agent binary for Quick Access shortcut use
        let icon_src = std::path::Path::new("../src-tauri/icons/icon.ico");
        if icon_src.exists() {
            if let Ok(out_dir) = std::env::var("OUT_DIR") {
                let target_dir = std::path::PathBuf::from(&out_dir)
                    .ancestors()
                    .find(|p| p.ends_with("debug") || p.ends_with("release"))
                    .map(|p| p.to_path_buf());
                if let Some(dir) = target_dir {
                    let _ = std::fs::copy(icon_src, dir.join("icon.ico"));
                }
            }
        }
    }
}
