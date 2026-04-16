# NFS Loopback Migration — Plan (2026-04-15)

Companion to `docs/nas-sync-log.md`. Living document; update as phases land.

## Why we're considering this

After Waves 1–4 shipped (v0.3.4 / v0.3.5), macOS directory-navigation latency
is still notably worse than plain SMB. Root cause is not fixable inside
FileProvider:

- Finder, Spotlight, QuickLook, `materializedItemsDidChange` drive the
  extension on their own schedule — we can't throttle.
- `NSFileProviderItem` + XPC + secure-coding is heavy per item.
- Extension is sandboxed; every op traverses extension → `fileproviderd`
  → XPC → extension → Unix socket → agent (four hops) before reaching NAS.
- Opaque `itemVersion` comparison invalidation is Apple's call.
- Framework is designed for 100ms+ cloud round-trips + offline semantics,
  not LAN media workflow.

An NFS3 loopback server, mounted by macOS's native kernel NFS client,
inverts the control: **Finder becomes a client of our server**. We decide
what to serve, how long the kernel caches attributes, and when to invalidate.
The server owns the request path; cache maintenance runs in decoupled workers.

LucidLink migrated macFUSE → FileProvider specifically because of kext
hostility on Apple Silicon. Their FileProvider ceiling is the same one we hit.
Pure NFS loopback needs no third-party kext (native Apple NFS client) and no
FileProvider framework tax.

## Goals

- Navigation latency matches plain SMB (sub-10ms warm folder).
- Request path does zero blocking work except one SQLite lookup.
- Cache maintenance (polling, hydration, eviction) runs in decoupled workers.
- No third-party kext, no user security prompts, no sandbox tax.

## Non-goals (v1)

- NFS 4.1 — NFS3 is simpler and macOS supports it natively.
- NLM advisory locking — skip with `nolocks` mount option.
- Block-level caching — whole-file cache for v1.
- Peer-to-peer block fetching — future mesh layer.
- Windows port — Cloud Files works well there; revisit only if we see the
  same pain pattern.

## Measurable success criteria

| Milestone | Metric | Target |
|---|---|---|
| Week 1 (Phase 0 exit) | passthrough-NFS nav vs plain SMB | ≤ 2× |
| Week 4 (Phase 1 exit) | warm folder nav | < 5 ms |
| Week 7 (Phase 2 exit) | 1 GB cache-hit re-open | < 100 ms |
| Week 10 (Phase 3 exit) | concurrent-write conflict detection | intact |
| Week 14 (Phase 4 exit) | 1-week burn-in | no crashes |
| Week 18 (Phase 5 exit) | FileProvider removed from build | clean build |

---

## Phase 0 — De-risk spike (Week 1)

**Goal:** answer "does NFS loopback actually feel faster?" before committing
to six weeks of engineering.

