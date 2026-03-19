# Linux Compatibility: mediamount-agent + UFB Backend

## Context

UFB-Tauri is a file browser with an rClone sidecar (mediamount-agent) for mounting NAS shares. The app was Windows-only. Recent uncommitted work added: system FFmpeg detection, binary PATH fallback, cross-platform path canonicalization, `keyring` crate, Linux deb bundle config, `get_platform()` command, and UI platform gating.

What remains: the mediamount-agent is entirely Windows-only (named pipes IPC, DefineDosDevice drive mapping, Windows Credential Manager, Win32 tray icon), and several UFB backend modules still have Windows-only code paths.

### Design Decisions

- **Mount indirection**: Symlink-based (user-facing path is a symlink to active backend, mirroring Windows DefineDosDevice)
- **NAS access on Linux**: Named rclone remote (user configures in `rclone.conf`)
- **System tray**: `tray-icon` + `muda` crates (Tauri ecosystem, cross-platform)

---

## Phase 1: MountConfig Schema -- Linux Path Fields

**Goal**: Extend config to support Linux mount paths alongside Windows drive letters.

### `mediamount-agent/src/config.rs`
- Add three `Option<String>` fields to `MountConfig`:
  ```rust
  #[serde(default)] pub rclone_mount_path: Option<String>,  // e.g. "/mnt/nas-rclone"
  #[serde(default)] pub smb_mount_path: Option<String>,      // e.g. "/mnt/nas-smb"
  #[serde(default)] pub mount_path_linux: Option<String>,     // e.g. "/mnt/nas" (user-facing)
  ```
- Make `mount_path()` platform-aware:
  - Windows: `format!("{}:\\", self.mount_drive_letter)` (unchanged)
  - Linux: `self.mount_path_linux.or(self.rclone_mount_path).unwrap_or("/mnt/media")`
- Add `rclone_target_path()` helper:
  - Windows: `format!("{}:\\", self.rclone_drive_letter)`
  - Linux: `self.rclone_mount_path.unwrap_or("/mnt/media-rclone")`
- Add `rclone_remote` field: on Linux, this is the rclone remote spec (e.g. `mynas:media`) configured in the user's `rclone.conf`. Required on Linux. On Windows this field is ignored (UNC path is used directly).

### `src-tauri/src/mount_client.rs`
- Mirror the same new fields in the duplicated `MountConfig` struct

### `src/lib/types.ts`
- Add `rcloneMountPath?: string`, `smbMountPath?: string`, `mountPathLinux?: string`, `rcloneRemote?: string` to `MountConfig`

---

## Phase 2: IPC -- Unix Domain Sockets

**Goal**: Replace Windows named pipes with Unix domain sockets on Linux. The wire protocol (length-prefixed JSON from `ipc/mod.rs`) is transport-agnostic.

### New file: `mediamount-agent/src/ipc/unix_server.rs`
- `IpcServer` struct matching the Windows version's interface: `command_rx`, `send()`, `start()`
- Socket path: `$XDG_RUNTIME_DIR/ufb/mediamount-agent.sock` (fallback: `/tmp/ufb-mediamount-agent.sock`)
- Use `tokio::net::UnixListener` for accepting connections
- One active client at a time (same model as Windows named pipe)
- Read side runs in `spawn_blocking` (same pattern as Windows)
- Clean up stale socket on startup (remove before bind)

### `mediamount-agent/src/ipc/mod.rs`
- Add `#[cfg(target_os = "linux")] pub mod unix_server;`
- The `write_message`/`read_message`/`send_message`/`recv_message` functions already work with any `Read`/`Write` -- no changes needed

### `src-tauri/src/mount_client.rs`
- Add `#[cfg(target_os = "linux")]` connection path using `std::os::unix::net::UnixStream`
- `connect_to_agent_unix()` -> connect to the same socket path
- Use `.try_clone()` for separate read/write halves (no `DuplicateHandle` needed)

---

## Phase 3: Linux Platform Module

**Goal**: Implement the three platform traits for Linux.

### New file: `mediamount-agent/src/platform/linux/mod.rs`
- Re-export `LinuxMountMapping`, `LinuxSmbSession`, `LinuxCredentialStore`

