# Windows WinFsp Port — Implementation Plan

## Context

The evaluation phase (see `docs/windows-io-backend-evaluation.md`) proved WinFsp via the SnowflakePowered `winfsp 0.12` crate is the correct Windows I/O backend: a 4 KiB user read arrives at our `read` callback as `length=4096`, bytes match SMB ground truth, no persistent placeholder state. This matches the "smart I/O interceptor, not a state-of-truth manager" pattern that made the macOS NFS3 loopback (shipped v0.4.1, 2026-04-16) succeed. ProjFS cannot serve this pattern — we proved it silently corrupts data under partial responses.

This plan executes the port. The macOS NFS implementation (`sync/nfs_server.rs` + `sync/macos_cache.rs`) is the architectural template — the Windows implementation should mirror its shape module-for-module, sharing `cache_core` / `conflict` / `connectivity` / IPC message types unchanged. The ProjFS branch (slices 0–5 already coded on `main`) is deleted.

Intended outcome (v0.5.0):
- WinFsp replaces ProjFS. Single-version cutover, no coexistence.
- User-facing path `C:\Volumes\ufb\{share}` stays the same but becomes a WinFsp mount point directly (junction layer gone).
- Metadata cache + block-level content cache working end-to-end, matching macOS behaviors (warm enum < 5 ms, partial-read scrub without full-file hydration, LRU eviction).
- UFB license changed MIT → GPL-3.0 to satisfy WinFsp's FLOSS exception.

## Architecture

The target shape mirrors macOS. Each concept on the left gets the same role on Windows:

| Concept | macOS | Windows (new) |
|---|---|---|
| VFS server | `sync/nfs_server.rs` (NFS3 loopback via `nfsserve`) | `sync/winfsp_server.rs` (WinFsp via `winfsp` crate) |
| Cache | `sync/macos_cache.rs` | `sync/windows_cache.rs` (simplified — remove CF-era bits, keep the ProjFS-era block columns) |
| Platform hooks | `platform/macos/` | `platform/windows/` (unchanged from today minus the deleted ProjFS bits) |
| Shared core | `sync/cache_core.rs`, `sync/conflict.rs`, `sync/connectivity.rs` | same, reused unchanged |
| Orchestrator | `orchestrator.rs` `#[cfg(target_os = "macos")]` | `orchestrator.rs` `#[cfg(windows)]` (rewrite around WinFsp mount/unmount) |
| Mount point | directory mount `~/ufb/mounts/{share}` (NFS kernel mount) | directory mount `C:\Volumes\ufb\{share}` (WinFsp reparse point) |

Single load-bearing simplification vs macOS: **the WinFsp `write` callback is synchronous and fires during the write**, same as NFS's `write`. So the write-through pipeline (`sync/write_through/` — coordinator / worker pool / ReadDirectoryChangesW watcher) that ProjFS needed for its async post-hoc notification model is **not needed by WinFsp either**. Windows collapses to the same "inline write + conflict check" shape macOS uses, with `sync/conflict.rs` helpers called directly from the `write` callback. `write_through/` gets deleted.

## Cleanup inventory (Slice 0)

From the exploration, the concrete deletion list:

### Files to delete entirely
- `mediamount-agent/src/sync/projfs_server.rs`
- `mediamount-agent/src/sync/write_through/` (whole directory — coordinator, worker, client_watcher)
- `mediamount-agent/run-spike.ps1`, `kill-spike.ps1`, `measure-spike.ps1`, `cold-measure.ps1`, `glenlivet-probe.ps1`, `probe-partial.ps1`, `probe-randomaccess.ps1`, `probe.ps1`, `partial-test.ps1`, `random-read.ps1`, `no-buffer-test.ps1`, `no-buffering-read.ps1`, `sparse-check.ps1`, `hydration-test.ps1`, `step1-test.ps1`, `step1-verify.ps1`, `verify-cache.ps1`, `find-file.ps1`, `list-files.ps1`, `list-zcb.ps1`, `list-deadline.ps1`, `survey-state.ps1`, `cleanup-all.ps1`, `kill-agent.ps1`, `run-agent.ps1`, `measure2.ps1`, `winfsp-inspect.ps1` (all session scratch)
- `docs/windows-projfs-plan.md` (superseded by `docs/windows-io-backend-evaluation.md`)
- `mediamount-agent/run-winfsp-spike.ps1`, `winfsp-partial-test.ps1` (spike artifacts — promote useful bits into `mediamount-agent/scripts/` if desired, otherwise drop)

