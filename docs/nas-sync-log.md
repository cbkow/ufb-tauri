# NAS On-Demand Sync — Decision Log

Running log of decisions, findings, and cross-platform notes.
Both Windows and macOS Claude Code instances should read and append to this file.

---

## 2026-04-07 — Project kickoff (Windows)

### cloud-filter crate evaluation

Researched `cloud-filter` v0.0.6 (crates.io) — Rust wrapper for Windows Cloud Files API.
Fork of `wincs` by ho-229, MIT licensed.

**Covers all needed APIs:**
- Sync root registration (`SyncRootIdBuilder`)
- Placeholder creation with custom identity blobs (up to 4 KB)
- All CF callbacks: fetch_placeholders, fetch_data, pin/unpin, dehydrate
- Both sync and async trait variants (async works with tokio via `block_on` closure)
- Ships with `cloud-mirror` example — working sync provider with chunked hydration

**Risks:**
- Version 0.0.6, pre-1.0, bus factor ~1 (maintainer: ho-229)
- Last commit August 2024 (~20 months stale)
- ~1,176 monthly downloads
- `PlaceholderFile` is `!Send`/`!Sync` — must create on same thread
- Threading is caller's responsibility (CF API dispatches on its own thread pool)
- `state_changed` uses `ReadDirectoryChangesW`, not native CF callback

**Decision:** Proceed with Phase 0 spike using `cloud-filter`. If viable, fork and own.
If not, fall back to raw `windows` crate CF API calls using `cloud-filter` source as reference.

### Architecture decisions

- macOS FileProvider extension will be hosted by the existing Swift tray app (MediaMountTray)
- Shared Rust core for SMB enumeration, staleness, and hydration — called via FFI from Swift
- Cross-platform communication via `docs/` markdown files in git

### Phase 0 spike (Windows) — PASSED

Goal: throwaway binary that proves CF API callback lifecycle works via `cloud-filter` crate.

**Result: Success.** Full callback lifecycle validated:
- Sync root registration works (`SyncRootIdBuilder` + `SyncRootInfo`) — requires a non-empty icon
- `FETCH_PLACEHOLDERS` callback fires when Explorer opens the sync root folder
- Placeholders appear in Explorer with cloud status icons
- `FETCH_DATA` callback fires on file open, hydration delivers content correctly
- Identity blobs (server path stored as bytes) round-trip through placeholders
- No threading issues observed in basic usage

**API notes discovered during spike:**
- `Request::path()` returns absolute path (volume letter + normalized), not relative to sync root
- `pass_with_placeholder` and `write_at` return `windows::core::Result`, not `CResult` — use `let _ =` or map errors
- `PlaceholderFile::new()` takes `impl AsRef<Path>`, not `&str` directly
- `SyncRootInfo` requires `.with_icon()` — panics without it

**Decision:** Proceed with `cloud-filter` crate for Phase 1. Plan to fork if maintenance becomes an issue.

Spike code: `spikes/cf-spike/`

### Phase 0.1 spike — Subdirectories, staleness, proactive push

**Subdirectory navigation: PASSED.**
- `FETCH_PLACEHOLDERS` fires for each directory as the user navigates deeper
- Relative path derivation works (strip client root from absolute request path)

**Staleness / re-enumeration: FAILED (expected).**
- Explorer fully caches populated folders
- Neither closing/reopening the folder nor F5 re-triggers `FETCH_PLACEHOLDERS`
- `PopulationType::Full` means "ask once, cache forever" from Explorer's perspective

**Proactive placeholder push: PASSED.**
- `PlaceholderFile::new(name).create(parent_dir)` successfully injects a new placeholder into an already-open folder
- Explorer picks it up immediately — no refresh needed
- This is the mechanism for live updates: diff server state, push new placeholders

**Key architecture decision: write-through required from Phase 1.**
- NAS is the source of truth — there are no "local only" files
- Any file saved into the sync root must go to the NAS immediately
- Flow: app saves file → `state_changed` detects it → upload via SMB → `convert_to_placeholder()`
- Delete/rename callbacks propagate to NAS directly
- Phased plan restructured: write-back moved from Phase 4 to Phase 1

**Revised enumeration strategy:**
- Initial folder open: `FETCH_PLACEHOLDERS` handles it
- While folder is open: lightweight periodic `readdir` on active folders only (tracked via `opened`/`closed` callbacks), diff and push via `PlaceholderFile::create()`
- No filesystem watchers on the server side — just periodic SMB metadata polls on open folders

### Explorer caching and `opened` callback investigation

**Problem:** Explorer fully caches populated folders. Neither F5, re-navigation,
nor closing/reopening triggers `FETCH_PLACEHOLDERS` again. The `opened` CF API
callback never fired at all in testing — likely only fires for placeholder file
access, not directory navigation.

**Attempted approaches that failed:**
- F5 refresh — no callback
- Navigate away and back — no callback
- `opened` callback with debounce — callback never fires for directories
- Recursive tree sync — works but O(n) on entire tree, non-starter for millions of files

**Solution: SMB `CHANGE_NOTIFY` via `ReadDirectoryChangesW`**

Tested `ReadDirectoryChangesW` against `\\192.168.1.170\test1` — works perfectly.
Real-time events for file add, remove, rename, modify. Subtree watching supported.
This is the same mechanism Windows Explorer uses for live updates on mapped drives.

Synology responds to SMB2 CHANGE_NOTIFY correctly. Events include:
- `ADDED` / `REMOVED` / `MODIFIED` / `RENAMED_OLD` + `RENAMED_NEW`
- Synology internal noise: `@eaDir`, `#recycle` — filter by prefix

**Architecture decision:** Use `ReadDirectoryChangesW` (Windows) and `FSEvents` (macOS)
to watch NAS folders the user has visited. On change event, push/remove placeholders
directly. No polling, no scanning, no tree walking. Event-driven, per-folder, server-side
notification — exactly how Explorer/Finder work natively.

Spike code: `spikes/cf-spike/src/smb_watch_test.rs`

### Edge case discussions (2026-04-07)

**1. File locking / concurrent access**

