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

**Step 4 DONE: Safe handling of untracked local files.**

Originally planned as "orphan quarantine" (.orphaned directory on NAS), simplified after
testing to a more natural approach: let write-through handle uploads, respect NAS deletions.

Key behaviors:
- **CF placeholders** not on NAS → removed (stale, no local data to lose)
- **Real files** not on NAS → left for write-through to upload naturally
- **Real files** in NAS `#recycle` → deleted locally only if size+mtime match recycled
  version (same file). If local version differs (newer edits), kept for upload.
- **Local directories** not on NAS → created on NAS via `fs::create_dir_all`
- **Watcher `remove_placeholder`** → skips real files, uses `remove_dir` (not `remove_dir_all`)
  to prevent recursive deletion of user content

New helpers:
- `cache.rs`: `pub(crate) fn is_cf_placeholder()` — checks reparse point attribute
- `sync_root.rs`: `was_recycled_while_offline()` — checks `#recycle` with size+mtime comparison
- NAS readdir filters now exclude dot-prefixed entries (`.DS_Store`, etc.)

### Steps 5+6+7 DONE: Symlink mounts + drive letter replacement + cache location (2026-04-10)

**Architecture (final):**
```
User-visible:  C:\Volumes\ufb\{shareName}\ 
  Drive mount:  symlink → \\server\share (UNC) — requires Dev Mode or elevation
  Sync mount:   junction → %LOCALAPPDATA%\ufb\sync\{shareName}\ (no elevation needed)

Cache root:    %LOCALAPPDATA%\ufb\sync\ (default) or user-selected via Settings
Cache DB:      %LOCALAPPDATA%\ufb\cache\{mountId}.db
```