### Code sections to delete
- `mediamount-agent/src/main.rs`:
  - Lines ~363–430 (Windows ProjFS provider startup block, starting with `Start ProjFS providers (Windows)`)
  - Lines ~556–596 (`--projfs-spike` and `--winfsp-spike` CLI flag handlers)
- `mediamount-agent/src/config.rs` lines ~225–234: `MountConfig::vfs_dir()` — obsolete, WinFsp mounts user-facing path directly
- `mediamount-agent/src/sync/mod.rs`: `pub mod projfs_server;` line(s), `pub mod write_through;` line, update header comment (macOS NFS / Windows WinFsp)
- `mediamount-agent/Cargo.toml`: `windows-projfs = "0.1"` dep
- `installer/ufb_tauri_installer.iss` lines ~303–310: ProjFS feature enable block (replaced by WinFsp MSI bundling in Slice 5)

### Code to simplify
- `mediamount-agent/src/sync/windows_cache.rs`: strip the ProjFS-era schema-migration conditionals (lines ~153–170). The `parent_path` / `chunk_bitmap` / `name` / `is_dir` / `nas_created` columns stay — they're what the new WinFsp backend will use — but move them from a migration block into the base `init_schema` DDL. No existing users to migrate from (Slice 0 is a hard cutover).
- `mediamount-agent/src/orchestrator.rs` `start_sync` / `stop_sync` — rewrite around WinFsp mount/unmount (see Slice 4 below). For Slice 0, just remove the junction-to-vfs_dir wiring; leave a stub that does nothing until Slice 4 fills it in.

### Confirmed gone already (no action)
- `sync/sync_root.rs`, `sync/filter.rs`, `sync/watcher.rs`, `sync/placeholder.rs` — CF API files, already deleted in v0.4.1
- `cloud-filter` crate dep — already removed

### Platform-neutral (no changes)
- `sync/cache_core.rs` — shared schema + bit ops
- `sync/conflict.rs` — sidecar generation
- `sync/connectivity.rs` — NAS status
- `sync/macos_cache.rs`, `sync/nfs_server.rs`, `platform/macos/` — macOS path
- IPC message types

## Approach

Six slices. Each ends at a shippable commit; Slice 0–2 are the minimum viable v0.5.0 (metadata-only cache), slices 3+ add content caching and polish.

### Slice 0 — ProjFS removal + GPL-3 relicense (1 day)

Single hard-cutover commit. Execute the cleanup inventory above. Also:

- `LICENSE`: replace MIT with GPL-3.0 text
- `Cargo.toml` `license = "GPL-3.0"` for the top-level crate and any workspace members that link WinFsp
- `README.md`: update license badge/statement
- `docs/windows-io-backend-evaluation.md`: stays as the decision record

**Exit:** `cargo build --release` clean on both `aarch64-apple-darwin` and `x86_64-pc-windows-msvc`; macOS still builds + runs normally; Windows has no sync backend (expected — Slice 1 adds it).

### Slice 1 — WinFsp server production-ize (3–4 days)

Promote `winfsp_server.rs` from spike to a proper `start(domain, nas_root, cache, ipc_tx)` entry point mirroring `nfs_server::start` at `sync/nfs_server.rs:1215`.