When a file is open locally (any app), we must not dehydrate or modify it even if
the NAS version changes. Strategy:

- Track open handles in-memory via `opened`/`closed` CF API callbacks: `HashMap<PathBuf, u32>` refcount
- On NAS MODIFIED event: if refcount > 0, defer the update to a pending queue
- When `closed` fires and refcount hits 0, apply pending NAS updates (update metadata + dehydrate)
- Conservative: we don't know read vs write access mode (CF API doesn't expose it),
  so we always defer. Slightly over-cautious but safe.
- macOS: FileProvider system handles this implicitly — `modifyItem` won't fire while
  an app has the file open for writing

**2. Partial writes / large file uploads**

Atomic writes via temp file: write to `.filename.~sync.{hostname}`, rename to final name
on success. If write fails, delete temp. Rename is atomic on SMB within same directory.

Rapid saves (manic revisions): debounce `state_changed` with a 3-second timer per path.
Multiple saves within the window collapse into one upload. If a save arrives during an
active upload, cancel the upload, delete temp, restart debounce. Queue-based — not
parallel debounces.

Resumable writes (Phase 2): chunked write with offset tracking. On reconnect, seek to
last successful offset in the temp file, continue. Checksum verification after resume.

**3. Multi-user write conflicts**

Default: last-write-wins (same as mapped drive behavior). Lightweight conflict detection
before the final rename:

- Record NAS mtime + size before upload starts
- After upload, before rename: stat the target file again
- If mtime AND size match recorded values → no conflict, rename
- If size matches our upload and mtime within 2s → Synology granularity / echo, rename
- Otherwise → genuine conflict, save as `filename.conflict.{hostname}.{timestamp}`
  and notify user

Suppress false positives from own writes: tag paths as "just wrote this" with a 5-second
TTL. Skip conflict detection for watcher events on those paths.

One extra SMB stat call per upload — negligible cost. No coordination between machines,
no locking beyond what SMB provides natively.

**9. Rapid file creation (render farm floods)**

Two-tier strategy based on watcher buffer state:

- Normal pace: batch incoming events, push via `BatchCreate` every 500ms
- Overflow (0 bytes returned): fall back to `readdir` + diff for that folder,
  then re-register the watch

On sustained floods (render farm): don't thrash between event/readdir modes.
After overflow, poll via readdir every 5 seconds until two consecutive diffs
show no changes, then re-register event watch. Cheap polling during storms,
real-time when calm.

Only affects folders the user has navigated to (watched). Five render jobs in
five folders but user has one open = one folder in flood mode. Others picked up
via `FETCH_PLACEHOLDERS` when user navigates there.

Core principle: events are a latency optimization, readdir + diff is the
correctness backstop. Losing events degrades latency, never correctness.

**8. File name conflicts (simultaneous local + remote creation)**

