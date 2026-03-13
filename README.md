# Union File Browser

A cross-platform desktop application for visual effects and post-production file management, built with Tauri, Rust, and SolidJS.

## Features

### File Browsing
- Dual-panel file browsers with bookmarks sidebar
- Quick access to user folders (Desktop, Documents, Downloads) and all drives
- Thumbnails for images, video, PSD, EXR, SVG, PDF, and Blender files
- OLE drag-and-drop between panels and to/from the OS file manager
- Context menus with cut/copy/paste, rename, delete to Recycle Bin

### Job Views
- Subscribe to project folders to open tabbed Job Views
- Each job auto-discovers subfolders as tabs (ae, c4d, flame, etc.)
- Per-folder file browser with item list and renders/projects panels
- Layout auto-detection: 2-panel (item list + browser) or 3-panel (item list + projects + renders)

### Dynamic Columns & Metadata
- Define custom columns per job/folder: text, dropdown, date, number, priority, checkbox, links, notes
- Column presets: save individual column definitions and add them to any folder
- Dropdown columns with color-coded options
- All metadata stored in SQLite, synced across the facility via Mesh Sync
- Aggregated Tracker view across all subscribed projects

### Mesh Sync
- Peer-to-peer metadata synchronization over the local network
- HTTP API for metadata updates with UDP multicast heartbeats for peer discovery
- Leader election with deterministic sorting by tags and node ID
- Selective database snapshots (syncs only project data, never personal data like bookmarks or settings)

### Media Tools
- Video transcode queue (FFmpeg-based, MOV to MP4)
- Thumbnail generation for images, video, PSD/AI, EXR, SVG, PDF, and Blender files

### Integrations
- Nilesoft Shell context menu extensions (Union Files, Folders, Terminal, Goto)
- Custom URI protocols (`ufb:///` and `union:///`) with cross-OS path translation
- Google Drive project notes integration
- Database backup and restore

## Building

### Prerequisites

- [Rust](https://rustup.rs/) (stable)
- [Node.js](https://nodejs.org/) 18+
- Windows 10 SDK (for Windows builds)
- FFmpeg development libraries (see `src-tauri/build.rs` for lookup paths)

### Development

```bash
npm install
cargo tauri dev
```

### Production Build

```bash
cargo tauri build
```

The executable is output to `src-tauri/target/release/`.

### Installer

An Inno Setup script is provided at `installer/ufb_tauri_installer.iss`. Build the app first with `cargo tauri build`, then compile the `.iss` with [Inno Setup 6](https://jrsoftware.org/isinfo.php). The installer handles:

- Application files and runtime dependencies (FFmpeg, DLLs)
- URI protocol registration (`ufb:///`, `union:///`)
- Nilesoft Shell context menu integration with backup/restore
- Windows Firewall rules for Mesh Sync (TCP 49200, UDP 4244)
- Desktop and Start Menu shortcuts
- User data cleanup options

## Architecture

- **Backend**: Rust (Tauri v2) with 17+ modules — database, metadata, columns, mesh sync, thumbnails, transcode, etc.
- **Frontend**: SolidJS with reactive stores and component-based UI
- **Database**: SQLite (rusqlite, WAL mode) at `%LOCALAPPDATA%/ufb/ufb.db`
- **Settings**: JSON at `%LOCALAPPDATA%/ufb/settings.json`

## License

GPL-3.0 - see [LICENSE](LICENSE) for details.

Third-party licenses are in the [LICENSES](LICENSES/) directory.
