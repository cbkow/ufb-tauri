# Union File Browser (UFB)

A cross-platform desktop file browser + project management tool built for visual effects and post-production workflows. Runs natively on **Windows** and **macOS**.

Version: **0.5.1**
License: [GPL-3.0-or-later](LICENSE) (required by the WinFsp FLOSS exception)

## What it does

UFB is two things in one window:

1. A **dual-pane file browser** with list/grid/tree views, thumbnails for every format film + VFX teams use (image, video, PSD, EXR, PDF, SVG, Blender, AI/EPS, RAW), OLE drag-and-drop to/from Explorer + Finder, and a sidebar of bookmarks + mount volumes.
2. A **project tracker** — subscribe to job folders, get auto-tabbed Job Views with custom metadata columns (text, dropdown, date, priority, checkbox, links), and an aggregated Tracker across all subscribed projects. Peer-to-peer Mesh Sync keeps metadata in sync across the facility over the local network.

Bundled tools: FFmpeg transcode queue, ExifTool metadata, Pdfium, custom URI protocols (`ufb://`, `union://`), and shell integrations (Nilesoft Shell on Windows).

## On-demand NAS sync

UFB can present remote SMB shares as local folders at `C:\Volumes\ufb\{share}` (Windows) or `~/ufb/mounts/{share}` (macOS), with file contents fetched on first access and cached locally. Metadata is cached in SQLite for instant directory listings; block-level content is cached in sparse files with LRU eviction.

The backend differs by OS:

| | Windows | macOS |
|---|---|---|
| Kernel driver | [WinFsp](https://winfsp.dev/) (user-mode FS driver) | NFS3 loopback via [`nfsserve`](https://github.com/xetdata/nfsserve) crate |
| Mount point | `C:\Volumes\ufb\{share}` | `~/ufb/mounts/{share}` |
| How it works | `FSP_FILE_SYSTEM_INTERFACE` — our process answers read/write callbacks | Local NFS3 server bound to `127.0.0.1`, `mount -t nfs` from kernel |
| Installer dep | WinFsp MSI (bundled, silent-installed) | None |

Both cache dirs are user-configurable in Settings → Sync Cache. Changing the location drains existing caches and rebuilds at the new path.

## Features

### File browsing
- Three view modes per pane: **list**, **grid** (thumbnail slider 48-256px), **tree** (Finder-style disclosure triangles with lazy-load)
- Resizable columns with per-session persistence (localStorage)
- Dual-panel layout with cross-panel drag-drop + splitter; project-tab layouts get triplet views
- 256px system icons on Windows (SHIL_JUMBO) + proper thumbnails for every format via typed extractors (image crate, ffmpeg, resvg, psd, exr, pdfium, blend)
- "Reachability" probe — Bookmarks dim to *Unavailable* when a mount's NAS becomes unreachable (VPN drop, NAS offline)

### Job Views
- Subscribe to project folders; each opens as a tabbed Job View
- Auto-discovers known subfolders (`ae`, `c4d`, `flame`, `premiere`, `postings`, `renders`, etc.) and lays them out as per-folder tabs
- 2-panel or 3-panel layouts (auto-detected from folder structure)
- Per-folder metadata: tracked items with checkboxes, custom columns, and file listing

### Dynamic columns
- Define columns per job/folder: text, dropdown (color-coded), date, number, priority, checkbox, URL, notes
- Column Presets: save definitions and re-apply to any folder
- All metadata in SQLite (`%LOCALAPPDATA%\ufb\ufb_v2.db` / `~/Library/Application Support/ufb/ufb_v2.db`)
- Aggregated Tracker view across all subscribed projects

### Mesh Sync
- Peer-to-peer metadata synchronization over the local network
- UDP multicast heartbeats on port 4244 for peer discovery
- HTTP mesh on port 49200 for metadata push/pull
- Leader election; selective snapshots (project data only — personal bookmarks + settings stay local)

### Media tools
- Transcode queue (ffmpeg: MOV → MP4 + custom ffmpeg flag pipelines)
- Thumbnail extractors: `image` crate, resvg, psd, OpenEXR, pdfium, `.blend` embedded thumbnails, ffmpeg for video, Windows Shell for catch-all (Office docs, RAW, etc.)

### Integrations
- **Windows**: Nilesoft Shell context menu ("Union Files/Folders/Goto/Terminal") auto-installed when Nilesoft Shell is present; Explorer sidebar pins for all mounts; OLE drag-out
- **macOS**: Finder integration via NFS mount points; Quick Look thumbnails via native extractors
- URI protocols (`ufb:///`, `union:///`) with cross-OS path translation
- Google Drive project notes + Database backup/restore

## Running the installed app

### Windows

- **Windows Developer Mode must be enabled**. UFB's non-sync mounts are symlinks to UNC network paths at `C:\Volumes\ufb\{share}`, and Windows requires either elevation or Developer Mode to create them. Without Dev Mode, the agent falls back to prompting for UAC elevation on every mount, which is not a pleasant experience.
  - Turn on via **Settings → Privacy & security → For developers → Developer Mode**.
- Known issue — **first launch after install**: the first click on any mount bookmark may fail with `ERROR_UNTRUSTED_MOUNT_POINT` (Windows error 448). This is a Windows security restriction that SmartScreen applies while it scans the unsigned binary for the first time. **Workaround: quit UFB and relaunch it.** Second launch works reliably. See [`docs/windows-smartscreen-448.md`](docs/windows-smartscreen-448.md) for the full diagnosis. The real fix is code-signing, which this project hasn't invested in.
- WinFsp runtime is bundled with the installer and silent-installs on first run; no manual install required.

### macOS

- No special OS prerequisites. macOS `mount_nfs` ships with the OS.
- If prompted, grant the agent permission to access the network volumes.

## Building from source

### Prerequisites (both platforms)

- [Rust](https://rustup.rs/) stable toolchain
- Node.js 18+ and npm
- FFmpeg (development libraries — see `src-tauri/build.rs` for lookup paths)

### Windows-specific

- Windows 10 SDK (comes with Visual Studio Build Tools)
- **WinFsp Developer SDK** (headers + import lib) — required to compile the agent. Install the full WinFsp installer and check the "Developer" feature.
- **LLVM / libclang** — required by `winfsp-sys`'s bindgen step. Set `LIBCLANG_PATH` if it's not on PATH.
- [Inno Setup 6](https://jrsoftware.org/isinfo.php) for installer compilation

See [`docs/windows-build-prereqs.md`](docs/windows-build-prereqs.md) for full details.

### macOS-specific

- Xcode Command Line Tools (for linker + macOS SDK)
- `mount_nfs` is included with macOS; no additional NFS tooling needed

### Development

```bash
npm install
npm run tauri dev
```

The agent (`mediamount-agent`) is built separately — Tauri auto-launches it in dev mode:

```bash
cd mediamount-agent
cargo build
```

### Release build

```bash
# Main app
npm run tauri build -- --no-bundle

# Agent (Windows)
cd mediamount-agent
cargo build --release --target x86_64-pc-windows-msvc

# Agent (macOS)
cd mediamount-agent
cargo build --release --target aarch64-apple-darwin
```

Artifacts land in `src-tauri/target/release/` and `mediamount-agent/target/{triple}/release/`. Copy the agent binary next to the main app before packaging the installer.

### Installer (Windows)

`installer/ufb_tauri_installer.iss` packages:
- The Tauri app + agent + bundled FFmpeg/ExifTool binaries
- **WinFsp 2.1.25156 MSI** (silent-installed if not already present)
- URI protocol registration (`ufb:///`, `union:///`)
- Nilesoft Shell integration with backup/restore
- Firewall rules for Mesh Sync (TCP 49200, UDP 4244)
- Explorer nav pane pins for all configured mounts

Compile with Inno Setup 6's `ISCC.exe`. Output: `installer/ufb-tauri-setup-{version}.exe`.

## Architecture

- **Frontend**: SolidJS + Vite + TypeScript; reactive stores per file browser; `<Switch>`/`<Match>` driven view modes
- **Tauri backend**: Rust (Tauri v2) — app state, metadata, column config, mesh sync, thumbnails, transcode queue, subscription manager, system icon cache, Explorer nav pins
- **Agent (`mediamount-agent`)**: separate Rust process running in the system tray. Owns SMB session management, the VFS server (WinFsp / NFS), block-level cache, and NAS reachability probes. Talks to the main app over a Unix-domain or named-pipe socket.
- **Database**: SQLite with `r2d2` connection pool, WAL mode
- **Settings**: JSON at `%LOCALAPPDATA%\ufb\settings.json` (Windows) / `~/Library/Application Support/ufb/settings.json` (macOS)
- **Sync cache**: SQLite index + sparse content blobs, configurable root

### Related docs in `docs/`

- `windows-build-prereqs.md` — full Windows build environment setup
- `windows-winfsp-port-plan.md` — ProjFS → WinFsp port history + design
- `windows-io-backend-evaluation.md` — why WinFsp over ProjFS (decision record)
- `windows-drag-out-notes.md` — HGLOBAL leak diagnosis for future OLE drag-out work
- `windows-smartscreen-448.md` — first-launch `ERROR_UNTRUSTED_MOUNT_POINT` diagnosis + workaround
- `v0.5.1-release-notes.md` — changelog + carryover items for this release
- `nas-sync-plan.md`, `nas-sync-log.md`, `nfs-loopback-plan.md` — sync backend history

## Licensing

UFB itself is licensed under [GPL-3.0-or-later](LICENSE). The GPL choice is required by the **WinFsp FLOSS exception** — apps linking the WinFsp DLL must be distributed under an OSI/FSF-approved license, and GPL-3.0 satisfies that while keeping UFB copyleft.

Third-party licenses and attributions are in [`LICENSES/`](LICENSES/) (see `THIRD_PARTY_NOTICES.txt` for the full list).

## Credits

Built on:

- [Tauri](https://tauri.app/) • [SolidJS](https://solidjs.com/) • [WinFsp](https://winfsp.dev/) • [nfsserve](https://github.com/xetdata/nfsserve) • [FFmpeg](https://ffmpeg.org/) • [SQLite](https://sqlite.org/) • [PDFium](https://pdfium.googlesource.com/pdfium/) • [Material Symbols](https://fonts.google.com/icons)
