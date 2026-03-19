# Windows Build Notes

## Prerequisites

- **Rust toolchain** — `rustup` with `stable-x86_64-pc-windows-msvc`
- **Node.js** — v18+ with npm
- **Visual Studio Build Tools** — C/C++ workload (for cc crate / FFmpeg linking)
- **FFmpeg** — place in `src-tauri/external/ffmpeg/` (include, lib, bin dirs) or set `FFMPEG_DIR`
- **ExifTool** — place `exiftool.exe` in `src-tauri/external/exiftool/` (optional)
- **Inno Setup 6** — for building the installer (optional)

## rclone removal (v0.1.7+)

rclone has been completely removed. The following are **no longer needed**:

- `src-tauri/external/rclone/rclone.exe` — delete if present
- `src-tauri/external/rclone/rclone.1` — delete if present
- WinFSP — no longer a runtime dependency for MediaMount

The mediamount-agent now uses direct SMB mounts only (via `WNetAddConnection2` + `DefineDosDevice` on Windows).

## Build steps

### 1. Build the frontend

```
npm install
npm run build
```

### 2. Build the Tauri app (release)

```
cd src-tauri
cargo build --release
```

Output: `src-tauri/target/release/ufb-tauri.exe`

### 3. Build the mediamount-agent (release)

```
cd mediamount-agent
cargo build --release
```

Output: `mediamount-agent/target/release/mediamount-agent.exe`

### 4. Copy agent next to main app

```
copy mediamount-agent\target\release\mediamount-agent.exe src-tauri\target\release\
```

### 5. Build installer (optional)

Open `installer/ufb_tauri_installer.iss` in Inno Setup 6 and compile. The installer expects:

- `src-tauri/target/release/ufb-tauri.exe`
- `mediamount-agent/target/release/mediamount-agent.exe`
- `src-tauri/target/release/*.dll` (FFmpeg DLLs)
- `src-tauri/target/release/ffmpeg.exe`, `ffprobe.exe`

No rclone binaries are needed.

## Verification

1. Run `ufb-tauri.exe` — app should launch
2. Run `mediamount-agent.exe` — tray icon appears, mounts SMB shares from `%LOCALAPPDATA%\ufb\mounts.json`
3. Settings > MediaMount Agent — should show simplified config (no rclone tuning, no cache section)
4. Mount drive letter (e.g. M:) should map to the UNC share path
5. Explorer nav pane pins and Nilesoft Shell shortcuts should work as before