Concretely:
- Rename `start_spike` → `start`, take the same args shape as `nfs_server::start` (domain, nas_root, Arc<WindowsCache>, ipc_tx for future badge/IPC). Mount point is derived from `MountConfig::volume_path()`.
- Wire `NasHealth` loop into the provider (borrow the macOS one from `nfs_server.rs:60+` — it's largely platform-neutral; factor into `sync/connectivity.rs` or a new `sync/nas_health.rs` if cleaner).
- Keep callbacks at the minimum viable set for Slice 1: `get_security_by_name`, `open`, `close`, `read` (passthrough, no cache yet), `read_directory` (live `fs::read_dir`, no cache), `get_file_info`, `get_volume_info`. Write and cleanup are stubs returning `STATUS_INVALID_DEVICE_REQUEST` — added in Slice 4.
- Wire into `main.rs` `run_event_loop()` Windows branch: iterate sync-enabled mounts, resolve SMB UNC path, call `winfsp_server::start` for each. This is the Windows equivalent of the `#[cfg(target_os = "macos")]` block at `main.rs:271–361`.
- Delete the temporary `--winfsp-spike` CLI flag (added in the evaluation phase) — sync mounts are now driven by config.

**Exit:** `syncEnabled: true` mount in `mounts.json` causes WinFsp mount to appear at `C:\Volumes\ufb\{share}` on agent start. Directory browse works, file reads go to SMB per-op. No cache, no writes. `Get-ChildItem -Recurse` on a 200-item subtree completes without error (validates WinFsp handles the paths ProjFS choked on).

### Slice 2 — Metadata cache authority (2–3 days)

Warm enumeration served from SQLite, cold falls back to live `fs::read_dir` and populates cache. This is the Windows mirror of `macos_cache::record_enumeration` + `cached_children`.

- `WindowsCache::open(share, nas_root, cache_limit)` — same signature as `MacosCache::open`. DB lives at `%LOCALAPPDATA%\ufb\cache\{share}.db`.
- `record_enumeration(parent_rel, entries)`, `cached_children_by_parent(parent_rel)`, `cached_attr_by_path(rel)` — port the macOS implementations into `windows_cache.rs`. Mostly copy-paste with s/fh/rowid/ substitutions if needed (macOS uses `fh INTEGER PRIMARY KEY AUTOINCREMENT`; Windows can do the same).
- `PassthroughFs` holds `Arc<WindowsCache>`. `read_directory` checks `cached_children_by_parent` first; if folder not yet enumerated, do live `fs::read_dir` → `record_enumeration` → serve from cache.
- `get_file_info` serves from `cached_attr_by_path`; miss → stat SMB once and cache.
- Stat-on-open drift check with 30s TTL (mirrors `nfs_server.rs` `require_online` + `last_verified_at` logic).

**Exit:** second enum of a 200-item folder < 5 ms (SQLite-only). First enum populates cache; agent restart preserves cache (no re-enum required). SQLite file visible at the expected path.

### Slice 3 — Block-level content cache (3–4 days)

Port `nfs_server.rs:read_with_bitmap` to `winfsp_server.rs:read`. 1 MiB chunks, bitmap in `known_files.chunk_bitmap`, sparse cache files at `%LOCALAPPDATA%\ufb\cache\by_key\{rowid:016x}` (same layout as macOS).

- Per-rowid `RwLock` keyed map for reader/evictor coordination (mirrors `macos_cache::fh_lock`)
- Read callback: if fully hydrated, `seek_read` from cache file; else walk chunks, serve cached runs, fetch + cache missing runs from SMB
- `mark_fully_hydrated` when bitmap is complete (flip `is_hydrated=1`, null the bitmap)
- Eviction task, 30s tick, LRU over `cache_limit_bytes`, skip files with active reader locks (same design as `macos_cache::evict_over_budget_now`)
- Connect to mount_service IPC drain path (reuse `mount_service.rs` `try_drain_cache` — already platform-agnostic)

**Exit:** scrub through a 4 GB ProRes at offsets → chunks cached at touched offsets only, on-disk size ≪ logical size, SQLite bitmap reflects touched chunks. Agent restart mid-scrub preserves cache. Re-read of fully cached file < 100 ms for 1 GB.

### Slice 4 — Write path + conflicts (2–3 days)

Implement `winfsp_server::write` synchronously — matches macOS NFS `write` pattern. No coordinator / worker queue; the WinFsp callback IS the write.

- `write(ctx, buffer, offset, write_to_eof, constrained_io, file_info)`: conflict-precheck via `conflict::make_conflict_path` if cached metadata drifted from live SMB stat, write to SMB, invalidate cache entry, refresh file_info.
- `overwrite` / `set_file_size` / `create` / `can_delete` / `set_delete` / `cleanup` / `rename` — minimal implementations that pass through to SMB and invalidate cache.
- `ConflictDetected` IPC message sent to UFB on sidecar creation (same message type macOS uses).
- `orchestrator::start_sync` wires `sync_state` transitions the way `nfs_server.rs` does — `Registering → Active` on success, `Error` on failure, `Offline` on NAS heartbeat failure.

**Exit:** write a file from Notepad on the mount, verify it appears on the NAS. Concurrent edit from a second SMB client + local edit produces a `.conflict-{host}-{timestamp}` sidecar; both versions survive; `ConflictDetected` reaches UFB UI.

### Slice 5 — Installer + service integration (3–5 days)

- Installer (`installer/ufb_tauri_installer.iss`): bundle `winfsp-2.1.25156.msi`, silent-install it with `msiexec /i winfsp-*.msi /quiet`. Skip if the installed WinFsp version is ≥ ours (idempotent upgrade).
- Delete the ProjFS feature-enable block.
- CI build environment: install LLVM (for `winfsp-sys` bindgen) and the WinFsp Developer SDK (for import lib).
- `build.rs` stays as `winfsp::build::winfsp_link_delayload()`.
- Service integration: restart flow — if WinFsp MSI install requires reboot, surface a tray notification.

**Exit:** clean install on a fresh VM picks up WinFsp silently, UFB launches, sync mounts work without user intervention. Uninstall cleanly removes UFB (WinFsp itself may be shared with other apps — leave it alone).

### Slice 6 — Compatibility burn-in (1 week)

One-week dogfood period. Blocker-level issues found here get fixed before v0.5.0 ships.

- Open + scrub cold files in Premiere Pro, DaVinci Resolve, After Effects, Bridge from the WinFsp mount
- Save AE projects to the mount (tests oplock / share mode handling)
- Long-path / special-char paths (the Glenlivet MAX_PATH bug that killed ProjFS) — verify WinFsp handles them
- Windows Search Indexer + Recycle Bin behavior — suppress via VolumeParams flags if they generate pathological I/O
- Measure `Get-ChildItem -Recurse` latency on 200-item + 2000-item subtrees; compare to SMB UNC baseline; tune metadata cache / `pass_query_directory_filename` flag if needed

**Exit:** no regressions against the media workflows that work today on SMB UNC. Block-level hydration confirmed for a 19 GB RAW file scrub. Eviction converges on cache_limit.

## Critical files

**New:**
- Nothing — `mediamount-agent/src/sync/winfsp_server.rs` already exists from the spike, promoted in Slice 1

**Modified:**
- `mediamount-agent/src/sync/winfsp_server.rs` — incrementally across slices 1–4
- `mediamount-agent/src/sync/windows_cache.rs` — slice 2 (schema cleanup + warm-enum methods) and slice 3 (block-level)
- `mediamount-agent/src/sync/mod.rs` — remove `projfs_server` / `write_through`, update doc comment
- `mediamount-agent/src/main.rs` — replace ProjFS provider startup with WinFsp startup (slice 1)
- `mediamount-agent/src/orchestrator.rs` — rewrite `start_sync` / `stop_sync` Windows paths (slice 4)
- `mediamount-agent/src/config.rs` — drop `vfs_dir()` (slice 0)
- `mediamount-agent/Cargo.toml` — drop `windows-projfs`, keep `winfsp` (slice 0)
- `LICENSE`, `README.md` — MIT → GPL-3 (slice 0)
- `installer/ufb_tauri_installer.iss` — ProjFS block → WinFsp MSI bundling (slice 5)

**Deleted:**
- `mediamount-agent/src/sync/projfs_server.rs` (slice 0)
- `mediamount-agent/src/sync/write_through/` (slice 0)
- Session scratch scripts in `mediamount-agent/*.ps1` (slice 0)
- `docs/windows-projfs-plan.md` (slice 0)

**Reused unchanged (reference only):**
- `mediamount-agent/src/sync/nfs_server.rs` — architectural template
- `mediamount-agent/src/sync/macos_cache.rs` — cache template
- `mediamount-agent/src/sync/cache_core.rs`
- `mediamount-agent/src/sync/conflict.rs`
- `mediamount-agent/src/sync/connectivity.rs`
- `mediamount-agent/src/mount_service.rs` drain path (already generic)
- IPC message types

## Verification

**Per-slice gates** — see each slice's Exit section above.

**End-to-end gates before v0.5.0 ship** (after Slice 6 burn-in):
- Install from installer on a clean VM: WinFsp MSI silent-installs, UFB runs, sync mount appears at `C:\Volumes\ufb\{share}`, file browse + open works
- `Get-ChildItem -Recurse` on a real job folder (Glenlivet-class paths) completes without error — the MAX_PATH failure mode ProjFS had must not recur
- Scrub a 19 GB RAW file from Premiere — physical on-disk size of cache file grows incrementally with touched chunks, not full-file
- Offline the NAS (disconnect cable / kill SMB session) — mounted folder continues to serve fully-hydrated files from cache, partial files degrade gracefully without crashing Premiere
- Reconnect NAS — reads resume against live SMB. No manual remount required
- Kill the agent mid-write — next run recovers; no orphaned mounts, no .tmp.* sidecars left lying around
- Conflict test: edit same file from two hosts — `.conflict-{host}-{timestamp}` appears, both versions intact

## Out of scope

- Windows kernel driver signing / code signing (WinFsp driver is signed by its maintainer; we only ship user-mode binaries)
- Tauri frontend changes — sync toggle UI should work unchanged since it's operating on the same `syncEnabled` config field
- macOS changes — untouched
- Mesh sync integration for block fetching
- Background full-hydration worker (user access drives cache; future feature)
- Cloud Files API revival — decided against in evaluation phase; dead option