### New file: `mediamount-agent/src/platform/linux/mountpoint.rs` -- `LinuxMountMapping` impl `DriveMapping`
- **Symlink indirection** (mirrors DefineDosDevice on Windows):
  - User-facing path (e.g. `/mnt/nas`) is a symlink pointing to whichever backend is active
  - `switch(mount_point, target_path)`: Remove old symlink, create new one pointing to target (rclone FUSE path or SMB mount path)
  - `read_target(mount_point)`: `std::fs::read_link`
  - `remove(mount_point)`: Remove symlink
  - `verify(mount_point, expected)`: Check symlink target matches
  - Ensure parent directory exists before creating symlink
  - Handle case where path exists but is not a symlink (log warning, refuse to overwrite)

### New file: `mediamount-agent/src/platform/linux/fallback.rs` -- `LinuxSmbSession` impl `SmbSession`
- Translate UNC `\\nas\media` -> `//nas/media`
- Try `gio mount smb://nas/media` (GVFS, no root needed) first
- Fall back to checking if already mounted via `/proc/mounts`
- If pre-mounted via fstab, return Ok

### New file: `mediamount-agent/src/platform/linux/credentials.rs` -- `LinuxCredentialStore` impl `CredentialStore`
- Use `keyring` crate (Secret Service D-Bus API)
- Store as JSON `{"u":"...","p":"..."}` under service `"mediamount-agent"`

### `mediamount-agent/src/platform/mod.rs`
- Add `#[cfg(target_os = "linux")] pub mod linux;`
- `is_drive_in_use()`: On Linux, check `/proc/mounts` for the given path
- `set_auto_start()`: Write/remove `~/.config/autostart/mediamount-agent.desktop`
- `is_auto_start_enabled()`: Check if that file exists

### `mediamount-agent/Cargo.toml`
- Add `[target.'cfg(not(windows))'.dependencies]` section with `keyring = "3"`

---

## Phase 4: Agent main.rs Linux Support

**Goal**: Make the main event loop run on Linux.

### `mediamount-agent/src/main.rs`
- **Single-instance**: Replace Windows mutex with `flock(2)` on a lock file at `$XDG_RUNTIME_DIR/ufb/mediamount-agent.lock`
- **IPC server**: Replace the non-Windows error/exit with `ipc::unix_server::IpcServer::start()`
- **Main event loop**: Remove the `#[cfg(windows)]` gate -- the loop body is platform-agnostic (channels + tokio::select). The tray branch is a no-op until Phase 7.
- **Logging**: Already has Linux path (`~/.local/share/ufb/mediamount-agent.log`)

---

## Phase 5: rClone Spawn Adaptations

**Goal**: Adapt rclone spawning for Linux FUSE.

