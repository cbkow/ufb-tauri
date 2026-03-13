/** File type icon mapping — returns a Material Symbols icon name and color. */

export interface FileIcon {
  icon: string;   // Material Symbols icon name
  color: string;
}

const FOLDER_ICON: FileIcon = { icon: "folder", color: "#dcb67a" };
const UNKNOWN_ICON: FileIcon = { icon: "description", color: "#9d9d9d" };

const extensionMap: Record<string, FileIcon> = {};

function register(exts: string[], icon: FileIcon) {
  for (const ext of exts) extensionMap[ext] = icon;
}

// Images
register(
  ["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "ico", "webp", "avif",
   "heic", "heif", "tif", "tiff", "svg", "jxl", "jp2", "j2k"],
  { icon: "image", color: "#a478e8" }
);
register(["exr", "hdr"], { icon: "hdr_on", color: "#e5c07b" });
register(["psd", "psb"], { icon: "palette", color: "#31a8ff" });
register(["ai", "eps"], { icon: "palette", color: "#ff9a00" });
register(
  ["raw", "cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2"],
  { icon: "camera_roll", color: "#e5c07b" }
);

// Video
register(
  ["mp4", "mov", "avi", "mkv", "wmv", "flv", "webm", "m4v", "mpg", "mpeg",
   "3gp", "mxf", "mts", "m2ts", "ts", "vob"],
  { icon: "movie", color: "#e06c75" }
);

// Audio
register(
  ["mp3", "wav", "flac", "aac", "ogg", "wma", "m4a", "aiff", "aif", "opus", "mid", "midi"],
  { icon: "music_note", color: "#e5c07b" }
);

// Documents
register(["doc", "docx", "rtf", "odt"], { icon: "article", color: "#4fadf5" });
register(["txt", "md"], { icon: "text_snippet", color: "#9d9d9d" });
register(["xls", "xlsx", "ods", "csv", "tsv"], { icon: "table_chart", color: "#98c379" });
register(["ppt", "pptx", "odp", "key"], { icon: "slideshow", color: "#e5874b" });
register(["pdf"], { icon: "picture_as_pdf", color: "#e06c75" });

// Code
register(
  ["js", "ts", "jsx", "tsx", "py", "rs", "cpp", "c", "h", "hpp", "cs", "java",
   "go", "rb", "php", "swift", "kt", "lua", "sh", "bash", "zsh", "ps1",
   "html", "htm", "css", "scss", "less", "vue", "svelte", "sql"],
  { icon: "code", color: "#61afef" }
);

// Data / Config
register(
  ["json", "xml", "yaml", "yml", "toml", "ini", "cfg", "conf", "env",
   "log", "lock", "gitignore", "editorconfig"],
  { icon: "data_object", color: "#56b6c2" }
);

// Archives
register(
  ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "cab", "iso", "dmg"],
  { icon: "folder_zip", color: "#d19a66" }
);

// 3D
register(
  ["obj", "fbx", "gltf", "glb", "usdz", "usda", "usdc", "usd", "dae",
   "ply", "stl", "3ds", "abc"],
  { icon: "view_in_ar", color: "#c678dd" }
);
register(["blend"], { icon: "view_in_ar", color: "#ea7600" });

// Scene files (DCC apps)
register(["nk", "nkple"], { icon: "auto_fix_high", color: "#f5c518" });
register(["hip", "hipnc", "hiplc"], { icon: "auto_fix_high", color: "#ff4713" });
register(["ma", "mb"], { icon: "auto_fix_high", color: "#37a5cc" });
register(["max"], { icon: "auto_fix_high", color: "#0696d7" });
register(["c4d"], { icon: "auto_fix_high", color: "#011a6a" });
register(["aep", "aet"], { icon: "auto_fix_high", color: "#9999ff" });
register(["prproj"], { icon: "auto_fix_high", color: "#9999ff" });

// Fonts
register(["ttf", "otf", "woff", "woff2", "eot"], { icon: "font_download", color: "#9d9d9d" });

// Executables
register(["exe", "msi", "dll", "sys", "bat", "cmd", "com"], { icon: "terminal", color: "#9d9d9d" });
register(["app"], { icon: "terminal", color: "#9d9d9d" });

// Shortcuts / links
register(["lnk", "url", "desktop"], { icon: "link", color: "#61afef" });

export function getFileIcon(extension: string, isDir: boolean): FileIcon {
  if (isDir) return FOLDER_ICON;
  const ext = extension.replace(/^\./, "").toLowerCase();
  return extensionMap[ext] ?? UNKNOWN_ICON;
}
