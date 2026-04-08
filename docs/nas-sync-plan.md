# NAS On-Demand Sync — Plan

A cross-platform agent that presents files from a Synology SMB share as
on-demand placeholders in the native file explorer, using each OS's first-class
cloud files API. Files appear locally but are only downloaded when accessed.
Writes go directly to the NAS — the local machine is a cache, not a replica.

---

## Core Principle

**The NAS is the only source of truth. The local machine is a smart cache.**

There is no sync, no reconciliation, no staleness model, no local database.
Every operation is a pass-through to the NAS via SMB:

- Open a folder → SMB `readdir` → build/refresh placeholders
- Open a file → SMB read → stream to OS cache → local SSD speed on repeat access
- Save a file → SMB write → convert local file to placeholder
- Delete/rename → SMB delete/rename → update placeholder

The OS manages the local cache automatically. Hydrated files are served from
local disk at full speed. The OS dehydrates them when disk space is needed.
Next access re-fetches from the NAS.

---

## Live Change Detection

The agent uses native SMB change notifications to keep placeholders in sync
with the NAS in real time. No polling, no scanning, no tree walking.

### How It Works

1. When the user navigates to a folder, `FETCH_PLACEHOLDERS` populates it
   and the agent registers a change watch on the corresponding NAS folder.
2. The NAS holds the watch request open (SMB2 `CHANGE_NOTIFY`) and responds
   when something changes — a file is added, removed, renamed, or modified.
3. The agent receives the event and translates it into placeholder operations:
   - File added on NAS → `PlaceholderFile::create()` in client folder
   - File removed on NAS → `std::fs::remove_file()` on client placeholder
   - File renamed on NAS → remove old placeholder, create new one
   - File modified on NAS → update placeholder metadata (dehydrate if needed)
4. When the user navigates away and the folder is no longer active, the
   watch is dropped. No background cost for unvisited folders.

### Platform APIs

| Platform | API | Notes |
|---|---|---|
| Windows | `ReadDirectoryChangesW` | Watches subtree, event-driven, same API Explorer uses |
| macOS | `FSEvents` | Works on SMB mounts, same API Finder uses |

### Filtering

Synology generates noise from internal bookkeeping (`@eaDir`, `#recycle`).
All paths starting with `@` or `#` are filtered out before processing.

---

## Shared Rust Core

A platform-agnostic crate exposing simple SMB operations. Both the Windows
and macOS platform layers call into this. No OS-specific code here.

```
list_dir(smb_path)                → Vec<FileEntry>   // name, size, mtime, is_dir
read_file(smb_path, offset, len)  → bytes            // streaming read for hydration
write_file(smb_path, data)        → Result           // local save → NAS
delete(smb_path)                  → Result
rename(smb_path, new_smb_path)    → Result
watch_dir(smb_path)               → Receiver<Event>  // SMB change notifications
```

The core handles SMB connection management, credential retrieval, and
reconnect logic. Platform layers never touch SMB directly.

---

## Windows

### API
**Windows Cloud Files API (CF API)** — available since Windows 10 1709. The same
infrastructure used by OneDrive, Dropbox, and Google Drive Files On-Demand.

### Crate
**`cloud-filter`** v0.0.6 on crates.io — safe Rust wrapper around the CF API.
Fork of `wincs`, MIT licensed. Validated in Phase 0 spike (`spikes/cf-spike/`).
Plan to fork if maintenance becomes an issue.

### Deployment
A standalone Rust sidecar binary. No app bundle, no driver. Registers directly
with the OS via `CfRegisterSyncRoot`.

### Callback → SMB Mapping

| CF API Callback | Action |
|---|---|
| `FETCH_PLACEHOLDERS` | `list_dir(smb_path)` → return placeholders, start NAS watch |
| `FETCH_DATA` | `read_file(smb_path)` → stream bytes to CF API |
| `state_changed` | New local file detected → `write_file(smb_path)` → `convert_to_placeholder()` |
| `delete` | `delete(smb_path)` → approve deletion |
| `rename` | `rename(smb_path, new)` → approve rename |
| `dehydrate` | Approve — OS reclaims local cache space |

### NAS Watch → Placeholder Mapping

| SMB Change Event | Action |
|---|---|
| `ADDED` | `PlaceholderFile::create()` in client folder |
| `REMOVED` | `std::fs::remove_file()` on client placeholder |
| `RENAMED_OLD` + `RENAMED_NEW` | Remove old, create new placeholder |
| `MODIFIED` | Update placeholder metadata via `Placeholder::update()` |

### Hydration
Streaming in 4MB chunks via `ticket.write_at()` at progressive offsets.
Progress reported via `ticket.report_progress()` so Explorer shows feedback.
CF API has a 60-second callback timeout — chunked streaming resets this timer
on each write.

### Identity Blob
Each placeholder stores the SMB path as its identity blob (up to 4 KB).
This is the only state we maintain per file — and it's stored in the
placeholder itself, not in a database.

### Cache Persistence
Placeholders and hydrated content are real NTFS files — they survive reboots.
Register the sync root once (on install). On subsequent launches, just
reconnect the filter. The OS manages dehydration when disk space is needed.

### Cache Tracking and Eviction
A per-mount SQLite database tracks hydrated files for cache size limiting:

```sql
CREATE TABLE cache_index (
    path    TEXT PRIMARY KEY,
    size    INTEGER NOT NULL,
    accessed INTEGER NOT NULL  -- unix timestamp, updated on each hydration
);
CREATE INDEX idx_accessed ON cache_index(accessed);
```