### `mediamount-agent/src/rclone/mod.rs`
- **Mount destination**: `config.rclone_target_path()` instead of `format!("{}:", config.rclone_drive_letter)`
- **Ensure mount dir exists**: `create_dir_all` on the mount path before spawning
- **Remote spec**: On Linux, use `config.rclone_remote` field (e.g. `mynas:media`). User configures the remote in their `rclone.conf` -- supports SMB, SFTP, S3, or any rclone backend. If `rclone_remote` is empty, log an error and fail (don't attempt UNC path on Linux).
- **FUSE check**: On Linux, verify `/dev/fuse` exists (warn about `fuse3` package if missing)
- **Orphan cleanup**: Add `#[cfg(target_os = "linux")]` version using `pgrep -a rclone` + `kill`
- **No `CREATE_NO_WINDOW`**: Already `#[cfg(windows)]` gated

---

## Phase 6: Orchestrator Wiring

**Goal**: Wire Linux platform implementations into effect dispatch.

### `mediamount-agent/src/orchestrator.rs`
- `dispatch_effect(MapDriveToRclone)`: Use `config.rclone_target_path()` on Linux
- `dispatch_effect(MapDriveToSmb)`: Use `config.smb_mount_path` on Linux (not UNC path)
- `switch_drive_mapping()`: Add `#[cfg(target_os = "linux")]` block using `LinuxMountMapping`
- `ensure_smb_session()`: Add `#[cfg(target_os = "linux")]` block using `LinuxSmbSession` + `LinuxCredentialStore`
- `run()` startup:
  - Orphan cleanup: pass mount path on Linux instead of drive letter
  - Drive conflict check: check if mount path is occupied via `/proc/mounts`

---

## Phase 7: System Tray on Linux

**Goal**: Implement tray icon using `tray-icon` + `muda` crates (Tauri ecosystem, cross-platform).

### `mediamount-agent/src/tray.rs`
- Add `#[cfg(target_os = "linux")]` block in `TrayManager::start` using `tray-icon` + `muda`
- Run on a dedicated thread with a GTK main loop (`gtk::init()` + `gtk::main_iteration_do()` polling)
- Build menu with `muda::Menu` / `muda::MenuItem`:
  - Status display (disabled) with cache usage + dirty files
  - Mount controls: Switch to SMB, Force rClone, Restart, Flush & Restart
  - System: Open UFB, Open Log, Auto-start toggle, Quit
- Icon: Load from bundled PNG/SVG file, or embed icon bytes in the binary
- State updates via shared `Arc<Mutex<TrayState>>` (same pattern as Windows)
- Menu actions send `TrayCommand` variants through the existing `cmd_tx` channel
- Poll for state updates + `MenuEvent::receiver()` on a timer (500ms, same as Windows)

### `mediamount-agent/Cargo.toml`
- Add under `[target.'cfg(target_os = "linux")'.dependencies]`:
  ```toml
  tray-icon = "0.19"
  muda = "0.15"
  gtk = "0.18"    # Required by tray-icon on Linux
  ```

---

## Phase 8: UFB Backend Remaining Items

### `src-tauri/src/commands.rs`
- **`get_drives()`**: On Linux, parse `/proc/mounts` to return user-relevant mount points (filter out `/proc`, `/sys`, `/dev`, tmpfs, etc.)
- **`mount_launch_agent()`**: Verify sidecar binary resolution works on Linux (should already work via `resolve_binary()`)
- **`mount_hide_drives()` / `mount_unhide_drives()`**: Already Windows-gated, return no-op error on Linux

### `src-tauri/src/drag_out.rs`
- Already returns "not implemented" stub on non-Windows. The `tauri-plugin-drag` may provide Linux support. Leave as-is for now.

### `src-tauri/src/explorer_pins.rs`
- Windows-only feature. Already `#[cfg(windows)]` gated. No Linux equivalent needed.

---

## Phase 9: Frontend Updates

### `src/stores/mountStore.ts` -- `getMountForPath()`
- Add platform check: on Linux, match against `cfg.mountPathLinux` or `cfg.rcloneMountPath` instead of drive letters

### `src/components/Settings/SettingsDialog.tsx`
- **Drive letter fields**: Wrap in `<Show when={platform() === "win"}>`
- **Mount path fields**: Add `<Show when={platform() === "lin"}>` section with inputs for rclone mount path, SMB mount path, user-facing mount path, rclone remote spec
- **NAS share path help text**: Update to mention Linux format when platform is "lin"
- **`defaultMountConfig()`**: Platform-aware defaults (empty mount paths for Linux)

### `src/App.tsx`
- Cache `getPlatform()` result at app initialization in a signal/store so it's available everywhere without re-fetching

---

## Verification

1. **Compile check**: `cargo build` for both `src-tauri` and `mediamount-agent` on Linux
2. **Agent standalone test**: Run `mediamount-agent` on Linux, verify:
   - Lock file prevents duplicate instances
   - Unix socket is created and accepts connections
   - Config loads from `~/.local/share/ufb/mounts.json`
   - Log writes to `~/.local/share/ufb/mediamount-agent.log`
3. **IPC test**: Run UFB + agent, verify mount state updates flow to the frontend
4. **rClone mount test**: Configure a mount with rclone remote, verify FUSE mount appears at configured path
5. **Credential test**: Store/retrieve credentials via Settings dialog
6. **Health probe test**: Verify health checks work on the FUSE mount path
7. **Tray test**: Verify tray icon appears with correct menu items and state updates
8. **Frontend test**: Verify Settings dialog shows mount path fields on Linux, drive letter fields on Windows