Self-resolving. If a local file exists, `PlaceholderFile::create()` fails silently.
The local file proceeds through write-through (upload to NAS → convert to placeholder).
Last-write-wins applies. Genuine conflicts caught by the mtime + size check before
rename (see #3 above).

**7. Deep paths / MAX_PATH**

Non-issue. Long paths enabled via registry on team machines. UNC paths + Rust's wide
API + CF API's internal `\\?\` prefix all support 32K+ characters. Any path length
failure would also break a regular mapped drive — not our problem to solve.

**6. Permission / ACL changes**

Don't mirror NAS permissions onto placeholders — let SMB handle it at operation time.
If a write/read fails due to permissions, the SMB error surfaces naturally.

Phase 2: surface errors in UI via mount status in tray/UFB app. Not a blocking popup —
a persistent "last error" field on the mount (e.g., "Access denied — scene_v2.nk, 10:32 AM").
Errors age out after a few minutes. Covers all SMB errors (permissions, disk full, path too
long, timeout) with one pattern. Passed back via existing `AgentToUfb` IPC messages.

**5. Symlinks and junction points**

Follow links transparently. `std::fs::metadata()` follows symlinks by default,
SMB resolves them server-side. We never see the link — just the target content.
No special code needed. If a link is broken or circular, `metadata()` fails and
we skip the entry (already handled by error pattern in the spike).

This means symlinks appear as regular files/folders to the user. Acceptable
trade-off — avoids all cross-platform symlink complexity.

**4. Offline behavior**

Simple policy: if the NAS is down, the mount is down — except for hydrated cache.

- Hydrated files: still accessible (real local files, no network needed)
- Dehydrated files: fail with clear error ("NAS unreachable")
- Writes: fail with clear error (no queueing, no silent local-only files)
- UI: mount shows offline status

No write queueing, no divergent state, no surprises. Same UX as a disconnected
mapped drive.

On reconnect:
- Re-register SMB watches
- Re-enumerate visited folders to catch up
- Mount flips back to online in UI

Connectivity detection: periodic SMB stat heartbeat (~30s interval).

### App integration notes

**Hydration status overlay icons:**
No extra queries needed. `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` (dehydrated) and
`FILE_ATTRIBUTE_PINNED` come back from standard `readdir` — already in the
`WIN32_FIND_DATA` struct. macOS: materialization state via standard file metadata.

Show hydration status in grid view only (checkmark overlay on thumbnails).
Tie into existing thumbnail generation process — check attribute during the same
per-file pass. List view doesn't need it.

**Thumbnail hydration flooding:**
Grid view thumbnail generation reads full files — triggers hydration for every visible
file. Existing thumbnail queue with concurrency limiting handles this. No sync layer
changes needed — throttling is in the thumbnail pipeline (2-3 concurrent, prioritize
visible, cancel on scroll-away).

**Dual browser drag/drop and file operations:**
App uses `fs_extra` for copy/move — reads file bytes directly, triggers hydration
transparently. Works correctly but can be optimized:

- Move within sync root: `fs::rename` instead of fs_extra (instant, no hydration)
- Copy within sync root: `CfCreatePlaceholders` at destination with same identity blob
  (instant, no hydration — destination is a dehydrated placeholder pointing to same NAS file)
- Copy between sync roots: same — create placeholder with NAS blob (instant)
- Copy from sync root to regular folder: must hydrate (unavoidable, same speed as direct
  SMB copy). Bonus: source file is now cached locally after the copy.

Copy/move commands need sync-awareness check: is source/dest in a sync root?
Route to placeholder operations when possible, fall back to fs_extra when not.

**Bulk drag/copy flooding:**
- Copy out of sync root: Explorer manages hydration sequentially with copy dialog. We just respond to FETCH_DATA.
- Copy into sync root: no hydration, write-through queue handles it.
- Move within sync root: metadata only, no hydration (rename callback).
- External apps (indexer, AV): user's config issue, same as OneDrive.
No special flood protection needed beyond existing mechanisms.

### Phase 1 integration planning (2026-04-08)

**Mount mode: mutually exclusive.**
A mount is either a traditional drive-letter mount OR an on-demand sync root. Not both.
Single toggle in Settings → Mounts editor switches between modes. Avoids dual-path
confusion and unnecessary hydration from copies between the same NAS via different paths.

**Config changes needed:**
- `MountConfig` gets `sync_enabled: bool` and optional `sync_root_path` override
- Default sync root: `%LOCALAPPDATA%\ufb\sync\{mount_id}\`
- `enabled` (drive mount) and `sync_enabled` are mutually exclusive — UI enforces this
- Frontend `MountConfig` TypeScript interface mirrors these fields

**Critical pre-existing bug to fix:**
`mount_service.rs` `apply_config()` skips existing mounts entirely (line 72).
Field changes (including toggling sync) are ignored until agent restart.
Must detect config changes on existing mounts and send events to orchestrators.

**Orphaned sync root cleanup:**
Cloud Files registrations persist across reboots. On startup, attempt unregister
before register, or reconnect to existing registration if path unchanged.

**SMB session without drive letter:**
If sync is enabled but drive mount isn't, agent still needs authenticated SMB session
for the watcher. Need a headless auth path (WNetAddConnection2W with null local name).

**State machine:**
Sync runs as a parallel sub-state on the orchestrator, not new top-level states.
SyncState: Disabled | Registering | Active | Error | Deregistering.
MountStateUpdateMsg extended with optional sync_state fields (backward compatible).

**Tray:**
Sync status shown on the mount's status line (e.g., "primary-nas ● Sync: Active").
Not a separate tray entry — sync is a property of the mount.

**UI toggle:**
Per-mount in Settings → Mounts editor, after drive letter section. Windows only.
Switching modes: save config → agent picks up change → orchestrator tears down old
mode and starts new one.

**Project folder detection:**
No changes needed. `isJobsFolder` flag on mount config works as-is — sync root
placeholders are real files/folders, existing detection logic applies.

**Path mappings:**
No special handling. Sync root paths are just local paths that map to NAS paths.
One more row in the existing path mapping table. Mount config already stores both
the NAS path and local mount point.

### Phase 1 — Agent integration (2026-04-08)

**Completed: full agent-side sync lifecycle.**

Changes made to mediamount-agent:
- `config.rs`: `sync_enabled`, `sync_root_path`, `sync_cache_limit_bytes` fields.
  `PartialEq` derived. `is_sync_mode()`, `sync_root_dir()`, `mount_path()` updated.
  Sync and drive mount mutually exclusive.
- `state.rs`: `SyncState` enum (Disabled/Registering/Active/Error/Deregistering).
  `ConfigChanged` event added with transition logic.
- `messages.rs`: Optional `sync_state`/`sync_state_detail` on MountStateUpdateMsg.
- `mount_service.rs`: `apply_config()` detects field changes on existing mounts,
  sends ConfigChanged events. (Pre-existing bug fixed.)
- `orchestrator.rs`: `sync_root: Option<SyncRoot>` + `sync_state: SyncState` fields.
  `start_sync()`/`stop_sync()` methods. `dispatch_effect()` branches on sync mode.
  `emit_state_update()` includes sync state.
- `platform/windows/fallback.rs`: `establish_smb_session()`/`disconnect_smb_session()`
  for deviceless WNet connections.
- `sync/mod.rs`, `sync/filter.rs`, `sync/watcher.rs`, `sync/sync_root.rs`: Full sync module.

**Tested end-to-end against \\192.168.40.100\test1:**
- Sync root registers and connects alongside existing drive mounts (G:, F:, V:)
- Placeholder population works (7ms for root, 11ms for subfolder)
- File hydration works (~26ms for small files)
- Live watcher detects add/rename/delete on NAS in real time
- Rename handling: RENAMED_OLD → remove placeholder, RENAMED_NEW → create placeholder

**Remaining for Phase 1:**
- Write-through (see below)
- Frontend UI (sync toggle in mount editor, sync status in sidebar)
- Credential handling for mounts with explicit credential keys

### Write-through design discussion (2026-04-08)

**Key insight: the local write IS the hydration.**
When a user saves a file, the data landing locally becomes the cache. We upload
to NAS in the background, then convert to placeholder. No round-trip. Same total
data transfer as saving directly to a mapped drive.

**state_changed callback is NOT usable for write detection.**
The cloud-filter crate's state_changed only watches FILE_NOTIFY_CHANGE_ATTRIBUTES
(pin/unpin detection). It does NOT fire for new files. We need our own client-side
ReadDirectoryChangesW watcher on the local sync root.

**File completion detection: quiescence, not locking.**
Rejected: exclusive file locking (try-open) — dangerous, can cause lock contention
with the app that's writing. Instead: watch for MODIFIED events to stop. 3-second
debounce after last MODIFIED = file is done. If wrong, upload cancels and restarts.

**Rapid save handling: cancel and restart.**
State machine per path: IDLE → DEBOUNCING → UPLOADING → CONVERTING → IDLE.
New MODIFIED at any stage resets to DEBOUNCING. Active upload cancelled via oneshot
channel, checked between 4MB chunk writes. Worst case waste: one 4MB chunk.

**Read-only sync root rejected as option.**
Discussed making sync root read-only (writes go to UNC path). Rejected because:
the sync root IS the mount — users need a single path for browse + read + write.
The CF API requires a local NTFS folder for the sync root, and the placeholders
must be what users interact with.

**Threading: coordinator (async) + worker (blocking).**
Upload coordinator lives on tokio runtime (manages timers, state, decisions).
Upload worker is a dedicated OS thread (blocking SMB writes). Client watcher is
another OS thread (blocking ReadDirectoryChangesW). Clean separation: decisions
are async, I/O is blocking on dedicated threads.

**Scale validated:**
- 200 files: ~50KB tracking state, ~40s upload at 10GbE
- 10K files: ~2.5MB tracking state, sequential queue
- 100K files: ~25MB tracking state, still manageable

**Implementation: 9 steps, each compiles independently.**
See docs/nas-sync-plan.md for full write-through architecture.

### Phase 0.3 — Live sync + chunked hydration

**Live change detection: PASSED.**
- `ReadDirectoryChangesW` on NAS root with subtree watching
- Background thread receives events, looks up which client folders are "visited"
  (registered during `FETCH_PLACEHOLDERS`), pushes/removes placeholders
- Files added/removed on NAS appear/disappear in Explorer in real time
- Synology noise (`@eaDir`, `#recycle`) filtered by path component prefix

**Chunked streaming hydration: PASSED.**
- 4MB chunks via `ticket.write_at()` at progressive offsets
- `ticket.report_progress()` feeds Explorer's progress UI
- 94MB file over 10GbE hydrates successfully with progress feedback
- Second access is instant (served from local cache)

**Cache persistence note:**
Placeholders and hydrated files persist across sessions — they're real NTFS files.
The spike destroys them on startup for clean testing. In production, register once
and just reconnect the filter on subsequent launches.

**Open question: hydration UX, caching, and eviction policies.**
Currently shows a file copy dialog (standard Windows progress dialog).
This is the default for `HydrationType::Full` without `StreamingAllowed`.

`HydrationPolicy` flags to evaluate in Phase 1:
- `StreamingAllowed` — may enable toast-style notifications for background hydrations
  instead of the blocking copy dialog. Windows decides foreground vs background style.
- `AutoDehydrationAllowed` — lets OS automatically evict cached files when disk space
  is low. Critical for VFX workflows with large files.
- `AllowFullRestartHydration` — restart from scratch if hydration is interrupted.

These tie together: eviction policy determines how aggressively the OS reclaims cache
space, hydration policy determines the UX when re-fetching evicted files, and streaming
policy affects whether the user sees a blocking dialog or a non-blocking toast.

For VFX (large video files, limited local SSD): aggressive auto-dehydration + streaming
+ toast notifications for background re-hydration is likely the right combination.
Needs hands-on testing with real workflow patterns.

### Write-through implementation (2026-04-08)

**Implemented: full write-through pipeline.**

New module: `sync/write_through/` with three files:
- `mod.rs` — EchoSuppressor, WriteThrough struct, async coordinator, placeholder conversion
- `client_watcher.rs` — ReadDirectoryChangesW on local sync root, filters placeholders/temps
- `worker.rs` — blocking upload thread, 4MB chunked SMB write, conflict detection

**Architecture (as planned):**
```
Client watcher (blocking thread) → ClientFsEvent (mpsc, 256)
  → Upload coordinator (async tokio task) → UploadJob (std::sync::mpsc)
    → Upload worker (blocking thread) → UploadResult (mpsc, 64)
      → Placeholder conversion (spawn_blocking)
```

**Key implementation details:**
- Client watcher uses `GetFileAttributesW` + `FILE_ATTRIBUTE_REPARSE_POINT` to distinguish
  regular files from CF placeholders — only regular files trigger uploads
- Filters: `.~sync.` temp files, `~$` Office locks, `.tmp` files, `#`/`@` Synology noise
- Per-path state machine: IDLE → DEBOUNCING(3s) → UPLOADING → placeholder conversion → IDLE
- New MODIFIED event at any stage cancels active upload and resets to DEBOUNCING
- Upload worker: writes to `.filename.~sync.HOSTNAME` temp file, conflict check via
  pre/post mtime+size comparison, rename to final on success
- Conflict files saved as `filename.conflict.HOSTNAME.TIMESTAMP`
- Echo suppression: 5-second TTL HashSet shared between write-through and NAS watcher
- NAS watcher checks echo suppressor before creating placeholders (prevents duplicates)
- Placeholder conversion via `cloud_filter::placeholder::Placeholder::convert_to_placeholder()`
  with `ConvertOptions::default().mark_in_sync().blob(nas_path_bytes)`
- Startup recovery: cleans orphaned `.~sync.HOSTNAME` temp files on NAS, queues
  non-placeholder files in sync root for upload
- Clean shutdown: `CancelIoEx` on client watcher handle (stored as `AtomicUsize` for
  Send safety — HANDLE wraps raw pointer which is `!Send`)

**HANDLE !Send solution:**
Windows `HANDLE` is `!Send` because it wraps `*mut c_void`. Stored the handle as
`AtomicUsize` instead of `Arc<Mutex<HANDLE>>`. HANDLEs are just opaque kernel IDs —
safe to use from any thread. This keeps `WriteThrough` Send+Sync without unsafe impls.

**Wired into existing code:**
- `sync_root.rs`: Creates `EchoSuppressor` and `WriteThrough`, stops write-through
  before CF session disconnect on teardown
- `filter.rs`: `NasSyncFilter::new()` accepts `Arc<EchoSuppressor>`, passes to NasWatcher
- `watcher.rs`: All placeholder-creating paths check `echo.is_suppressed()` first

**Status: tested end-to-end against \\192.168.40.100\test1. All core flows working.**

### Write-through testing (2026-04-08)

**Tested against \\192.168.40.100\test1. Bugs found and fixed during testing:**

1. **Startup recovery path mapping (fixed):** `queue_non_placeholders` used the recursion
   directory for `strip_prefix` instead of the original client root. Nested files got
   mapped to wrong NAS paths (e.g., `project_files/scene_01.txt` → `\\test1\scene_01.txt`).
   Fix: pass `client_root` separately from `scan_dir` through recursion.

2. **Cancel-on-closed oneshot (fixed):** Startup recovery creates `UploadJob` with
   `oneshot::channel()` but drops the sender immediately. Worker treated `Closed` as
   cancellation (`Ok(()) | Err(Closed) => cancel`). Fix: only treat explicit `Ok(())`
   as cancel. `Closed` means the coordinator isn't tracking the job — not a cancel.

3. **Placeholder::open() invalid handle (fixed):** `Placeholder::open()` uses
   `CfOpenFileWithOplock` which doesn't work for regular (non-placeholder) files.
   Fix: use `std::fs::File::open()` → `Placeholder::from(file)` which wraps a standard
   Win32 handle. `CfConvertToPlaceholder` works with either handle type.

4. **Placeholder modification detection (fixed):** Client watcher filtered by
   `FILE_ATTRIBUTE_REPARSE_POINT` — skipped placeholders entirely. When a user edits a
   hydrated placeholder, the file stays as a reparse point, so modifications were missed.
   Fix: detect modifications on ALL files (not just regular files). Added conversion
   suppression: coordinator tracks recently-converted paths and ignores the first event
   after conversion (one-shot, not time-based) to prevent feedback loops.

5. **Double-convert error (fixed):** `CfConvertToPlaceholder` on an existing placeholder
   returns `0x8007017C` ("cloud operation is invalid"). Fix: check reparse point attribute
   first. If already a placeholder, call `Placeholder::mark_in_sync()` instead of
   `convert_to_placeholder()`.

6. **Conversion suppression window (fixed):** Initially used 5-second time window — too
   long, suppressed legitimate user saves within 5s of a conversion. Fix: one-shot
   suppression (remove entry after first event caught) + reduced window to 2s.

**Test results (all passing):**
- New file creation → uploaded → converted to placeholder ✓
- New file in subdirectory → correct NAS path mapping ✓
- Placeholder modification → detected → uploaded → re-synced ✓
- Rapid saves (3 saves in 2s) → debounced → only final content uploaded ✓
- Startup recovery → non-placeholder files uploaded and converted ✓
- Echo suppression → NAS watcher doesn't duplicate after upload ✓
- Performance: ~20-30ms per small file upload over 10GbE

### Phase 2 design note: upload worker pool (2026-04-08)

Current write-through uses a single upload worker thread. At 25MB+ per file (typical
for this workflow), 1000 queued files would take ~3-4 minutes to drain.

**Plan: 3-4 concurrent worker threads**, matching Synology Drive Client's approach.
- 3-4 workers saturates NAS disk I/O without overwhelming spinning disks
- Switch coordinator→worker channel to multi-consumer (crossbeam-channel or
  `Arc<Mutex<std::sync::mpsc::Receiver>>`)
- Workers share the same EchoSuppressor, send results to same result_tx
- Coordinator doesn't care which worker handles which job

**Prioritization: small files first.**
Synology Drive does this — clears queue count quickly, gives visible progress.
Large files block a worker for longer; interleaving with small files on other
workers keeps throughput smooth. Implementation: two queues (small/large threshold,
e.g., 10MB) or a priority channel, rather than plain FIFO.

Not needed for Phase 1 — single worker handles the interactive "save → see on NAS
in 3s" use case. Becomes important for bulk operations (paste 1000 files, render
farm output).