- On `FETCH_DATA` completion: `INSERT OR REPLACE` with current timestamp
- On `dehydrate` callback: `DELETE` the row
- Cache size: `SELECT SUM(size) FROM cache_index`
- Eviction: `SELECT path FROM cache_index ORDER BY accessed LIMIT N`
  then programmatically dehydrate those files

User-configurable cache cap per mount (e.g., 200GB). When hydration pushes
total over the cap, evict LRU files until under budget. If the DB is lost,
rebuild by walking hydrated files on startup (one-time cost).

This is a cache index, not sync state — purely local bookkeeping independent
of the NAS source of truth. Uses `rusqlite` (already a dependency).

### Threading
CF API dispatches callbacks on its own thread pool. The `cloud-filter` crate
requires `Send + Sync` on the filter impl. SMB watches run on a background
thread and push events to the main loop.

---

## macOS

### API
**FileProvider / NSFileProviderReplicatedExtension** — available since macOS 11.
The same infrastructure used by iCloud Drive, Dropbox, and Google Drive on Mac.

### Crate
**`objc2-file-provider`** on crates.io — Rust bindings to Apple's FileProvider
framework, part of the actively maintained `objc2` family.

### Deployment
A macOS App Extension bundled inside the existing MediaMountTray Swift app
(already signed and notarized). The extension runs as a separate process,
instantiated by the system. Calls into the shared Rust core via FFI.

### Method → SMB Mapping

| FileProvider Method | Action |
|---|---|
| `enumerateItems(for:)` | `list_dir(smb_path)` → return item metadata, start NAS watch |
| `fetchContents(for:)` | `read_file(smb_path)` → return local URL to OS |
| `createItem` | `write_file(smb_path)` → local save to NAS |
| `modifyItem` | `write_file(smb_path)` → local edit to NAS |
| `deleteItem` | `delete(smb_path)` → remove from NAS |

### NAS Watch → FileProvider Mapping

SMB change events (via `FSEvents`) trigger
`NSFileProviderManager.signalEnumerator(for:)` which causes the system to
re-enumerate the affected container.

### Version Token
`NSFileProviderItemVersion` carries opaque `Data` — we store the SMB path,
same concept as the Windows identity blob.

### Cache Tracking and Eviction
Same SQLite cache index as Windows. On eviction, call
`NSFileProviderManager.evictItem(identifier:)` instead of CF API dehydrate.

---

## Phased Delivery

### Phase 1 — Single share, full pass-through with live updates
- Sync root / domain registration (Windows + macOS)
- `FETCH_PLACEHOLDERS` / `enumerateItems` → SMB `readdir`
- `FETCH_DATA` / `fetchContents` → SMB read, stream to OS
- Write-through: local saves → SMB write → convert to placeholder
- Delete/rename → SMB delete/rename
- **Live change detection** via SMB `ReadDirectoryChangesW` / `FSEvents`
- Proactive placeholder push/remove when NAS contents change
- Single hardcoded share
- SMB connection + credential management in shared core

### Phase 2 — Resilience and multi-share
- SMB reconnect handling (mid-hydration, mid-write, watch re-registration)
- Graceful offline behavior (hydrated files still accessible, watches resume on reconnect)
- Pin → proactive hydration, Unpin → let OS dehydrate
- Persisted sync root / domain configuration
- Multiple share support

---

## Known Risks

| Risk | Mitigation |
|---|---|
| SMB session drops mid-operation | Reconnect + retry in shared core; re-register watches |
| macOS App Extension sandboxing | SMB credentials via shared app group container |
| CF API callback threading (Windows) | `cloud-filter` crate handles dispatch; core must be `Send + Sync` |
| FileProvider process lifecycle (macOS) | System may terminate extension when idle — re-register watches on restart |
| Synology `@eaDir` / `#recycle` noise | Filter all paths starting with `@` or `#` in change events |
| SMB `CHANGE_NOTIFY` reliability | Validated against Synology — works (same mechanism Explorer uses) |
| NAS unreachable | Same UX as disconnected mapped drive — honest failure, no stale data |

---

## Crates of Interest

| Crate | Platform | Purpose |
|---|---|---|
| `cloud-filter` | Windows | CF API wrapper (validated in spike) |
| `objc2-file-provider` | macOS | FileProvider framework bindings |
| `windows` / `windows-sys` | Windows | SMB watch via `ReadDirectoryChangesW` |
| `objc2` | macOS | Objective-C runtime foundation |

---

## What We Proved (Spike)

Using `spikes/cf-spike/`:

- Sync root registration works (`SyncRootIdBuilder` + icon required)
- `FETCH_PLACEHOLDERS` fires on folder navigation, including subdirectories
- `FETCH_DATA` hydrates files from NAS via SMB (~20-30ms for small files)
- Identity blobs round-trip through placeholders
- `PlaceholderFile::create()` pushes new placeholders into already-open folders instantly
- Explorer caches populated folders — `FETCH_PLACEHOLDERS` only fires once per folder
- `ReadDirectoryChangesW` works on Synology SMB shares — real-time events for add/remove/rename/modify
- Delete/rename callbacks propagate to NAS correctly (handle duplicate callbacks gracefully)
- SMB `readdir` on NAS takes ~3-10ms for small folders
- Synology noise (`@eaDir`, `#recycle`) is easily filtered by prefix
- Live change detection via `ReadDirectoryChangesW` on NAS share works in real time
- Chunked 4MB streaming hydration with progress reporting works correctly
- 94MB file hydrates successfully over 10GbE with Explorer progress dialog
- Second access is instant — served from local SSD cache
- Cache (placeholders + hydrated data) persists across sessions as real NTFS files
