# Mount UI Redesign — Testing Guide (Windows)

## What Changed

Unified mount connect/disconnect across all UI surfaces. Previously only "Restart" was available outside Settings.

### Summary of Changes

**Backend (Rust Tauri commands)**
- `src-tauri/src/commands.rs` — Added `mount_start` and `mount_stop` commands
- `src-tauri/src/lib.rs` — Registered both in invoke_handler

**Frontend (TypeScript/SolidJS)**
- `src/lib/tauri.ts` — Added `mountStart`, `mountStop` bindings
- `src/stores/mountStore.ts` — Added `start()`, `stop()`, `toggleMount()` methods
- `src/components/SubscriptionPanel/SubscriptionPanel.tsx` — Mount context menu now has Connect/Disconnect; mount rows have an inline toggle button (appears on hover)
- `src/components/SubscriptionPanel/SubscriptionPanel.css` — Styles for `.mount-toggle-btn`
- `src/components/Settings/SettingsDialog.tsx` — Removed duplicate "Live Status" section; relabeled "Enabled" to "Auto-connect on startup"

**Agent (mediamount-agent)**
- `mediamount-agent/src/orchestrator.rs` — Orchestrator no longer exits on Stop, so it can accept a subsequent Start (reconnect)
- `mediamount-agent/src/ipc/unix_server.rs` — IPC server now broadcasts to multiple clients (macOS only, not relevant to Windows)
- `mediamount-agent/src/tray.rs` — Windows tray: added `IDM_TOGGLE_BASE` Connect/Disconnect menu items per mount. Linux tray: same with muda MenuItem

**macOS tray (not relevant to Windows)**
- `mediamount-tray/MediaMountTray.swift` — Added Connect/Disconnect/Restart buttons and IPC send methods

## Windows Build & Test

### Build

```sh
# From repo root
cd mediamount-agent && cargo build
cd ../src-tauri && cargo build
# Or just:
cargo tauri dev
```

No extra sidecar copy steps on Windows — the agent binary is found automatically at `mediamount-agent/target/debug/mediamount-agent.exe`.

### What to Test

1. **Sidebar (Bookmarks panel)**
   - Hover over a mount row — a play/stop icon should appear on the right edge
   - Click the icon to disconnect a mounted drive — state should change to Stopped
   - Click again to reconnect — state should return to Mounted
   - Right-click a mount — context menu should show Connect or Disconnect (depending on state), Restart, then navigation items

2. **Settings > Mounts**
   - The old "Live Status" section (with duplicate checkboxes and restart buttons) should be gone
   - The "Enabled" checkbox in the mount editor should now read "Auto-connect on startup"
   - The checkbox in the config list should have updated tooltips

3. **Windows System Tray**
   - Right-click the tray icon
   - Each mount should show: status line (disabled), then "Connect {name}" or "Disconnect {name}", then "Restart {name}"
   - Click Connect/Disconnect — mount state should toggle
   - Tooltip should update with mount counts

4. **Reconnect after disconnect**
   - Disconnect a mount via any surface (sidebar, tray, or context menu)
   - Reconnect via any surface
   - This previously broke because the orchestrator exited on Stop — now it stays alive

### Key Files for Windows Tray

The Windows tray changes are in `mediamount-agent/src/tray.rs` in the `windows_tray` module:
- `IDM_TOGGLE_BASE = 1100` — menu ID constant for connect/disconnect items
- `show_context_menu()` — renders the per-mount toggle items
- `handle_menu_command()` — dispatches Start/Stop events based on current mount state

### Known Non-Issues

- The `ipc/server.rs` (Windows named pipe) still has single-client design. This is fine because on Windows the tray runs inside the agent process (via thread), not as a separate IPC client. Only UFB connects over the pipe.
- `mesh_sync` permission errors in the log are unrelated to these changes.