### Phase 1 complete (2026-04-09)

**All Phase 1 items delivered and tested on Windows:**

- Write-through: client watcher → debounce → upload worker → placeholder conversion
- Frontend UI: sync toggle in Settings, sync status in sidebar, navigation to sync root
- Credential handling: mediamount_ prefix, empty key skip, 1219 conflict reuse
- Sync root path: `C:\Volumes\ufb\{shareName}` (extracted from NAS path)
- Shell integration: custom cloud-sync.ico for Explorer folder icon
- Agent shutdown: orchestrator loop exit fix + NAS watcher CancelIoEx + thread join timeouts

**10 bugs found and fixed during testing** (see project memory for full list).
Key lessons for macOS implementation below.

---

## Lessons from Windows Phase 1 — hints for macOS

These are patterns and pitfalls discovered during Windows implementation that the
macOS FileProvider implementation should anticipate.

### 1. Write detection for modified placeholders
On Windows, CF API placeholders are NTFS reparse points. When a user edits a hydrated
placeholder, the file stays as a reparse point — the original client watcher filtered
these out and missed modifications. Fix: detect changes on ALL files (not just new ones),
then suppress events from our own conversion operations (one-shot suppression per path).

**macOS equivalent:** FileProvider's `modifyItem` callback handles this natively —
the system tells you when a materialized file is modified. No client-side watcher needed
for modification detection. But write-through upload + itemVersion update still needed.