**Key decisions made during implementation:**
- **Junctions for local targets, symlinks for UNC**: `CreateSymbolicLinkW` to a local path
  causes Explorer to resolve the symlink and show the real path in the address bar. Junctions
  don't have this problem — Explorer keeps the junction path. Junctions also don't need elevation.
  UNC targets must use symlinks (junctions can't point to UNC paths).
- **SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE (0x2)**: Required flag for Developer Mode to
  work. Without it, CreateSymbolicLinkW still demands admin even with Dev Mode enabled.
- **SMB sessions in background**: Traditional mounts establish SMB sessions via `tokio::spawn`
  after reporting "mounted" state. This avoids blocking the agent startup. Windows authenticates
  on-demand when the user accesses the symlink if the session isn't ready yet.
- **Teardown on agent stop**: All symlinks/junctions removed on agent stop (both graceful and
  crash recovery). `C:\Volumes\ufb\` is empty when agent is down — prevents users from
  accidentally working with stale/broken paths. Originally planned to keep sync mount junctions
  on crash for offline cached file access, but simpler/safer to remove everything.
- **Cache path change = nuke + re-register**: When user changes sync cache location in Settings,
  all sync mounts are stopped, unregistered at old path, and re-started fresh at new path.
  Hydrated files are lost (they're cache — NAS is truth). Option A from design discussion.

**New files:**
- `platform/windows/mountpoint.rs`: `WindowsMountMapping` with `CreateSymbolicLinkW` (UNC)
  and `mklink /J` (local). `DriveMapping` trait implementation. `SymlinkError::NeedsElevation`.
- `platform/windows/elevation.rs`: `ShellExecuteW` runas launcher for `--create-symlinks` mode.

**Config changes:**
- `MountsConfig.sync_cache_root`: Global cache root (default `%LOCALAPPDATA%\ufb\sync`).
- `MountConfig.share_name()`: Now returns last UNC component (not second). Public method.
- `MountConfig.volume_path()`: User-facing path `C:\Volumes\ufb\{share_name}`.
- `MountConfig.volumes_base()`: Platform-specific base dir for volume mounts.
- `MountConfig.mount_path()`: Windows always returns volume_path(). No more drive letters.
- `MountConfig.sync_root_dir(cache_root)`: Takes cache root param (was hardcoded).
- `MountsConfig.cache_root()`: Resolves effective cache root from config or default.

**Agent changes:**
- `main.rs --create-symlinks`: Elevated mode. Creates all symlinks/junctions, migrates old
  drive letters, exits. Called via ShellExecuteW runas from normal agent.
- `orchestrator.rs mount_drive()`: SMB session in background, symlink check/create foreground.
  Reports `needs_elevation` if symlink creation fails.
- `orchestrator.rs start_sync()`: Creates junction to cache dir after CF registration.
- `orchestrator.rs disconnect_drive()/stop_sync()`: Removes symlinks/junctions on teardown.
- `mount_service.rs apply_config()`: Detects cache root change, stops all sync mounts for
  re-registration at new path.
- `messages.rs`: `needs_elevation` field on state updates, `CreateSymlinks` IPC command.

**Frontend changes:**
- Sidebar: removed per-mount toggle buttons, added "Mount Volumes" button (when needs_elevation).
- Settings: replaced drive letter input with read-only volume path. Added "Sync Cache Location"
  with folder picker and reset button. Cache path change triggers agent config reload.
- `mountStore.ts`: `getMountPath()` always returns volume path on Windows. Simplified
  `getMountForPath()` to use volume paths. `needsElevation` computed, `createSymlinks()`.
- Explorer pins: now point to volume paths, run in background `spawn_blocking`.

**IPC reconnect optimization:**
- Reduced initial backoff from 5s to 500ms (first 5 attempts), then 3s.

### macOS port notes

The architecture is designed for cross-platform. macOS equivalents:
- **Mount paths**: Currently uses `/opt/ufb/mounts/{id}` symlinks — switching to
  `/opt/ufb/mounts/{share_name}` for path consistency with Windows. See 2026-04-11 entry.
- **Sync mounts**: FileProvider domains live in `~/Library/CloudStorage/`. The symlink approach
  still applies — `/opt/ufb/mounts/{share_name}` symlink targets either `/Volumes/{share}`
  (SMB mode) or `~/Library/CloudStorage/{domain}` (sync mode). Same stable path regardless.
- **Cache location**: FileProvider controls cache location on macOS. `sync_cache_root` setting
  is ignored. Frontend hides "Cache Location" picker on macOS.
- **Elevation**: macOS symlinks don't need admin. The `/opt/ufb/mounts/` base dir needs admin
  to create (one-time), handled via installer or first-run elevation. Worth the cost for
  clean, Finder-friendly paths that users interact with daily.
- **Teardown**: Same pattern — remove symlinks on agent stop. macOS `open smb://` mounts
  persist in `/Volumes/` but the symlink in `/opt/ufb/mounts/` is removed.
- **Explorer pins → Finder sidebar**: macOS uses `LSSharedFileListInsertItemURL` or
  FileProvider sidebar integration. Different API but same concept.
- **DB schema, reconciliation, watcher**: All cross-platform Rust. Only the mount/unmount
  and symlink/junction code is platform-specific.

**Next:** Step 8 (Frontend UI polish) and Step 9 (cleanup/dead code removal).

---

## Explorer integration — CF nav entry deduplication (2026-04-10)

### Problem

Sync mounts created **two** Explorer sidebar entries:
1. Our nav pin (CLSID with `0FB` prefix) → `C:\Volumes\ufb\{share}` (junction path)
2. CF API auto-created `NamespaceCLSID` → cache path (e.g. `C:\z_ufbCache\test1`)

Users saw duplicate entries, and the CF one pointed to the internal cache path — an
implementation detail they shouldn't see or use directly.

Additionally, stale sync root registrations (from removed mounts like `sync-test`) left
orphaned `NamespaceCLSID` entries in `Desktop\NameSpace` even after the `SyncRootManager`
key was deleted.

### Solution: redirect + skip nav pin

The CF API's `NamespaceCLSID` is a standard shell folder CLSID with an
`Instance\InitPropertyBag\TargetFolderPath` value. We patch this after registration to
point at the junction (`C:\Volumes\ufb\{share}`) instead of the cache dir. Explorer then
navigates through the junction, which preserves the path in the address bar.

Since the CF entry now points to the right place (and has the cloud icon), we skip creating
a nav pin for sync mounts entirely.

**What was tried and rejected:**
- **Hiding the CF entry** (deleting from `Desktop\NameSpace`): The CF API re-creates it on
  `Session::connect()`, causing a race. Would need delayed cleanup with threads — fragile.
- **Registering CF at the junction path**: Would break the purpose of the junction (disconnect
  switch) and it's unclear if the CF API follows junctions correctly for placeholder ops.

### Implementation

**Agent (`mediamount-agent/src/sync/sync_root.rs`):**
- `redirect_sync_root_nav_entry(mount_id, volume_path)`: Looks up the `NamespaceCLSID` from
  `SyncRootManager`, patches `TargetFolderPath` in `HKCU\Software\Classes\CLSID\{clsid}\
  Instance\InitPropertyBag` to the junction path.
- Called in `SyncRoot::start()` after CF connect/register, and in `cleanup_stale_roots()`.
- `cleanup_stale_roots(active_sync_mounts)`: Takes `HashMap<mount_id, volume_path>`.
  Active sync roots get redirected. Stale roots get their nav entry removed + unregistered.
- `remove_orphaned_cf_nav_entries()`: Second-pass cleanup. Scans `Desktop\NameSpace` for
  entries whose default value is a `MediaMount!...` sync root ID but whose `SyncRootManager`
  key no longer exists. Removes the orphaned CLSID + class registration.
- `lookup_namespace_clsid(mount_id)`: Reads `NamespaceCLSID` from `SyncRootManager`.
- `remove_nav_entry_by_clsid(clsid)`: Deletes from `Desktop\NameSpace` + `Classes\CLSID`.

**Agent (`mediamount-agent/src/mount_service.rs`):**
- `apply_config()` calls `cleanup_stale_roots()` at startup and on config reload with a
  `HashMap<mount_id, volume_path>` of enabled sync mounts.

**Tauri app (`src-tauri/src/explorer_pins.rs`):**
- `collect_nav_pins()` skips mounts where `sync_enabled == true`. CF entry handles them.

### Edge cases

- **Fresh registration**: `register_fresh()` calls `unregister()` then `register()`. The new
  `register()` creates a fresh CLSID with the cache path. Our redirect runs immediately after.
- **Agent crash before redirect**: Cache path shows briefly. Fixed on next agent launch when
  `cleanup_stale_roots()` re-applies the redirect.
- **NamespaceCLSID changes**: `lookup_namespace_clsid()` reads it fresh from `SyncRootManager`
  each time, so it always finds the current one.
- **Orphaned CLSIDs**: Previous runs may have deleted the `SyncRootManager` key without
  cleaning the CLSID. `remove_orphaned_cf_nav_entries()` catches these by scanning
  `Desktop\NameSpace` for entries with `MediaMount!` default values.

### macOS implications

macOS FileProvider manages Finder sidebar entries natively — no equivalent of `NamespaceCLSID`.
FileProvider domains appear in `~/Library/CloudStorage/`. The redirect hack is Windows-only.
macOS needs its own sidebar strategy (likely `LSSharedFileListInsertItemURL` for symlink-based
entries, or just relying on FileProvider's built-in Finder integration).

**Verify FileProvider path:** On macOS, check whether the FileProvider domain's Finder sidebar
entry points to `~/Library/CloudStorage/MediaMount-{id}/` or to the symlink at
`/opt/ufb/mounts/{share}`. If it points to CloudStorage, the user sees internal paths — same
problem we had on Windows. May need a similar redirect or to ensure the symlink is the
canonical user-facing path.

### Other fixes in this session (2026-04-10) with macOS relevance

**System icons (cross-platform):**
- Icons now requested at 256px (was 32px). macOS `NSWorkspace` implementation (not yet built)
  should also use large icons. See `src-tauri/src/system_icons.rs` — macOS currently returns
  None, falling back to Material Symbols icon font.
- `ThumbnailImage.tsx`: System icon now loads immediately as a placeholder while the thumbnail
  request is queued. Thumbnail overwrites when ready. This is cross-platform SolidJS logic.

**Selection preserved on refresh (cross-platform):**
- `fileStore.ts navigateTo()`: Same-directory refreshes now preserve selection, pruning only
  paths that no longer exist. Previously, mount state updates (every ~2s for sync mounts)
  cleared selection. This fix applies to macOS too.

**Shell context menus (platform-specific):**
- Nilesoft Shell integration (`union_goto.nss`, `union_projects.nss`, `project_notes.ps1`)
  updated to use `C:\Volumes\ufb\{share}` paths. Windows-only.
- macOS needs equivalent Finder integration: Finder Extensions, Services, or Quick Actions
  for project creation, notes, and navigation shortcuts. Not yet implemented.

---

## 2026-04-11 — macOS path consistency & symlink unification

### Problem

Windows unified all mounts under `C:\Volumes\ufb\{share_name}` — symlink target changes
based on mode (SMB vs sync), but the user-facing path is always the same. macOS was using
`/opt/ufb/mounts/{id}` where `id` is the config identifier (e.g., `primary-nas`), not the
human-readable share name. This creates a mismatch:

```
NAS: \\192.168.40.100\Jobs_Live   config id: primary-nas
Windows:  C:\Volumes\ufb\Jobs_Live        ← share_name
macOS:    /opt/ufb/mounts/primary-nas     ← id (doesn't match)
```

Path mappings between platforms become unintuitive when the last component differs.

### Decision: macOS mount_path() uses share_name

Change `mount_path()` on macOS from `self.id` to `self.share_name()`:

```
Before:  /opt/ufb/mounts/{id}           → /opt/ufb/mounts/primary-nas
After:   /opt/ufb/mounts/{share_name}   → /opt/ufb/mounts/Jobs_Live
```

Uses `volumes_base().join(self.share_name())` — same pattern as Windows `volume_path()`.
The `mount_path_macos` override still takes precedence for custom paths.

### Decision: symlink approach works for both modes on macOS

The symlink at `/opt/ufb/mounts/{share_name}` is the stable user-facing path. Its target
changes based on mode:

```
macOS SMB mode:
  /opt/ufb/mounts/Jobs_Live  →  /Volumes/Jobs_Live

macOS Sync mode (FileProvider):
  /opt/ufb/mounts/Jobs_Live  →  ~/Library/CloudStorage/com.unionfiles.mediamount-tray.FileProvider-Jobs_Live/
```

This mirrors Windows exactly:
```
Windows SMB mode:
  C:\Volumes\ufb\Jobs_Live  →  \\nas\Jobs_Live

Windows Sync mode (Cloud Files):
  C:\Volumes\ufb\Jobs_Live  →  %LOCALAPPDATA%\ufb\sync\Jobs_Live\
```

Users always navigate to the same path. Path mappings work because the last component
(`Jobs_Live`) matches across platforms.

### Decision: keep /opt/ufb/mounts as base path

Considered `~/.local/share/ufb/mounts` (user-writable, no elevation) but rejected because
users interact with these paths in Finder daily — bookmarks, scripts, drag-drop. `/opt/ufb/mounts`
is clean, short, and discoverable. One-time `sudo mkdir -p /opt/ufb/mounts && sudo chmod 755
/opt/ufb` during installer or first-run is an acceptable cost.

### Decision: no cache location setting on macOS

FileProvider controls where the cache lives (`~/Library/CloudStorage/`). Unlike Windows Cloud
Files where we pick the sync root path, macOS FileProvider assigns it via `NSFileProviderDomain`.

Changes:
- Frontend: hide "Cache Location" picker when `platform === "mac"`
- Backend: `sync_root_dir()` returns `fileprovider_domain_path()` on macOS, ignoring `sync_cache_root`
- `sync_cache_root` config field is simply unused on macOS (no breaking change)

### Decision: FileProvider bundle ID convention

Extension bundle ID: `com.unionfiles.mediamount-tray.FileProvider`
Domain identifier per mount: `{share_name}` (e.g., `Jobs_Live`)
CloudStorage path: `~/Library/CloudStorage/com.unionfiles.mediamount-tray.FileProvider-{share_name}/`

### New config helper: fileprovider_domain_path()

Added to `MountConfig` (macOS only):
```rust
pub fn fileprovider_domain_path(&self) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/CloudStorage")
        .join(format!("com.unionfiles.mediamount-tray.FileProvider-{}", self.share_name()))
}
```

### Migration: stale symlink cleanup

Old symlinks at `/opt/ufb/mounts/{id}` become orphaned after switching to `{share_name}`.
At startup, scan `/opt/ufb/mounts/` and remove symlinks that don't match any active mount's
`share_name()`. Safe because all symlinks in this directory are agent-managed.

### Share name collisions

Two mounts could have the same `share_name()` (e.g., two NAS servers both sharing `Jobs_Live`).
Added `share_name_collisions()` detection on `MountsConfig` — logs a warning at startup.
User resolves by setting `mount_path_macos` override on one mount. Same limitation exists
on Windows today.

### Frontend changes

- `mountStore.ts getMountPath()`: macOS branch now uses `getShareName(cfg)` instead of `cfg.id`
- `SettingsDialog.tsx`: Cache location section gated on `platform !== "mac"`
- `SettingsDialog.tsx`: Sync toggle enabled for macOS (was Windows-only). The actual
  FileProvider implementation comes later; this just exposes the config toggle.

### Files to modify

| File | Change |
|------|--------|
| `mediamount-agent/src/config.rs` | `mount_path()` macOS default uses `share_name()`, add `fileprovider_domain_path()`, `sync_root_dir()` macOS branch, `share_name_collisions()` |
| `mediamount-agent/src/orchestrator.rs` | Sync mode routing in macOS `mount_drive()` and `disconnect_drive()` |
| `mediamount-agent/src/mount_service.rs` | Stale symlink cleanup at startup |
| `src/stores/mountStore.ts` | `getMountPath()` macOS uses `getShareName()` |
| `src/components/Settings/SettingsDialog.tsx` | Hide cache location on macOS, enable sync toggle on macOS |

All changes implemented and tested. 34/34 agent tests pass. Frontend builds clean.
Verified symlinks at `/opt/ufb/mounts/` show `Jobs_Live` and `MinRender` (share_name based).
`test1` sync symlink points to FileProvider domain path (dangling until extension is built).

---

## 2026-04-11 — FileProvider extension: architecture & spike plan

### Current state of MediaMountTray

The tray app is a **single Swift file** (`MediaMountTray.swift`, 340 lines) compiled directly
with `swiftc`. It communicates with the Rust agent via Unix domain socket IPC (length-prefixed
JSON, same protocol as the Tauri app on Windows named pipes).

Structure:
- `MediaMountTrayApp` — SwiftUI MenuBarExtra (LSUIElement)
- `AgentConnection` — POSIX socket client, auto-reconnect, message parsing
- `MountInfo` — Observable model for mount status

Build: `swiftc -parse-as-library -O -o MediaMountTray MediaMountTray.swift`
Bundle: Manually assembled `.app` in `build-macos.sh`

### Why migration is needed

FileProvider extensions are **App Extensions** — they must be bundled inside a host app as
`.appex` in `Contents/PlugIns/`. This requires:
- Proper Xcode targets (host app + extension)
- Entitlements for app groups (shared sandbox container)
- Info.plist with NSExtension configuration
- Code signing with developer certificate

Single-file `swiftc` compilation cannot produce App Extensions.

### Decision: XcodeGen for project generation

Using XcodeGen (`brew install xcodegen`) to generate `.xcodeproj` from a `project.yml` spec.
Keeps project definition in version control as YAML, regenerate with `xcodegen generate`.

### Architecture: FileProvider extension ↔ agent communication

**Phase 0 approach (spike):** Extension accesses NAS files directly via the agent's existing
SMB mount at `/Volumes/{share}`. No new IPC needed. This mirrors the Windows pattern where
CF API callbacks use `std::fs` on UNC paths — the OS SMB driver handles the network I/O.

**Sandbox risk:** FileProvider extensions are sandboxed. If they can't access `/Volumes/`,
we fall back to IPC-based file operations where the extension sends read/write requests to
the agent via a Unix socket in the shared app group container.

**Phase 0 will test this hypothesis before committing to either architecture.**

### Project structure

```
mediamount-tray/
├── project.yml                          (XcodeGen spec)
├── MediaMountTray/
│   ├── MediaMountTrayApp.swift          (SwiftUI MenuBarExtra — from existing code)
│   ├── AgentConnection.swift            (IPC — extracted from existing code)
│   ├── MountInfo.swift                  (Model — extracted)
│   ├── DomainManager.swift              (FileProvider domain registration)
│   ├── Info.plist
│   └── MediaMountTray.entitlements
├── FileProviderExtension/
│   ├── FileProviderExtension.swift      (NSFileProviderReplicatedExtension)
│   ├── FileProviderItem.swift           (NSFileProviderItem wrapper)
│   ├── FileProviderEnumerator.swift     (Directory enumeration)
│   ├── Info.plist
│   └── FileProviderExtension.entitlements
└── MediaMountTray.swift                 (original single-file, kept as reference)
```

### Targets

**MediaMountTray** (host app):
- Bundle ID: `com.unionfiles.mediamount-tray`
- macOS 12.0+ (FileProvider Replicated stable from 12)
- LSUIElement: true
- App group: `group.com.unionfiles.mediamount-tray`
- On launch: reads `~/.local/share/ufb/mounts.json`, registers FileProvider domains
  for sync-enabled mounts via `NSFileProviderManager.add(domain:)`

**FileProviderExtension** (app extension):
- Bundle ID: `com.unionfiles.mediamount-tray.FileProvider`
- Extension point: `com.apple.fileprovider-nonui`
- Same app group
- NSExtensionFileProviderSupportsEnumeration: true
- Implements `NSFileProviderReplicatedExtension`:
  - `item(for:)` — return metadata for a single item
  - `enumerator(for:)` — return enumerator for directory listing
  - `fetchContents(for:)` — download file from NAS (sandbox test)

### Domain registration flow

1. Host app reads `mounts.json` on launch
2. For each mount with `syncEnabled: true`:
   - Domain identifier = `share_name` (e.g., `Jobs_Live`)
   - Display name = `mount.displayName` (e.g., `Studio NAS`)
   - `NSFileProviderManager.add(domain:)` → creates `~/Library/CloudStorage/` entry
3. Agent creates symlink: `/opt/ufb/mounts/{share_name}` → `~/Library/CloudStorage/{domain}/`
4. Extension launched by system when user browses the domain

### FileProvider callback → SMB mapping (mirrors Windows CF API)

| FileProvider | Windows CF API | SMB Operation |
|---|---|---|
| `enumerateItems` | `fetch_placeholders` | `fs::read_dir(/Volumes/{share}/path)` |
| `fetchContents` | `fetch_data` | `fs::File::open(/Volumes/{share}/path)` → stream |
| `createItem` | (write-through) | `fs::File::create(/Volumes/{share}/path)` |
| `modifyItem` | (write-through) | `fs::write(/Volumes/{share}/path)` |
| `deleteItem` | `delete` | `fs::remove_file(/Volumes/{share}/path)` |

### Build system changes

`build-macos.sh` step 4 changes from:
```bash
swiftc -parse-as-library -O -o MediaMountTray MediaMountTray.swift
```
To:
```bash
xcodegen generate
xcodebuild -project MediaMountTray.xcodeproj -scheme MediaMountTray -configuration Release
```

The built `.app` now contains `Contents/PlugIns/FileProviderExtension.appex`.

### Phase 0 spike success criteria

1. Domain appears in `~/Library/CloudStorage/`
2. Finder sidebar shows the domain
3. Browsing domain in Finder triggers `enumerateItems` → shows NAS directory contents
4. Opening a file triggers `fetchContents` → file opens from NAS
5. **Sandbox verdict:** either `/Volumes/` access works, or we know the exact error

### Reusable from Windows implementation

- Cache DB schema (`known_files`, `visited_folders`, `metadata` tables)
- Upload worker logic (temp file, conflict detection, chunked write)
- Echo suppressor (HashMap with TTL)
- Coordinator state machine (debounce timers, per-path tracking)
- NAS connectivity tracking (online/offline state)
- Startup reconciliation (three-way diff)

### macOS-specific (new code)

- FileProvider extension Swift code (extension, item, enumerator)
- Domain registration from host app
- FSEvents-based NAS watcher (replaces ReadDirectoryChangesW)
- `NSFileProviderManager.signalEnumerator()` for change notifications
- `NSFileProviderManager.evictItem()` for cache eviction (replaces CfDehydratePlaceholder)

---

## 2026-04-11 — FileProvider Phase 0 spike results

### What was built

Migrated MediaMountTray from single-file `swiftc` build to XcodeGen project with two targets:
- **MediaMountTray** (host app): MenuBarExtra + `DomainManager` for FileProvider domain registration
- **FileProviderExtension** (app extension): `NSFileProviderReplicatedExtension` spike

Build: `xcodegen generate && xcodebuild -scheme MediaMountTray -configuration Debug -allowProvisioningUpdates`

### Project structure

```
mediamount-tray/
├── project.yml                          (XcodeGen spec → generates .xcodeproj)
├── MediaMountTray/
│   ├── MediaMountTrayApp.swift          (SwiftUI MenuBarExtra + DomainManager init)
│   ├── AgentConnection.swift            (Unix socket IPC to agent)
│   ├── MountInfo.swift                  (Observable mount state model)
│   ├── DomainManager.swift              (reads mounts.json, registers NSFileProviderDomain)
│   ├── Info.plist
│   └── MediaMountTray.entitlements
├── FileProviderExtension/
│   ├── FileProviderExtension.swift      (NSFileProviderReplicatedExtension)
│   ├── FileProviderItem.swift           (NSFileProviderItem with contentType, itemVersion)
│   ├── FileProviderEnumerator.swift     (directory listing via FileManager)
│   ├── Info.plist                       (com.apple.fileprovider-nonui extension point)
│   └── FileProviderExtension.entitlements
└── MediaMountTray.swift                 (original single-file, kept as reference)
```

### Findings during spike

**1. Sandbox entitlement required (CRIT)**
FileProvider extensions MUST have `com.apple.security.app-sandbox` entitlement.
Without it: `"Extension must have com.apple.security.app-sandbox entitlement."` and the
extension process is never created.

**2. App group needs team ID prefix**
`group.com.unionfiles.mediamount-tray` → REJECTED by containermanagerd.
`5Z4S9VHV56.group.com.unionfiles.mediamount-tray` → APPROVED.
Group container IDs must be prefixed with the team ID on macOS.

**3. Root container needs filename and itemVersion**
FileProvider crashes the extension process (assertion failure) if root container item
is missing `filename` or `itemVersion`. Both are required even for `.rootContainer`.
- `filename`: Use `domain.displayName`
- `itemVersion`: `NSFileProviderItemVersion(contentVersion:metadataVersion:)` with any data

**4. `typeIdentifier` deprecated → use `contentType: UTType`**
Latest macOS SDK marks `typeIdentifier` as unavailable. Use `contentType` property
returning `UTType` instead.

**5. `modifyItem` uses `NSFileProviderModifyItemOptions`, not `NSFileProviderCreateItemOptions`**
Different option types for create vs modify — compiler catches this.

**6. macOS 13+ required for MenuBarExtra**
`MenuBarExtra` API requires macOS 13.0. Set deployment target accordingly.

### SANDBOX VERDICT: /Volumes/ access BLOCKED

**This is the critical finding.** The sandboxed FileProvider extension cannot access
`/Volumes/{share}` (SMB mount points). Error:

```
NSCocoaErrorDomain Code=257
"The file "test1" couldn't be opened because you don't have permission to view it."
NSPOSIXErrorDomain Code=1 "Operation not permitted"
```

The share is mounted and accessible from the host app and terminal, but the extension
sandbox blocks it. This rules out the direct `/Volumes/` access approach.

**Consequence:** All file I/O must go through IPC to the agent. The extension sends
requests (list_dir, read_file, write_file) and the agent services them from the
mounted SMB share.

### Domain registration works

`DomainManager` reads `~/.local/share/ufb/mounts.json`, finds sync-enabled mounts,
calls `NSFileProviderManager.add(domain:)`. Result:
- `~/Library/CloudStorage/MediaMountTray-Test1/` appears
- Finder sidebar shows the domain
- System launches extension on browse

### IPC architecture (next phase)

Since the extension can't access `/Volumes/` directly, we need a file-operation IPC
channel between the extension and the agent:

```
┌─────────────────────────────────┐
│  FileProviderExtension          │
│  (sandboxed, no /Volumes/)      │
│                                 │
│  enumerateItems ──┐             │
│  fetchContents  ──┼── IPC ──────┼──► Agent (has /Volumes/ access)
│  modifyItem     ──┤  (socket    │       │
│  deleteItem     ──┘   in app    │       ├── SMB readdir
│                      group)     │       ├── SMB read (stream)
│                                 │       ├── SMB write
└─────────────────────────────────┘       └── SMB delete/rename
```

**IPC transport:** Unix domain socket in the shared app group container:
`{group_container}/agent.sock`

The agent already has a Unix socket server (`mediamount-agent/src/ipc/unix_server.rs`).
We add a second listener in the app group container specifically for extension requests.

**New message types needed (Extension → Agent):**

| Message | Purpose | Response |
|---------|---------|----------|
| `list_dir(nas_path)` | Directory enumeration | Array of `{name, is_dir, size, mtime}` |
| `read_file(nas_path, offset, length)` | Chunked file read | Binary data chunk |
| `write_file(nas_path, data)` | File upload | Success/error |
| `delete(nas_path)` | File/dir deletion | Success/error |
| `rename(old_path, new_path)` | Rename/move | Success/error |
| `stat(nas_path)` | File metadata | `{size, mtime, is_dir}` |

**Agent → Extension notifications:**
- NAS change detected → agent calls `NSFileProviderManager.signalEnumerator()`
  via the host app (extension can't do this directly? — verify)

**File streaming for fetchContents:**
For large files, the agent writes to a temp file in the app group container, then
returns the path. The extension passes this URL to the FileProvider completion handler.
This avoids streaming binary data through JSON IPC.

```
Extension: fetchContents("project/scene.nk")
  → Agent: reads /Volumes/test1/project/scene.nk
  → Agent: writes to {group_container}/temp/{uuid}.tmp
  → Agent: responds with temp file path
  → Extension: completionHandler(tempFileURL, item, nil)
```

### App group container path

```swift
let groupContainer = FileManager.default.containerURL(
    forSecurityApplicationGroupIdentifier: "5Z4S9VHV56.group.com.unionfiles.mediamount-tray"
)!
let socketPath = groupContainer.appendingPathComponent("agent.sock")
```

Rust agent equivalent:
```rust
// macOS: ~/Library/Group Containers/5Z4S9VHV56.group.com.unionfiles.mediamount-tray/
let group_dir = dirs::home_dir()
    .join("Library/Group Containers/5Z4S9VHV56.group.com.unionfiles.mediamount-tray");
let socket_path = group_dir.join("agent.sock");
```

### Implementation order for IPC phase

1. **Agent: second Unix socket listener** in app group container
2. **Agent: file operation message handlers** (list_dir, stat, read → temp file)
3. **Extension: IPC client** connecting to app group socket (reuse AgentConnection pattern)
4. **Extension: enumerateItems** via IPC list_dir instead of FileManager
5. **Extension: fetchContents** via IPC read → temp file → completion handler
6. **Test:** browse CloudStorage in Finder, open a file
7. **Extension: createItem/modifyItem** via IPC write (write-through)
8. **Agent: NAS watcher** signals extension via signalEnumerator

### Files to modify (IPC phase)

| File | Change |
|------|--------|
| `mediamount-agent/src/ipc/mod.rs` | Add app-group socket listener alongside existing |
| `mediamount-agent/src/ipc/unix_server.rs` | Support file-operation messages |
| `mediamount-agent/src/messages.rs` | New message types for file ops |
| `FileProviderExtension/FileProviderExtension.swift` | IPC client, replace FileManager calls |
| `FileProviderExtension/FileProviderEnumerator.swift` | IPC-based enumeration |
| `FileProviderExtension/AgentIPCClient.swift` | New: socket client for extension |

---

## 2026-04-12 — FileProvider IPC implementation (Phase 1)

### IPC file operations server — implemented and working

Built a separate request-response IPC channel between the FileProvider extension and the agent.
The existing mount-management IPC (broadcast-based, `/tmp/ufb-mediamount-agent.sock`) is
untouched. The new file ops socket is purpose-built for per-client request-response.

**Architecture:**
```
Existing (unchanged):
  /tmp/ufb-mediamount-agent.sock  →  broadcast mount state to all clients

New (file operations):
  ~/Library/Group Containers/.../fp.sock  →  request-response per-client
  FileProviderExtension ──request──► Agent ──response──► same client
```

### Implementation

**Rust side (mediamount-agent):**

- `messages.rs`: Added `FileOpsRequest` and `FileOpsResponse` enums with tagged JSON serde.
  Request types: `ListDir`, `Stat`, `ReadFile`, `Ping`.
  Response types: `DirListing`, `FileStat`, `FileReady`, `Error`, `Pong`.
  All requests carry a `request_id` for matching.

- `ipc/fileops_server.rs`: New Unix socket server in the app group container.
  - Listens on `{group_container}/fp.sock`
  - Per-client blocking read loop (not broadcast)
  - `handle_request()` dispatches to `handle_list_dir`, `handle_stat`, `handle_read_file`
  - Path resolution: maps domain (share_name) → mount config → filesystem path
  - For sync mode mounts: resolves directly to `/Volumes/{share_name}` (not through the
    FileProvider symlink, which would be circular)
  - `ReadFile`: copies file to `{group_container}/temp/{timestamp}.tmp`, returns path
  - Path traversal protection via `canonicalize()` + `starts_with()` check
  - Filters hidden/system files (`.`, `@`, `#` prefixes)

- `ipc/mod.rs`: Added `pub mod fileops_server` (macOS only)
- `main.rs`: `FileOpsServer::start()` called in `run_event_loop()` on macOS

**Swift side (FileProviderExtension):**

- `AgentFileOpsClient.swift`: New synchronous socket client.
  - Connects to `{group_container}/fp.sock` via POSIX Unix domain socket
  - Same wire protocol as existing IPC (4-byte LE length + JSON)
  - `listDir(domain:relativePath:)` → sends `list_dir`, returns `[DirEntryResponse]`
  - `stat(domain:relativePath:)` → sends `stat`, returns `FileStatResponse`
  - `readFile(domain:relativePath:)` → sends `read_file`, returns `(URL, FileStatResponse)`
  - Thread-safe via `NSLock`, auto-reconnect on failure
  - Singleton: `AgentFileOpsClient.shared`

- `FileProviderExtension.swift`: Updated to use IPC.
  - `item(for:)` → `client.stat()` instead of `FileManager.attributesOfItem`
  - `fetchContents(for:)` → `client.readFile()` returns temp file URL
  - `enumerator(for:)` passes `domainId` instead of `nasBasePath`

- `FileProviderEnumerator.swift`: Updated to use IPC.
  - `enumerateItems` → `client.listDir()` instead of `FileManager.contentsOfDirectory`
  - Builds `FileProviderItem` from `DirEntryResponse`

### Findings during implementation

**1. Unix socket path length limit (104 bytes on macOS)**
`agent-fileops.sock` in the app group container exceeded the `sun_path` limit (104 chars).
Shortened to `fp.sock` (93 chars). This is a hard macOS kernel limit.

**2. Sync mode path resolution is circular**
For sync-enabled mounts, `mount_path()` returns `/opt/ufb/mounts/{share}` which symlinks
to `~/Library/CloudStorage/...` — the FileProvider domain itself. The agent can't read from
there (that's what the extension provides). Fix: for sync mode mounts, the fileops server
resolves directly to `/Volumes/{share_name}` where the actual SMB mount lives.

**3. Product name controls Finder sidebar label**
Finder sidebar shows the host app's `PRODUCT_NAME`, not `CFBundleName`. Changed from
`MediaMountTray` to `UFB` in `project.yml`. Required killing `fileproviderd` and Finder
to clear cached state. The DomainManager now removes all existing domains before
re-registering to ensure config changes take effect.

**4. Domain re-registration needed for name changes**
Simply changing `CFBundleName` doesn't update the sidebar. Must remove old domain via
`NSFileProviderManager.remove()` and re-add. The `DomainManager.registerDomains()` now
always removes all existing domains first, then registers fresh.

**5. Single domain shows app name only**
With one FileProvider domain, Finder sidebar shows just "UFB". With multiple domains,
it shows "UFB - {displayName}" to disambiguate.

### Test results

- Agent starts, fileops socket listens at `~/Library/Group Containers/.../fp.sock`
- MediaMountTray launches, registers domain, extension connects to socket
- Browsing `~/Library/CloudStorage/UFB-Test1/` in Finder shows NAS directory contents
- Files listed via IPC (agent reads `/Volumes/test1`, returns entries to extension)
- Finder sidebar shows "UFB"

### Remaining work (as of early 2026-04-12)

- `fetchContents` (file download) — not yet tested end-to-end
- `createItem` / `modifyItem` — write-through not implemented
- `deleteItem` — not implemented
- NAS watcher → `signalEnumerator` — not implemented
- Cache eviction via `evictItem` — not implemented
- Agent should mount SMB headlessly for sync-mode shares (currently requires manual `open smb://`)

---

## 2026-04-12 — Write-through, headless SMB, agent lifecycle, icon

### Write-through (create/modify/delete) — working

Implemented full write support through the IPC channel:

**Agent side (`fileops_server.rs`):**
- `WriteFile` handler: receives staged file path from group container, copies to NAS
- `DeleteItem` handler: deletes file or directory on NAS
- Both new handlers with path traversal protection

**Swift side (`AgentFileOpsClient.swift`):**
- `writeFile()` stages content in `{group_container}/staging/` before sending to agent
- `deleteItem()` sends delete request to agent

**Key finding: file staging required for writes.**
The system provides file content to the extension via a URL in FileProvider's internal
staging area (`~/Library/Application Support/FileProvider/.../wharf/propagate/`). The agent
cannot read this path — it's sandboxed to the extension process. Fix: extension copies
the file to the shared app group container first, then tells the agent the staged path.
Same pattern as `fetchContents` but in reverse — the group container is the handoff zone.

**Extension capabilities updated:**
Items now have full capabilities: `.allowsReading`, `.allowsWriting`, `.allowsDeleting`,
`.allowsRenaming`, `.allowsAddingSubItems` (directories), `.allowsContentEnumerating`.

**Error domain fix:**
FileProvider only accepts `NSCocoaErrorDomain` and `NSFileProviderErrorDomain`. Our custom
`FileOpsError` type was being rejected. Added `.asNSError` converter that maps errors to
appropriate `NSCocoaErrorDomain` codes.

**Working set / trash containers:**
Extension now returns empty results for `.workingSet` and `.trashContainer` enumerations
instead of trying to resolve them as NAS paths.

### Headless SMB mount for sync-mode shares — working

The agent now mounts SMB shares in the background for sync-mode mounts. Previously required
manual `open smb://` before the FileProvider could browse.

**Orchestrator change:** In `mount_drive()` for macOS sync mode, the agent:
1. Mounts SMB via `macos_smb_mount()` (same as regular mounts)
2. Creates symlink from `/opt/ufb/mounts/{share}` → FileProvider domain path

On disconnect, it unmounts the headless SMB mount at `/Volumes/{share_name}`.

### Multi-domain support — working

Tested with two sync-enabled shares (`test1` on home NAS, `GFX_Dropbox` on work NAS).
Both appear in Finder sidebar under "UFB". DomainManager correctly:
- Preserves existing domains across relaunches (cache persists)
- Only removes stale domains (ones no longer in config)
- Registers new domains as needed

### Agent lifecycle — tray app manages it

New `AgentProcess.swift` manages the agent binary lifecycle:
- On tray app launch: finds and spawns `mediamount-agent` as a background process
- On quit: terminates agent gracefully (SIGTERM, then SIGINT after 2s)
- Skips launch if agent is already running (checked via `pgrep`)
- Binary search order: bundled Resources → sibling → cargo debug build → system path

No more terminal window needed to run the agent.

### App icon

Host app uses the main UFB icon (`AppIcon.icns` from `src-tauri/icons/icon.icns`).
FileProvider picks this up automatically for the Finder sidebar via `CFBundleIconFile`
in the host app's Info.plist.

`PRODUCT_NAME: UFB` controls the sidebar label. With multiple domains, Finder shows
"UFB - Test 1" and "UFB - GFX Dropbox".

### Current status

| Feature | Status |
|---|---|
| Domain registration | Done |
| Browse (enumerateItems) | Done |
| Open files (fetchContents) | Done |
| Create files/folders | Done |
| Modify files | Done |
| Delete (trash) | Done |
| Multi-domain | Done |
| Headless SMB mount | Done |
| Agent auto-launch | Done |
| App icon in Finder | Done |
| Cache persistence | Done |
| Error handling | Done |
| NAS watcher → signalEnumerator | Done |
| SQLite cache DB (change tracking) | Done |
| Live deep folder changes | Done |
| Cold start catch-up | Done |
| Rename support | Not started |
| Cache eviction (evictItem) | Not started |
| build-macos.sh update | Not started |

---

## 2026-04-12 — Live change detection + cache DB

### The problem

FileProvider caches enumeration results. Once the system has called `enumerateItems` for a
folder, subsequent visits use the cache and only call `enumerateChanges`. Without a way to
compute deltas, Finder shows stale data.

### Key findings

**1. `signalEnumerator` only works with `.workingSet`**
Calling `signalEnumerator(for: .rootContainer)` is silently ignored by the system. Only
`.workingSet` triggers the system to call `enumerateChanges`. This was the primary blocker
for live updates. Discovered via Apple docs research — the FileProvider API requires the
working set pattern for change propagation.

**2. `enumerateChanges` for individual folders is never called by the system**
When the user browses a folder, the system serves from its internal cache. It does NOT call
`enumerateChanges` for that specific container. All change propagation flows through the
working set. This means the working set `enumerateChanges` must report items from ALL
visited folders, not just root.

**3. Extension process caching during development**
The system caches the FileProvider extension process. After rebuilding, you must kill
`FileProviderExtension` and `fileproviderd` to force a fresh load of the new binary.
Without this, the old code runs even after xcodebuild succeeds.

### Architecture: SQLite cache + FSEvents + working set

```
FSEvents on /Volumes/{share} (recursive)
  │
  ├── Detects file changes at any depth
  │
  └── Posts DistributedNotification → Extension receives
        │
        └── signalEnumerator(.workingSet)
              │
              └── System calls enumerateChanges on working set enumerator
                    │
                    └── Extension calls agent IPC: getChanges(domain, anchor)
                          │
                          └── Agent diffs ALL visited folders:
                              DB (known_files) vs NAS (live readdir)
                              │
                              └── Returns {updated: [...], deleted: [...]}
                                    │
                                    └── Extension reports to system
                                          │
                                          └── Finder updates
```

### SQLite cache DB

**Location:** `~/.local/share/ufb/cache/{domain}.db`

**Schema:**
```sql
CREATE TABLE known_files (
    path TEXT PRIMARY KEY,       -- relative path from share root
    name TEXT NOT NULL,
    is_dir INTEGER NOT NULL DEFAULT 0,
    nas_size INTEGER NOT NULL,
    nas_mtime REAL NOT NULL,
    nas_created REAL NOT NULL DEFAULT 0
);

CREATE TABLE visited_folders (
    path TEXT PRIMARY KEY,       -- relative path from share root
    folder_mtime REAL NOT NULL DEFAULT 0
);

CREATE TABLE metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

Simplified vs Windows: no hydration/eviction columns (FileProvider manages materialization).
Only tracks NAS-side metadata for three-way diffing.

**How it populates:**
- `ListDir` handler records entries in `known_files` and folder in `visited_folders`
- `WriteFile` handler updates `known_files` after successful write
- `DeleteItem` handler removes from `known_files` after successful delete
- Root folder is always included in change detection even if never visited

**How it detects changes:**
- `get_changes_since()` walks ALL visited folders
- For each: readdir NAS, compare against `known_files` in DB
- New on NAS (not in DB) → reported as update
- Gone from NAS (in DB) → reported as deletion
- Size/mtime changed → reported as update
- Updates DB with current state after diffing

### New IPC messages

| Request | Response | Purpose |
|---------|----------|---------|
| `get_changes(domain, anchor)` | `{updated, deleted, new_anchor}` | Delta query for working set |
| `record_enumeration(domain, path, entries)` | `ack` | Record enumeration in cache |

### Notification mechanism

Using `DistributedNotificationCenter` (not Darwin notifications via `notify_post`).
Darwin notifications require a CFRunLoop which the extension's XPC service queue may not have.
DistributedNotificationCenter works across processes without run loop requirements.

Agent posts: `CFNotificationCenterPostNotification` via raw FFI to the distributed center.
Extension listens: `DistributedNotificationCenter.default().addObserver(...)`.
Notification name: `com.unionfiles.ufb.nas-changed.{domain}`.

### FSEvents behavior on SMB

- Recursive watching works for live changes (tested with deep folder creation)
- Does NOT detect changes that happened before the watch started (cold start gap)
- Poll fallback (5-second interval, root only) catches changes FSEvents might miss
- FSEvents fires reliably for most operations; poll is a safety net

### Development workflow

After rebuilding the extension, you must force-reload it:
```
killall FileProviderExtension; killall fileproviderd
```
Then relaunch the tray app. The system will load the fresh binary.

---

## 2026-04-12 — Rename support + build script update

### Rename support — implemented

New `RenameItem` IPC message: extension sends old path + new path, agent does `fs::rename`
on the NAS, updates cache DB (removes old entry, adds new). `modifyItem` in the extension
checks `changedFields.contains(.filename)` and calls `renameItem` instead of `writeFile`.

The renamed item gets a new `NSFileProviderItemIdentifier` (based on the new relative path).
The system handles updating its internal state when the extension returns the new item from
`modifyItem`.

### build-macos.sh updated

Step 4 replaced: old `swiftc` single-file build → `xcodegen generate` + `xcodebuild`.
The built `UFB.app` includes the embedded `FileProviderExtension.appex` in `Contents/PlugIns/`.
Signing order: extension first (inner), then tray app (outer), then main UFB app.

### Echo suppression — deferred

Own writes trigger FSEvents → spurious re-enumeration. Not breaking: the extra `getChanges`
call diffs the folder and finds no real changes (the DB already has the new entry). The 500ms
FSEvents debounce coalesces rapid events. Will add suppression later if bulk operations
(e.g., dragging 500 files) cause performance issues.

### Dev workflow: auto-reload extension

The system caches the FileProvider extension binary. In debug builds, the tray app now
kills `fileproviderd` on launch (`#if DEBUG` guard), forcing the system to reload the
fresh extension. No more manual `killall FileProviderExtension; killall fileproviderd`
after each rebuild. In production/release builds this is skipped — macOS handles binary
updates through the normal app install flow.

### Current status — macOS FileProvider Phase 1 complete

| Feature | Status |
|---|---|
| Domain registration | Done |
| Browse (enumerateItems) | Done |
| Open files (fetchContents) | Done |
| Create files/folders | Done |
| Modify files | Done |
| Delete (trash) | Done |
| Rename | Done |
| Multi-domain | Done |
| Headless SMB mount | Done |
| Agent auto-launch | Done |
| App icon in Finder | Done |
| Cache persistence | Done |
| Error handling | Done |
| Live change detection (FSEvents) | Done |
| SQLite cache DB | Done |
| Cold start catch-up | Done |
| Working set change propagation | Done |
| build-macos.sh | Done |
| Echo suppression | Deferred (harmless) |
| Cache eviction (evictItem) | Not needed (FileProvider manages) |
| mtime optimization | Deferred (optimize later for large shares) |
| Settings UI macOS polish | Done |
| System icons (NSWorkspace) | Done |
| Show Mounts in Finder (tray) | Done |
| Cache eviction (LRU + Clear Cache) | Done |
| Rename | Done |
| Tray auto-launch from Tauri app | Done |
| Production build + sign + notarize | Done |
| Version bump to 0.3.1 | Done |

---

## 2026-04-12 — Production build + remaining fixes

### Version 0.3.1 released

All components bumped to 0.3.1: package.json, tauri.conf.json, both Cargo.toml files,
both Info.plist files.

### Tray auto-launch on app startup

The Tauri app now calls `mountStore.launchAgent()` in `App.tsx` `onMount`, which:
1. Launches `mediamount-agent` (if not already running)
2. Launches `UFB.app` tray (if found in Resources)
3. Tray app registers FileProvider domains + starts agent

**Fix:** Tray launch path was still `MediaMountTray.app` → changed to `UFB.app`.

### Build + sign flow updated for FileProvider

**`build-macos.sh`:** Already updated with xcodegen + xcodebuild.

**`sign-and-notarize.sh`:** Updated to sign FileProvider extension:
- FileProviderExtension.appex signed with sandbox + app group entitlements
- UFB.app (tray) signed with app group entitlements
- Inside-out signing order: extension → tray → agent → main app
- Notarization: Accepted

### Cache DB migration fix

Existing cache DBs (created before hydration columns were added) failed to open
because `CREATE INDEX IF NOT EXISTS idx_hydrated ON known_files(is_hydrated)` ran
before the migration added the column. Fix: create tables without hydration columns,
migrate to add them, then create indexes.

### On-demand cache opening

FileOps server now opens cache DBs on demand when a request arrives for an unknown
domain. Uses `RwLock<HashMap>` for thread-safe dynamic insertion. Handles mounts
added while the agent is running.

### Frontend fixes

- Sidebar width: 220px → 250px (less truncation)
- Drives section filtered: mounts shown as bookmarks excluded from Drives list
- Mount toggle persistence: now updates both local state + saves to disk
- `saveConfig` updates `mountStore.configs` so bookmarks panel reflects changes immediately
- Sync mount bookmarks navigate to `~/Library/CloudStorage/UFB-{displayName}`
- `confirm()` dialog removed from Clear Cache (blocks in Tauri WebView)

### Windows build notes for 0.3.1

Shared code was modified in this session. Before building on Windows, verify:

1. **messages.rs** — New message types: `RenameItem`, `ClearCache`, `RecordEnumeration`,
   `GetChanges`, plus response types `RenameOk`, `RecordOk`, `ChangesResp`. All behind
   the shared `FileOpsRequest`/`FileOpsResponse` enums. macOS-only usage, but enums are
   compiled on all platforms. Run `cargo build -p mediamount-agent` on Windows to verify.

2. **mountStore.ts getMountPath()** — macOS sync path now returns
   `~/Library/CloudStorage/UFB-{displayName}`. Windows path unchanged
   (`C:\Volumes\ufb\{shareName}`). Conditional on `platform === "mac"`.

3. **App.tsx** — `mountStore.launchAgent()` now called on startup before `loadStates()`.
   On Windows this launches `mediamount-agent.exe`. Verify agent doesn't double-launch
   if already running as a Windows service or from the tray.

4. **SubscriptionPanel.tsx** — Drives section filtered via `filteredDrives()` to exclude
   paths already shown as mount bookmarks. On Windows, mount paths are `C:\Volumes\ufb\{share}`.
   Verify drive letters (C:\, D:\, etc.) are NOT accidentally filtered — they shouldn't
   match mount paths.

5. **SettingsDialog.tsx** — `mountPathMacos` added to `defaultMountConfig()`. Serializes
   as empty string. Verify config round-trip on Windows doesn't break.

6. **Sidebar width** — `initialSize` changed 220 → 250 in App.tsx. Cosmetic, all platforms.

7. **Clear Cache button** — `confirm()` dialog removed (blocks in Tauri WebView on macOS).
   If Windows needs a confirmation prompt, use Tauri's `dialog::ask()` API instead.

All macOS-specific Rust code is behind `#[cfg(target_os = "macos")]`. All frontend
platform checks use `platformStore.platform === "mac"`. Low risk but verify builds pass.

### Cache eviction — implemented

**Automatic LRU eviction:**
- Agent tracks hydration in `known_files` DB (is_hydrated, hydrated_size, last_accessed)
- After each `ReadFile`, agent checks total cached bytes against `syncCacheLimitBytes`
- If over budget, selects LRU victims and adds to `pending_evictions`
- `getChanges` response includes `evict` list → extension calls `evictItem()` for each

**Manual "Clear Cache" button:**
- Frontend sends `ClearSyncCache` via mount IPC
- Agent orchestrator posts `com.unionfiles.ufb.clear-cache.{domain}` notification
- Extension receives, lists root files, calls `evictItem()` for each
- Note: `confirm()` dialog in Tauri WebView was blocking the call — removed

**macOS cache DB schema (final):**
```sql
CREATE TABLE known_files (
    path TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    is_dir INTEGER NOT NULL DEFAULT 0,
    nas_size INTEGER NOT NULL,
    nas_mtime REAL NOT NULL,
    nas_created REAL NOT NULL DEFAULT 0,
    is_hydrated INTEGER NOT NULL DEFAULT 0,
    hydrated_size INTEGER DEFAULT 0,
    last_accessed REAL DEFAULT 0
);
```

---

## 2026-04-12 — Architecture decisions: FileProvider scope + Finder sidebar

### FileProvider for sync mounts only (not all mounts)

Explored making ALL mounts use FileProvider for unified Finder sidebar integration. Rejected
because FileProvider materializes files locally on access — every opened file gets cached on
disk. For sync mounts this is the point (NVMe speeds). For regular mounts (file copying
between departments) it would hoard files and waste disk space.

**Decision:** Keep the split:
- **Sync mounts** (`syncEnabled: true`): FileProvider + cache DB + FSEvents + sidebar under "UFB"
- **Regular mounts**: direct SMB + symlink at `/opt/ufb/mounts/` + appears under Finder Locations

### Finder sidebar favorites — no programmatic API

Researched adding regular SMB mounts to Finder sidebar programmatically:
- `LSSharedFileList` API: deprecated macOS 10.11, crashes on Tahoe (SIGSEGV)
- `sfltool`: can list sidebar items but not add them
- AppleScript: Finder has no sidebar scripting dictionary
- Direct plist manipulation: protected by `sharedfilelistd` daemon
- LucidLink (macFUSE-based): also relies on manual user drag to Favorites

**No supported public API exists on modern macOS.** Same situation as LucidLink, Dropbox
(non-FileProvider), and every other third-party app.

**Solution:** "Show Mounts in Finder" button in tray menu opens `/opt/ufb/mounts/` in Finder.
User can drag mount folders to sidebar Favorites as a one-time setup.

### Rename support added

`RenameItem` IPC message + handler. Extension's `modifyItem` checks `changedFields.contains(.filename)`
and calls `renameItem` instead of `writeFile`. Agent does `fs::rename` on NAS, updates cache DB.

---

## Future cleanup — rclone/WinFSP legacy (noted 2026-04-12)

Dropped rclone + WinFSP entirely when sync moved to CF API / FileProvider, but a few
dead references remain. Not urgent — safe to defer.

- `mediamount-agent/src/config.rs`: `rclone_drive_letter`, `rclone_mount_path`,
  `rclone_remote`, `max_rclone_start_attempts`, `extra_rclone_flags` fields are marked
  `Legacy: rclone (no longer used, silently ignored)` and preserved for deserializing
  old config files. Mirror fields exist in `src-tauri/src/mount_client.rs`. Can be
  removed once we're confident no user configs in the wild still carry them (or add
  a one-shot migration step).
- `src-tauri/target/release/rclone.exe`: stale build artifact. Not referenced by the
  Inno Setup installer (`installer/ufb_tauri_installer.iss`), so it doesn't ship.
  Will disappear on next `cargo clean`.
- `LICENSES/WinFSP-LICENSE.txt` and the corresponding `THIRD_PARTY_NOTICES.txt` entry
  were removed in this session.

---

## 2026-04-12 — v0.3.2

### Phonebook `endpoint.json` atomic write

`peer_manager.rs::register_endpoint` previously used plain `std::fs::write`, which
does create + truncate + stream. On high-latency SMB (observed over a WireGuard
tunnel) a mid-write interruption left the file partial/corrupt, and LAN peers
silently dropped the entry during `serde_json::from_str` in `discover_peers`.

Changed to tmp-file write + `std::fs::rename`. Matches the pattern already used by
`mesh_sync.rs::snapshot_to_db`.

Cross-platform: `peer_manager.rs` is shared code, applies to macOS automatically.

### Windows agent: SMB session awaited before Mounted

`orchestrator.rs::mount_drive` (Windows, regular/non-sync mounts) previously spawned
`establish_smb_session` as `tokio::spawn` fire-and-forget, so the `Mounted` state
fired before the SMB session was live. First-launch click on a mount bookmark
followed the symlink to a UNC with no session behind it; `listDirectory` failed
and `fileStore.ts::navigateTo` swallowed the error.

Changed to await the `spawn_blocking` session call before returning from
`mount_drive`.

macOS path already awaits `macos_smb_mount` synchronously — no change needed.

### Cross-VPN reachability observation

Ping from LAN to the VPN-pool client IP times out. TCP on port 49200 (mesh HTTP)
succeeds. Future debug should probe with `curl` / `Test-NetConnection -Port 49200`,
not `ping`.

### Version bump

0.3.1 → 0.3.2 in `package.json`, `src-tauri/Cargo.toml`,
`mediamount-agent/Cargo.toml`, `src-tauri/tauri.conf.json`,
`installer/ufb_tauri_installer.iss`.
