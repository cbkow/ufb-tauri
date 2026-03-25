# macOS Support Plan

## Context

UFB and the mediamount-agent have full Windows support and Linux support, but macOS compiles with many features stubbed or broken. The developer is on Windows and won't have a Mac immediately. This plan focuses on what can be done now on Windows (cfg gate fixes, frontend fixes) vs what needs a Mac later.

## Phase 1: Can Do Now on Windows (No Mac Required) -- DONE

### 1A. Widen `#[cfg(target_os = "linux")]` to `#[cfg(unix)]` where macOS is compatible

Unix domain sockets, flock, libc, file permissions, symlinks -- all work identically on macOS. These are mechanical gate changes.

**Main app (`src-tauri/src/mount_client.rs`):**
- Unix socket connect block: `#[cfg(unix)]`
- Unsupported platform fallback: `#[cfg(not(any(windows, unix)))]`
- `connect_to_agent_unix()`: `#[cfg(unix)]`
- Socket path: Uses `XDG_RUNTIME_DIR` with `/tmp` fallback -- works on macOS (falls to `/tmp`)

**Agent (`mediamount-agent/src/main.rs`):**
- `MutexGuard` struct: `#[cfg(unix)]`
- Fallback stub: `#[cfg(not(any(windows, unix)))]`
- `ensure_single_instance()`: `#[cfg(unix)]`
- Fallback stub: `#[cfg(not(any(windows, unix)))]`
- IPC server start: `#[cfg(unix)]`
- Fallback exit: `#[cfg(not(any(windows, unix)))]`
- Main event loop: `#[cfg(any(windows, unix))]`
- `open_log()`: Added `#[cfg(target_os = "macos")]` branch using `open` command (not `xdg-open`)

**Agent (`mediamount-agent/src/ipc/mod.rs`):**
- `unix_server` module: `#[cfg(unix)]`

**Agent (`mediamount-agent/src/orchestrator.rs`):**
- `retrieve_credentials()`: `#[cfg(unix)]` (file-based cred store works on macOS)
- `mount_drive()`: Added `#[cfg(target_os = "macos")]` stub with clear error: "macOS SMB mounting not yet implemented"
- `disconnect_drive()`: Added `#[cfg(target_os = "macos")]` stub (log warning, no-op)
- Linux-only blocks using gio/proc remain `#[cfg(target_os = "linux")]`

**Agent (`mediamount-agent/src/platform/mod.rs`):**
- Linux platform module: `#[cfg(unix)]` (credentials + symlink mountpoint work on macOS; the gio SMB session is only called from orchestrator which has its own gate)
- `is_drive_in_use()`: The `/proc/mounts` check stays Linux-only; macOS uses the existing `#[cfg(not(any(...)))]` fallback with `Path::exists()`
- Auto-start stubs: Left as-is for now (macOS LaunchAgent support in Phase 2)

### 1B. Frontend: Cmd+key shortcuts -- DONE

**`src/components/FileBrowser/FileBrowser.tsx` -- `onKeyDown()` function:**

Added `const modKey = e.ctrlKey || e.metaKey` and replaced all `e.ctrlKey` checks:
- Ctrl/Cmd+A: select all
- Ctrl/Cmd+C: copy
- Ctrl/Cmd+X: cut
- Ctrl/Cmd+V: paste
- Ctrl/Cmd+Shift+N: new folder
- Ctrl/Cmd+Backspace: delete (macOS alternative to Delete key)

Note: `FileGridView.tsx` and `FileListView.tsx` already use `e.ctrlKey || e.metaKey` for selection -- no changes needed there.

### 1C. Improved error messages for unimplemented macOS features -- DONE

**`src-tauri/src/commands.rs` `mount_smb_share()`:**
- macOS: "SMB mounting not yet implemented on macOS. Use Finder > Go > Connect to Server to mount SMB shares."
- Other platforms: "SMB mounting is not available on this platform"

---

## Phase 2: Needs macOS Compile-Testing

These can be written on Windows but need a macOS build to verify they compile.

### 2A. macOS SMB mounting (agent)
- Create `mediamount-agent/src/platform/macos/` module with `mod.rs`, `fallback.rs`
- Use `open smb://user@server/share` (mounts to `/Volumes/` automatically, no root needed) then symlink from expected path
- Alternative: `mount_smbfs //user:pass@server/share /mount/point` (needs root)

### 2B. macOS auto-start (agent)
- Write LaunchAgent plist to `~/Library/LaunchAgents/com.ufb.mediamount-agent.plist`

### 2C. macOS tray icon (agent)
- `tray-icon` crate supports macOS natively (NSStatusBar). Add `cfg(target_os = "macos")` dependency section and tray implementation.

### 2D. macOS native clipboard
- Use `objc2` crate for NSPasteboard file URL support
- Enables Cmd+C in UFB -> Cmd+V in Finder

---

## Phase 3: Needs Hands-On macOS Testing

### 3A. Drag-out to Finder
- Test if `tauri-plugin-drag` handles macOS drag already
- If not, implement via NSDraggingItem

### 3B. SMB mount verification
- Test `open smb://` vs `mount_smbfs` approaches
- Verify /Volumes/ symlink strategy

### 3C. Window chrome
- Verify traffic lights don't overlap custom titlebar

---

## What Phase 1 Unlocks

After Phase 1, a macOS user would get:
- App launches and browses files normally
- Keyboard shortcuts work (Cmd+C/V/X/A)
- Agent connects via IPC (Unix socket)
- Single-instance enforcement works
- Search uses Spotlight (mdfind) -- already implemented
- Trash deletion works -- `trash` crate supports macOS
- Thumbnails, transcode, mesh sync -- all cross-platform
- Clipboard copy/paste -- plain text (not Finder-native, but functional)
- Clear error messages for unimplemented features (SMB mount)

**Not working yet:** Native Finder clipboard interop, drag-out to Finder, SMB mounting via agent, tray icon, auto-start.

## Verification

- `cargo check` in `src-tauri/` (Windows cross-check -- cfg gates are compile-time)
- `cargo check` in `mediamount-agent/` (same)
- `cargo test` in `mediamount-agent/` (state machine tests should pass)
- Manual test: keyboard shortcuts in browser with metaKey (can verify via devtools on Windows)
- Full macOS testing requires a Mac build later