### 2. Placeholder conversion API
`CfConvertToPlaceholder` requires a standard Win32 file handle (via `std::fs::File`),
NOT a CF API handle (`CfOpenFileWithOplock`). For files that are already placeholders,
use `Placeholder::mark_in_sync()` instead — `convert_to_placeholder` returns
`ERROR_CLOUD_OPERATION_INVALID` (0x8007017C) on existing placeholders.

**macOS equivalent:** FileProvider handles materialization state internally. After
uploading, signal completion via the `NSFileProviderManager` completion handler.
No manual file conversion needed.

### 3. Shutdown and thread cleanup
Blocking Win32 calls (`ReadDirectoryChangesW`, `WNetAddConnection2W`) need explicit
cancellation via `CancelIoEx`. Without it, threads hang indefinitely and the agent
won't quit. Thread joins need timeouts (3s) because NAS may be unreachable.

**macOS equivalent:** `FSEvents` streams need `FSEventStreamStop` + `FSEventStreamInvalidate`
on shutdown. NSFileProviderManager operations should be cancelled via invalidation.
Dispatch queues need explicit cleanup.

### 4. Orchestrator event loop exit
The orchestrator held its own `event_tx` (sender), keeping the mpsc channel alive even
after the mount service dropped its sender. `event_rx.recv()` never returned `None`.
Fix: break the loop explicitly when state reaches `Stopped`.

**macOS equivalent:** Same Rust orchestrator code is shared. This fix applies to all platforms.

