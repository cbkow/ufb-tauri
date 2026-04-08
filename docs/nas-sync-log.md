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