- New throwaway crate `mediamount-nfs-spike` (separate from agent; delete
  later if numbers don't land).
- Pull in `nfsserve` (xetdata/nfsserve, Rust NFS3 server, production-used
  by XetHub).
- Implement `NFSFileSystem` trait as **naive passthrough** to a local
  directory — no SQLite, no cache, no agent integration.
- Verify mount works:
  ```bash
  mount -t nfs \
    -o "port=12345,mountport=12345,nolocks,vers=3,tcp,nobrowse,actimeo=1" \
    localhost:/spike ~/ufb/vfs-spike
  ```
- Swap passthrough target from local dir to an existing SMB mount
  (`/Volumes/Jobs_Live-1`).
- Measure three-way on a known 100-folder / 1000-file subtree:
  - plain SMB (`/Volumes/Jobs_Live-1`)
  - current FileProvider (`~/Library/CloudStorage/…`)
  - NFS loopback (`~/ufb/vfs-spike`)
  - Metric: `time ls -R <path>` wall-clock.

**Go/no-go gate.** Justify Phase 1 only if:
- NFS passthrough within 2× plain SMB, AND
- current FileProvider significantly worse (≥ 5×).

If NFS passthrough is itself slow, the bottleneck is not Apple's framework
— abandon this plan. If FileProvider is already close, the rewrite isn't
worth it.

**Secondary questions answered in Phase 0:**
- Can we user-mount NFS without sudo? (`mount_nfs` flags, TCC prompts)
- Large-file support (> 4 GB)?
- Does QuickLook work against an NFS mount?
- Any Finder sidebar / Spotlight weirdness?
- Handle behavior on NAS-side changes during polling?

---

## Phase 1 — Metadata cache authority (Weeks 2–4)

**Goal:** warm-folder navigation served entirely from SQLite, zero
NAS round-trip on the request path.

- New module `mediamount-agent/src/nfs_server.rs`, wraps `nfsserve`.
- Schema migration: add `fh INTEGER PRIMARY KEY AUTOINCREMENT` to
  `known_files`. Stable across restarts; never reused; deleted rows stay
  (soft-delete) so old handles get `ESTALE`.
- Implement NFS ops from SQLite:
  - `LOOKUP` — `SELECT fh WHERE path = ?`
  - `GETATTR` — one row by `fh`
  - `READDIR` / `READDIRPLUS` — `SELECT * WHERE parent_path = ?`
    (indexed since Wave 3.2)
  - `FSINFO`, `ACCESS`, `STATFS` — trivial constants
- First-visit cold path: if `parent_path` has no rows, do live
  `fs::read_dir` on SMB, populate cache, return.
- Background metadata poller (tokio task): restats rows in
  `visited_folders`, diffs, updates `known_files`, bumps `generation`
  on changes.
- Mount opts: `vers=3,tcp,nolocks,actimeo=1,nobrowse`.
- NFS mount point lives at `~/ufb/mounts/<share>` — the same user-facing
  path as non-sync plain-SMB mounts. Toggling sync on a mount only swaps
  the backing at that path (symlink ↔ NFS mount point); bookmarks stay
  valid. A stale symlink at that path is unlinked before NFS mount.

**Exit criteria:** warm nav < 5 ms. Cold first-visit populates cache.
All `list_dir` SMB calls happen off the request path.

---

## Phase 2 — Block-level content cache (Weeks 5–7)

**Goal:** once a chunk of a file lives on local disk, reads of that chunk
come from disk — even mid-open, even when the user is scrubbing forward /
backward through a partially-hydrated media file.

**Why block-level instead of whole-file:**

Whole-file (is_hydrated true/false) means every first-read pays full SMB
cost and nothing is cacheable until download completes. For a 4 GB ProRes
clip a user scrubbed in yesterday, that's a hard no-go. Block-level means:

- Scrub to 75% of a file you opened yesterday → chunk at that offset is
  already cached → instant seek.
- Mid-open read of a brand-new file: chunks the user just read are cached
  immediately; chunks they haven't touched yet proxy SMB on demand.
- Background hydration (optional, later) fills the gaps so eventually the
  whole file is local.

**Chunk size:** 1 MiB. Matches our NFS `rsize=1048576` mount option, so
each NFS `READ` RPC typically maps to exactly one chunk — clean accounting,
no cross-chunk splitting in the hot path.

**Schema additions (`known_files`):**

- `chunk_bitmap BLOB NULL` — one bit per chunk, little-endian byte order.
  NULL until the file has at least one cached chunk. For fully-cached
  files the `is_hydrated=1` shortcut avoids a bitmap read.
- Computed from `nas_size`: `num_chunks = (size + CHUNK_SIZE - 1) / CHUNK_SIZE`.
  A 4 GB file → 4096 chunks → 512 bytes of bitmap.

**Content layout:** `~/ufb/cache/by_handle/{fh}` — sparse files. We only
write chunks we have. `pread` on a missing region would return zeros, so
the bitmap is authoritative for "is this chunk valid."

**NFS `READ(fh, offset, count)`:**

1. Load `(is_hydrated, chunk_bitmap, nas_size)` from `known_files`.
2. If `is_hydrated=1`: single `pread` on the cache file, return.
3. Otherwise split `[offset, offset+count)` into chunk-aligned runs:
   - Consecutive cached chunks → one `pread` on the cache file.
   - Consecutive missing chunks → one SMB read, then `pwrite` into the
     cache file + set those bits + persist bitmap. Bytes go to both the
     caller's buffer AND the cache.
4. If all chunks in the file are now set, flip `is_hydrated=1` and drop
   the bitmap column (null out to save space, is_hydrated=1 is the fast
   path anyway).

**Eviction (LRU):**

- `last_accessed` bumped on every read.
- When over `cache_limit_bytes`: evict the oldest file wholesale — delete
  cache file, NULL out bitmap, set `is_hydrated=0`, `hydrated_size=0`.
- Partial hydration survives agent restart (bitmap persisted in SQLite);
  eviction still wipes it cleanly.

**Explicit non-goal for v1:** background full-hydration worker. The user's
own access drives what gets cached. If they play a file linearly, the whole
file ends up cached as they watch. If they only scrub certain ranges, only
those are cached. A proactive background filler can come later if the
partial-cache pattern isn't good enough (measure first).

**Exit criteria:**

- Scrubbing (jumping around) a 4 GB file that's been touched before in the
  same region feels local-disk-fast.
- First linear read of a cold 1 GB file: each chunk after the first arrives
  faster (same read but now also writing to cache has negligible overhead
  vs plain SMB).
- Second linear read of the same file (now fully cached): pure local
  disk speed, << 100 ms.
- Agent restart mid-hydration: cached chunks are still honored, missing
  chunks still fetch from SMB.

---

## Phase 3 — Writes + correctness (Weeks 8–10)

**Goal:** writes work correctly, conflict detection remains intact.

- Implement `WRITE`, `CREATE`, `MKDIR`, `REMOVE`, `RMDIR`, `RENAME`,
  `SETATTR`.
- Write path: proxy to SMB authoritatively; on success, invalidate
  cache row (`is_hydrated=0`, bump `generation`).
- Rename: update `path` + `parent_path` in one transaction; `fh`
  unchanged (rename preserves the handle — clients keep working).
- Remove: soft-delete. Keep the `fh` row with `deleted=1` so old
  handles resolve to `ESTALE`, not to an unrelated new file.
- NFS write sync modes: support `UNSTABLE` + `COMMIT` (standard pattern:
  `UNSTABLE` writes stream to cache, `COMMIT` flushes through to SMB).
- Hook existing conflict detection into the write proxy path
  (sidecar-file behavior preserved).

**Exit criteria:** concurrent edit from our NFS + another SMB client
gets conflict-sidecar behavior we already have.

---

## Phase 4 — Hardening (Weeks 11–14)

**Goal:** not crash-prone; handles edge cases gracefully.

- **Persistent handle stability across agent restarts.** Our `fh` values
  (from the `nfs_handles` AUTOINCREMENT column) ARE stable on disk. But
  the `nfsserve` crate wraps them in an opaque `nfs_fh3` with a generation
  number derived from server startup time — so every agent restart
  invalidates every cached NFS handle in every connected kernel client,
  forcing a full `umount`/`mount` or silent stale-handle errors mid-use.
  Fix: override the default `id_to_fh` / `fh_to_id` in our `NFSFileSystem`
  impl to use a persistent generation (e.g. a fixed server-id loaded at
  startup, or just drop the generation field entirely and store the raw
  `fh` bytes). Validate by: agent restart + `ls` on the mount should work
  without remount. This is prerequisite to real-user dogfood.
- Stale handle semantics: deleted row + client still has old `fh` → return
  `NFS3ERR_STALE` cleanly (not panic).
- NAS disconnect handling: transient `EAGAIN` / `ETIMEDOUT` surface as
  NFS `JUKEBOX` (retry), not `EIO` (terminal).
- Permission errors: SMB `EACCES` → NFS `EACCES` passthrough.
- Large file tests (> 4 GB ProRes / RED raw samples).
- Sparse file tests (don't re-inflate on hydration).
- Symlink policy: pass through literally; don't resolve server-side.
- Clean shutdown: unmount NFS mount before exit (so mount doesn't linger).
- Integration with existing freshness sweep: invalidates SQLite rows,
  triggers poll.

### Explicit non-goal: mesh sync integration for the NFS layer

Considered and rejected. The existing mesh sync (UI-facing metadata
edits — subscriptions, column defs, item_metadata) is not battle-
hardened enough to carry filesystem cache state without inviting a
new class of crash/recovery edge cases. Propagating `known_files`
diffs or peer-to-peer block fetches across the mesh sounds like a
team-scale performance win on paper, but the reality is:

- Solo-user scenarios gain nothing.
- Small-team overlapping-workflow scenarios gain something (peer
  block fetch for large files is genuinely useful there), but the
  value doesn't justify spending weeks on a distributed-systems layer
  atop a mesh implementation that's still finding its sea legs.
- Every new cross-node write path is a new failure mode — partial
  update during peer crash, conflict between mesh-propagated state
  and local enumeration, staleness from an offline peer.

If real multi-user LAN pain shows up later, revisit. For now the
per-node cache + NAS is the source of truth and peers don't talk
about filesystem state.

**Exit criteria:** 1-week burn-in on one workstation. No crashes.
All tools in the user's media workflow function correctly.

---

## Phase 5 — Cutover (Weeks 15–18)

**Goal:** remove FileProvider from macOS build.

- Mount migration: `~/ufb/mounts/<share>` symlinks swap from FileProvider
  cloud path → NFS mount path. Users' muscle memory unchanged.
- Retire:
  - `mediamount-tray/FileProviderExtension/` (Swift extension).
  - `mediamount-tray/FileProviderExtension.entitlements`.
  - Xcode target for extension.
  - `AgentFileOpsClient.swift` (IPC client).
  - Wave 2's Swift connection pool.
  - `handle_list_dir` / `handle_stat` etc. in fileops_server.rs (replaced
    by NFS ops).
- Ship as **v0.4.0** — major version signals architectural change.
- Keep Windows Cloud Files path untouched.

---

## What we keep (most of Waves 1–4)

- `mediamount-agent` process and its mount orchestration (extended with
  `nfs_server` module; retains SMB mount logic).
- `known_files` schema (add `fh` column; everything else reused).
- SQLite pool (r2d2) from Wave 1.5.
- `prepare_cached` hot paths from Waves 1.4 / 4.1.
- `parent_path` indexed column from Wave 3.2 (now pulling its weight).
- Eviction + hydration tracking logic.
- Mesh sync layer.
- Conflict detection on write.
- Freshness sweep (simpler — just invalidates SQLite rows now).
- Entire `src-tauri` UI — untouched.

## What we retire (macOS only)

- FileProvider extension (Swift).
- AgentFileOpsClient IPC + Swift connection pool.
- FileProvider-specific cache path conventions (CloudStorage directory).
- Darwin notification-based cache invalidation (mesh sync covers this).

---

## Risks & decision points

| # | Risk | Mitigation | Decision point |
|---|---|---|---|
| R1 | `nfsserve` crate has protocol gaps | Read source in Week 1 | End Week 2 |
| R2 | macOS NFS client quirks (stale storms, aggressive caching) | Phase 0 spike tests this | End Week 1 |
| R3 | User-space NFS mount requires sudo | Phase 0 `mount_nfs` flag investigation | Week 1 |
| R4 | Some app depends on NLM locking | Verify with user's tools | Before Phase 3 |
| R5 | Apps broken by NFS semantics (xattr, ACLs, BOM files) | Test Resolve / Premiere / QuickTime in Phase 4 | Phase 4 |
| R6 | Performance ceiling is actually the NAS, not Apple's framework | Phase 0 passthrough measurement answers this | End Week 1 |

---

## Cross-platform outlook

- **Windows:** keep Cloud Files for now. If specific CF pain appears
  (admin elevation, sync-root quirks, metadata storms), the path
  forward is ProjFS — Microsoft's Projected FS, the architectural
  analog of macOS NFS loopback. Full notes in
  `docs/windows-port-notes.md`. (WinFsp evaluated + ruled out due to
  POSIX-to-Windows semantic pain; Windows NFS client isn't viable
  because it requires admin-gated optional-feature installation.)
- **Linux:** current FUSE setup works; NFS loopback is trivial there too.
  Migrate at leisure, post-macOS.

---

## Changelog

- **2026-04-15** — Plan authored. Waves 1–4 shipped in v0.3.4/0.3.5.
  FileProvider perf ceiling observed in real use. NFS loopback chosen
  over staying inside FileProvider, over macFUSE (kext hostility), and
  over FUSE-T (still a kernel-userspace hop layered on NFS anyway —
  cleaner to skip the middleman). Phase 0 spike next.