### 5. Credential handling
Windows Credential Store keys use a `mediamount_` prefix (set by the Tauri app).
The agent must match this prefix when reading. Empty credential keys should skip
lookup entirely (not call CredReadW with empty string). Multiple mounts to the same
NAS server cause SMB credential conflicts (ERROR 1219) — retry without credentials
to reuse the existing session.

**macOS equivalent:** Keychain Access uses service name + account name. The macOS
credential store implementation should use the same `mediamount_` prefix convention.
SMB session sharing on macOS is handled by the kernel — one mount per share path.

### 6. Echo suppression for write-through
After uploading to NAS, the NAS watcher sees the new/modified file and tries to
create/update a placeholder. Suppressed via a shared HashMap<PathBuf, Instant> with
5-second TTL. Upload worker writes to it, NAS watcher reads.

**macOS equivalent:** After upload via FileProvider, signal the enumerator via
`NSFileProviderManager.signalEnumerator(for:)`. The system re-enumerates and sees
the file is already materialized. May not need explicit echo suppression if the
FileProvider system handles this, but test carefully.

### 7. Sync root path
Windows: `C:\Volumes\ufb\{shareName}` — visible in Explorer, easy to find.
Share name extracted from NAS path (e.g., `\\192.168.40.100\test1` → `test1`).
Created automatically via `fs::create_dir_all`.

**macOS equivalent:** FileProvider domains appear under `~/Library/CloudStorage/`
automatically. The display name is set via `NSFileProviderDomain`. Users see it in
Finder sidebar. No manual path creation needed.

### 8. Icon handling
CF API sync root icon must be set during registration via `SyncRootInfo::with_icon()`.
Format: `path_to_file.ico` or `path_to_dll,index`. The exe's embedded icon works
(`exe_path,0`). Custom .ico files work directly. Icon must exist at registration time.

**macOS equivalent:** FileProvider domain icon set via `NSFileProviderDomain` configuration
or the extension's Info.plist. Standard macOS icon handling (NSImage, asset catalogs).

### 9. File structure for macOS reference
```
mediamount-agent/src/sync/
  mod.rs              — module def, Windows-only gates (#[cfg(windows)])
  filter.rs           — CF API SyncFilter impl (Windows)
  watcher.rs          — NAS watcher via ReadDirectoryChangesW (Windows)
  sync_root.rs        — Sync root lifecycle (Windows)
  write_through/
    mod.rs            — Coordinator, echo suppressor, conversion (Windows)
    client_watcher.rs — Local fs watcher via ReadDirectoryChangesW (Windows)
    worker.rs         — Upload worker (CROSS-PLATFORM — pure std::fs)

Parts reusable on macOS:
  - worker.rs: upload logic (temp file, conflict detection, chunked write) is pure Rust/std::fs
  - EchoSuppressor: platform-agnostic HashMap<PathBuf, Instant>
  - Coordinator async logic: debounce timers, state machine, per-path tracking
  - Config fields: sync_enabled, sync_root_path, share_name() extraction

Parts Windows-only (need macOS equivalents):
  - filter.rs → NSFileProviderReplicatedExtension methods
  - watcher.rs → FSEvents stream on NAS mount
  - client_watcher.rs → FileProvider modifyItem callback (system handles detection)
  - sync_root.rs → NSFileProviderDomain registration
  - Placeholder conversion → FileProvider materialization/completion handlers
```

### 10. NAS watcher: 0-byte placeholders during batch copy (2026-04-09)
When files are copied to the NAS in a batch, Synology sends ADDED events sequentially.
The first file in the batch may still be 0 bytes when the second file's ADDED fires.
Synology does NOT reliably send MODIFIED after the copy completes for all files.

**Solution: belt-and-suspenders.**
- Added `FILE_NOTIFY_CHANGE_LAST_WRITE` to the watcher filter (catches some MODIFIED events)
- MODIFIED handler does delete+recreate (not CfUpdatePlaceholder, which silently fails on 0-byte placeholders via Win32 handles)
- Deferred fallback: spawns a thread that polls the NAS file at increasing intervals (2s, 3s, 5s, 10s, 15s, 30s), delete+recreate when non-zero
- Whichever mechanism fires first (MODIFIED event or deferred poll) fixes the placeholder

**Key finding: `CfUpdatePlaceholder` silently fails on 0-byte placeholders.**
The API returns Ok but the size doesn't change in Explorer. Delete+recreate (`fs::remove_file` + `PlaceholderFile::create`) is the reliable approach.

**macOS equivalent:** FileProvider handles this differently — `signalEnumerator` triggers
re-enumeration, and the system fetches fresh metadata. No 0-byte race condition because
FileProvider's `enumerateItems` is called by the system, not triggered by individual events.

### 11. Echo suppression for deletes (2026-04-09)
When a user deletes from the sync root, the CF API delete callback removes the NAS file.
The NAS watcher then sees REMOVED and tries to remove the local placeholder — but the
CF API is still processing the deletion. This race causes Explorer to show "file not found"
dialogs repeatedly.

Fix: the CF API delete callback suppresses the NAS path via EchoSuppressor before deleting.
The NAS watcher checks suppression on REMOVED events (same as it does for ADDED).

**macOS equivalent:** FileProvider's `deleteItem` callback handles both sides. The system
manages the local materialization state. No watcher race condition expected.

### 12. NAS watcher trailing backslash on UNC root (2026-04-09)
`Path::parent()` on `\\server\share\file` returns `\\server\share\` (with trailing backslash).
But the watched folder map stores `\\server\share` (no trailing backslash) from FETCH_PLACEHOLDERS.
Root-level file events never matched — all watcher events for root files were silently dropped.

Fix: normalize parent path by stripping trailing separator before map lookup.

**macOS equivalent:** Not applicable — FSEvents uses forward-slash paths without trailing
separator ambiguity.

### 13. Upload worker pool (2026-04-09)
Upgraded from single upload worker thread to 3 concurrent workers via `crossbeam-channel`.
Workers share a single multi-consumer receiver. The coordinator dispatches jobs to whichever
worker is free. All workers share the same EchoSuppressor and result_tx.

**macOS equivalent:** Same architecture applies — the worker pool is pure Rust/crossbeam,
platform-agnostic. Only the coordinator→worker channel type changed.

### 14. Credential session sharing (2026-04-09)
Multiple mounts to the same NAS server caused SMB credential conflicts (ERROR 1219) even
with the same credentials, because Windows treats each WNetAddConnection2W call with
explicit credentials as a new session attempt.

Fix: shared `Arc<Mutex<HashSet<String>>>` of connected server hostnames across all
orchestrators. First mount to a server authenticates; subsequent mounts skip credential
lookup and pass null credentials to reuse the existing session.

Also handle the 85→1219 sequence: drive letter already assigned → disconnect → retry with
credentials → credential conflict → retry with null credentials to reuse session.

**macOS equivalent:** macOS kernel handles SMB session sharing automatically. No explicit
tracking needed.

### 15. System file type icons (2026-04-09)
Added OS-native file type icons to the Tauri app's file browser (both list and grid views).
- Backend: `system_icons.rs` — `SHGetFileInfoW` with `SHGFI_USEFILEATTRIBUTES` → HICON → BGRA → RGBA → PNG
- Frontend: `systemIconCache.ts` (deduped Promise cache by extension) + `FileTypeIcon` component
- Priority: thumbnail (grid only) → system icon → Material Symbols fallback
- Folder icons supported via `FILE_ATTRIBUTE_DIRECTORY` flag
- Backend caches by extension in `RwLock<HashMap>`

**macOS equivalent:** `NSWorkspace.icon(forFileType:)` → NSImage → PNG. Same frontend
cache and component. Backend needs `objc2` bindings for NSWorkspace/NSImage.

### 16. Drag-drop freeze fix (2026-04-09)
Windows `DoDragDrop()` is a blocking call that pumps its own message loop. Calling it from
an `async fn` blocked the entire Tauri async runtime, freezing all system drag operations.

Fix: dispatch to main UI thread via `app.run_on_main_thread()` (same pattern as macOS).
`DoDragDrop` runs safely on the main thread because it pumps its own message loop.
`spawn_blocking` didn't work because `OleInitialize` uses per-process `Once` — the
blocking thread pool doesn't have OLE initialized.

**macOS equivalent:** Already uses `app.run_on_main_thread()` — no issue on macOS.

### Cache eviction design (2026-04-09)

**Goal:** Per-mount configurable cache budget with LRU eviction.

**Architecture:** SQLite DB per mount (`{sync_root}/.cache_index.db`) tracks hydrated files
(path, size, last-access timestamp). After each hydration: check budget → evict LRU to 80%
of limit. Dehydration via `cloud_filter::ext::file::FileExt::dehydrate(..)` which calls
`CfDehydratePlaceholder`. Skip files with open handles. Rebuild DB from filesystem attributes
on startup if missing.

**Manual clear:** "Clear Cache" button in Settings — DB-driven, no filesystem walk. SELECT all
paths from cache_index, DELETE all rows, dehydrate each file. Fast and targeted.

**Key decisions:**
- Evict after each hydration (no timer). Simple and immediate.
- Evict to 80% of limit (not exactly 100%) to avoid thrashing with large VFX files.
- DB per-mount (not shared) — natural partitioning, clear = DELETE FROM cache_index.
- `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` check for rebuild — cheap, no file opens.
- OS auto-dehydration (`AutoDehydrationAllowed`) remains as a safety net independent of our budget.

**Manual clear:** Two levels — per-mount "Clear Cache" button and global "Clear All Cache".
Both DB-driven (SELECT paths, DELETE all, dehydrate each — no filesystem walk needed).
Modal progress UI blocks interaction during clear, shows file count progress.

**Damaged DB self-heal:** On corruption (SQLITE_CORRUPT, SQLITE_NOTADB, integrity_check fail):
delete DB → create fresh → dehydrate ALL hydrated files (nuclear but safe, can't trust stale
tracking) → rebuild empty index. Automatic on startup. Emits progress events to frontend
for a "Repairing cache..." modal. No user intervention needed.

**macOS equivalent:** FileProvider manages eviction via `NSFileProviderManager.evictItem()`.
Same SQLite cache index can track hydrated items. The eviction policy (LRU, budget) is
cross-platform logic in Rust.

---

## Unified Symlink Mount Architecture (2026-04-09)

Major refactor planned: unify all Windows mounts behind symlinks to `C:\Volumes\ufb\{name}\`.

### Problem
- Sync root re-registers on every restart, destroying placeholders
- No startup reconciliation — offline changes lost
- Drive letter mounts vs sync mounts are two different systems
- Users can write into sync root while offline, creating ambiguous orphans
- Drive hiding is a messy registry hack

### Architecture
```
User-visible:  C:\Volumes\ufb\{shareName}\ (symlink)
  Drive mount: symlink → \\server\share (UNC)
  Sync mount:  symlink → {cacheRoot}\sync\{shareName}\ (CF API root, hidden)

Cache root:    %LOCALAPPDATA%\ufb\ (default) or user-selected (e.g., D:\ufb-cache\)
Cache DB:      {cacheRoot}\cache\{mountId}.db
```

### Key decisions
- **Fully replace drive letters** — all mounts at C:\Volumes\ufb\. No drive letters.
- **PowerShell RunAs for elevation** — same pattern as existing hide_drives.
  Two-tier: silent if Dev Mode, "Connect Mounts" button if not.
- **Global cache location** in main Settings. One root for all sync mounts.
- **Leave symlinks on crash** — reconcile on next start. User can browse cached files while agent is down.
- **Sync root reconnect without re-register** — try Session::connect() first, register only if new.
  Placeholders survive agent restarts.

### Startup reconciliation (DB-driven)
- DB stores visited folders with folder mtime.
- On startup: SMB stat each visited folder's mtime → skip unchanged → readdir changed folders.
- Three-way diff: DB (known state) vs NAS (current) vs Local (current).
- NAS is truth. DB is our snapshot. Local wins only for verified newer saves.
- Files not in DB discovered organically through browsing (FETCH_PLACEHOLDERS).

### Orphan quarantine
Files that don't fit reconciliation logic → quarantined to `\\server\share\.orphaned\`:
```
.orphaned\
  project_files\
    scene_v2.nk                         (first occurrence, original name)
    scene_v2.orphaned.2026-04-10.nk     (duplicate, timestamped)
```
Threshold: >10 orphans in one reconciliation → quarantine ALL (batch confusion).
UI shows notification: "N files quarantined". Browsable mirror of original path structure.

### Elevation strategy
```
Agent starts → try CreateSymbolicLinkW silently
  → Works (Dev Mode): all mounts connected
  → Fails: set needs_elevation flag → sidebar shows "Connect Mounts" button
  → User clicks → single UAC prompt → all symlinks created
```

### DB schema additions
```sql
-- Visited folders (for reconciliation scope)
CREATE TABLE visited_folders (
    nas_path TEXT PRIMARY KEY,
    client_path TEXT NOT NULL,
    folder_mtime INTEGER NOT NULL
);

-- Merge cache_index into broader file tracking
CREATE TABLE known_files (
    path TEXT PRIMARY KEY,
    nas_size INTEGER NOT NULL,
    nas_mtime INTEGER NOT NULL,
    is_hydrated INTEGER NOT NULL DEFAULT 0,
    hydrated_size INTEGER DEFAULT 0,
    last_accessed INTEGER DEFAULT 0
);
```

### Implementation order
1. DB schema changes (visited_folders + known_files)
2. Sync root reconnect (preserve placeholders)
3. Startup reconciliation (DB-driven diff)
4. Orphan quarantine
5. Symlink module (creation/removal/elevation)
6. Mount refactor (replace drive letters)
7. Cache location setting (global, configurable)
8. Frontend UI (remove drive letter/hide drives, add Connect + cache location)
9. Cleanup (remove dead code)

**macOS equivalent:** macOS already uses symlinks for all mounts (/opt/ufb/mounts/{id}).
FileProvider domains are in ~/Library/CloudStorage/. The reconciliation logic and DB
schema are cross-platform Rust. Symlink elevation is not needed (macOS allows symlinks
without admin). The orphan quarantine pattern works identically via FSEvents.

### Implementation progress (2026-04-09)

**Step 1 DONE: DB schema migration.**
- `cache_index` table migrated to `known_files` with NAS metadata (nas_size, nas_mtime,
  is_hydrated, hydrated_size, last_accessed). Auto-migration preserves existing cache data.
- New `visited_folders` table tracks folders user has browsed (for reconciliation scope).
- New `metadata` table (key-value) stores `last_connected_at` timestamp.
- Dehydration now marks `is_hydrated=0` instead of deleting rows (preserves file knowledge).
- New methods: `record_known_file`, `known_files_in_folder`, `record_visited_folder`,
  `visited_folders`, `update_folder_mtime`, `last_connected_at`, `update_last_connected`.

**Step 2 DONE: Sync root reconnect without re-register.**
- `SyncRoot::start()` now tries `Session::connect()` first. If the registration already
  exists, connects to it directly — placeholders survive agent restarts.
- Falls back to `register_fresh()` only if connect fails (first run or path changed).
- `stop()` no longer calls `unregister()` — only disconnects the CF session. Registration
  persists across restarts. Separate `SyncRoot::unregister()` method for explicit disable.
- `last_connected_at` updated on both start and stop.
- Tested: agent restart preserves all placeholders, log shows "Reconnected to existing sync root".

**Key finding:** `Session::connect()` (wrapping `CfConnectSyncRoot`) consumes the filter.
If connect fails, a new filter instance must be created for the registration path. Solved
with a `make_filter` closure that creates fresh instances.

**Step 3 DONE: Startup reconciliation (DB-driven diff).**

Three-layer reconciliation strategy:

1. **Visited folders seeding** (`seed_visited_folders`): On reconnect, walks local directories
   only (no NAS I/O) and ensures each has an entry in `visited_folders`. New entries get
   mtime=0 (forces diff); existing entries keep their mtime. Cost: milliseconds.

2. **Startup reconciliation** (`reconcile_startup`): Iterates all visited folders. Checks NAS
   folder mtime — skips unchanged folders (fast). For changed folders (or mtime=0 first-timers),
   performs three-way diff: DB (known_files) vs NAS (readdir) vs Local (on-disk). NAS is truth:
   new files → push placeholder, missing files → remove, changed size/mtime → update.
   Updates DB mtime after each folder. Cost: one SMB stat per folder, readdir only for changed.

3. **Live watcher**: `NasWatcher` uses prefix swap (NAS root → client root) for all events —
   no map lookup needed. Handles real-time changes eagerly across entire tree. Buffer overflow
   fallback diffs only visited folders (safe on large shares).

Supporting changes:
- `filter.rs`: FETCH_PLACEHOLDERS records `visited_folders` (with folder mtime) and
  `known_files` (with NAS size + mtime) in the cache DB during directory enumeration.
- `watcher.rs`: `NasWatcher` holds `Arc<CacheIndex>` and `client_root`. All live placeholder
  operations (push/remove/update) update `known_files` in DB. `ensure_parent_placeholders`
  creates intermediate directory placeholders for deep events.
- `cache.rs`: New `ensure_visited_folder` method (INSERT OR IGNORE with mtime=0).
- `orchestrator.rs`: Calls `reconcile_startup()` after `SyncRoot::start()` succeeds.

Bug fixes discovered during testing:
- `init_schema`: Migration from old `cache_index` table failed on fresh DBs because SQLite
  compiles `FROM cache_index` even inside a `WHERE EXISTS` guard. Fixed with separate step.
- `NasWatcher::start()` was never called on initial startup (only on reconnect via restart).
- Write-through `startup_recovery` was re-uploading all existing placeholders — added
  `is_placeholder()` check to skip CF reparse point files.

**Next:** Step 4 (orphan quarantine) — files that don't fit reconciliation logic get moved
to `\\server\share\.orphaned\` with timestamped duplicates.
