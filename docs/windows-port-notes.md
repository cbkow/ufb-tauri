# Windows Port Notes — NFS-loopback architecture

Companion to `docs/nfs-loopback-plan.md`. Captures the strategy for
bringing the macOS NFS-loopback model to Windows when we choose to
revisit the current Cloud Files implementation.

**Not a plan of record yet.** No decision to port — this is a reference
for when real Windows user pain justifies it.

## The three Windows options we considered

### 1. WinFsp — ruled out

FUSE-like user-space filesystem for Windows. Active, Microsoft-tolerant,
Rust bindings (`winfsp` crate).

**Dealbreaker for us:** historical pain with POSIX-to-Windows semantic
translation (case sensitivity, symlinks, ACLs, reserved names). Even
with improvements over the years, it's a layer that forces us into a
translation mindset that's incompatible with our "own the stack" goal.

### 2. ProjFS — **the likely fit when we port**

Microsoft's Projected File System (Win10 1809+). Used by VFS for Git.
Lower-level and more focused than Cloud Files API.

**Architectural match to our macOS NFS loopback:**
- Kernel calls our provider's callbacks (`GetFileDataCallback`,
  `GetPlaceholderInfoCallback`, `StartEnumerationCallback`) when a user
  opens / reads / enumerates through a projected path.
- We return bytes — from cache if hit, proxied from SMB (with write-back
  to cache) if miss. Identical flow to our current block-level cache on
  macOS.
- Same conceptual inversion as NFS loopback: "OS projects our virtual
  tree, we feed bytes."

**Wins over Cloud Files (current Windows):**
- No sync-root ID registration.
- No admin elevation required for normal registration.
- Simpler placeholder semantics overall.
- Enumeration callbacks match what we already do in `cached_children`.

**What ProjFS isn't:**
- Not a protocol we own (unlike NFS loopback). Still callback-driven.
- Minifilter driver in the I/O stack — bugs in our provider can hang
  the virtualized tree (same class as FUSE risk).
- Windows-only; can't share code with Linux.

**Ships in Win10 1809+.** No external install required.

### 3. Windows NFS Client — not viable for production

Optional Windows feature. Requires admin to install, often blocked by
corporate IT, historically bad NFSv3 semantics (case sensitivity,
permission quirks). Shipping NFS loopback on Windows would mean making
"install the NFS optional feature" part of our onboarding. Nope.

## What ports cleanly from macOS (~80% of the code)

- `MacosCache` → rename, abstract behind trait: schema, SQL, UPSERT,
  `fh` via AUTOINCREMENT rowid, chunk bitmap — all path-string-pure,
  zero platform assumptions.
- `sync::conflict` (sidecar path, timestamp, hostname) — untouched.
- Block-level hydration logic, content cache-by-handle layout, eviction
  LRU — platform-agnostic.
- Handle-stability semantics and the soft-delete rules for fh.

## What changes (~20%)

- `PassthroughFs: NFSFileSystem` → a new `PassthroughProvider` that
  implements ProjFS's callback API via the `windows` crate (or a
  dedicated ProjFS binding crate).
- Registration / virtualization-root setup
  (`PrjStartVirtualizing`, `PrjMarkDirectoryAsPlaceholder`).
- Error-code translation: `nfsstat3::NFS3ERR_*` → `NTSTATUS`.
- Path handling: UTF-16, backslash-separated, case-insensitive default
  behavior to match NTFS semantics.
- Mount point semantics: virtualization root is a directory, not a
  drive letter; existing Cloud Files infrastructure likely mapped to a
  similar location — reuse the same layout.

## Rough effort estimate

- **3–4 weeks** to reach current-macOS parity (Phase 0 through Phase 3
  equivalent).
- **+1–2 weeks** for Windows-specific compatibility pass: Premiere, AE,
  Resolve on Windows; NTFS-specific quirks (alternate data streams,
  reparse points, short names).

## Decision framing

**Don't port yet.** Cloud Files on Windows is mature, deployed, and
works well enough. Migration to ProjFS is solution-searching-for-problem
unless and until specific CF pain shows up in real dogfood.

**When to revisit:**
- Cloud Files sync-root admin-elevation UX complaints.
- Cloud Files metadata-storm perf issues (same class we hit on
  FileProvider).
- Divergence between Windows and macOS behaviour becoming confusing for
  users or maintainers.

**When we do port:**
- The hard work is done — everything under `sync/macos_cache.rs`,
  `sync/conflict.rs`, and the non-trait-impl parts of
  `sync/nfs_server.rs` transplants with minimal changes.
- Swap the OS-facing surface (ProjFS provider) — same trick as the
  macOS FileProvider → NFS loopback move we just shipped.

## 2026-04-16 — Concrete pre-thinking after the macOS cutover shipped

With Slices 1–5 landed (NFS loopback is now the sole macOS backend in
v0.4.1), the mental model of "how does the agent serve a projected
filesystem" is well-exercised. This section captures how to translate
that mental model to Windows when we pick this up.

### Crate choice: `windows-projfs`

Three Rust crates wrap ProjFS:

- **`windows-projfs`** (GPL-2.0, github.com/WolverinDEV/windows-projfs) —
  **recommended**. Single `ProjectedFileSystemSource` trait, same shape as
  `nfsserve::NFSFileSystem`. `ProjectedFileSystem` struct manages
  lifecycle. Lands the nearest one-to-one mapping with our existing
  `PassthroughFs` impl in `mediamount-agent/src/sync/nfs_server.rs`.
- **`projfs`** (MIT) — alternative; evaluate only if GPL licensing is a
  problem for the agent binary (it isn't today; the agent is internal).
- **`projfs-sys`** — raw bindings, reach for only if both wrappers leak
  on our workload.

First move: vendor `windows-projfs` at a pinned revision, because it's
0.1.x and pre-stable. Keep the pin in sync with `nfsserve` (also 0.x).

### Trait mapping — NFS op → ProjFS callback

`PassthroughFs: NFSFileSystem` → new `PassthroughProvider: ProjectedFileSystemSource`.

| NFS op (what we have) | ProjFS callback (what we need) | Notes |
|---|---|---|
| `lookup(dirid, name)` | synthesized via `enumerate_directory`'s results | ProjFS enumerates on demand; no explicit lookup |
| `getattr(fileid)` | `get_file_info(path)` | Metadata query for a placeholder |
| `readdir(dirid)` | `enumerate_directory(path)` | Streams DirectoryEntry values |
| `read(fileid, offset, count)` | `get_file_data(path, offset, length)` | Hydration callback; fill the provided buffer |
| `write(fileid, offset, data)` | filter callback for file-modified notifications | Write-through proxied to SMB as before |
| `create(dirid, filename)` | CreateFile notification | Hook fires after user creates |
| `remove(dirid, filename)` | DeleteFile notification | Hook fires after user deletes |
| `rename(from, to)` | Rename notification | Hook fires after user renames |
| `setattr(fileid, attrs)` | BasicInfoChange notification | Hook on attr changes |

The lack of an explicit `lookup` is the biggest shape delta — ProjFS
drives enumeration top-down. Our cache-backed path enumeration
(`MacosCache::cached_children`) ports without shape change; just
iterate and yield `DirectoryEntry::File`/`DirectoryEntry::Directory`.

### What ports cleanly from the macOS code

- `mediamount-agent/src/sync/macos_cache.rs` — rename module; the
  schema (`known_files` with `fh INTEGER PRIMARY KEY AUTOINCREMENT`,
  `chunk_bitmap`, `dirty`-style hydration tracking) is filesystem-
  agnostic. Windows-side cache module is ~95% identical. Split into a
  shared `sync::cache_core` + `sync::macos_cache` / `sync::windows_cache`
  when porting.
- `mediamount-agent/src/sync/conflict.rs` — sidecar-name format,
  hostname detection, already cross-platform.
- Block-level chunk bitmap + `evict_over_budget_now` + the 30s tick
  spawned from `nfs_server::start` — port directly. Just relocate the
  spawn point into `projfs_server::start`.
- `drain_all`, `cache_stats`, `set_badge_tx` — mechanical copy.
- `MountService::try_drain_nfs_cache` rename to a backend-agnostic
  `try_drain_cache`; the mount-id-vs-share-name fallback we added
  still applies because the UI → agent message identity doesn't
  change across platforms.

### What does NOT port from macOS — actively avoid these mistakes

- **`SUN_LEN` socket-path gotcha**: Windows IPC uses named pipes, not
  Unix sockets. `\\.\pipe\MediaMountAgent` (which we already use on
  Windows) has no length constraint for practical purposes. No
  equivalent constraint.
- **App group container path (`~/Library/Group Containers/...`)**:
  macOS-specific. Windows ProjFS provider is launched by the system
  via the registered CLSID; no sandbox-crossing required. Agent and
  provider can share state via any user-owned path (we already use
  `%LOCALAPPDATA%/ufb/`).
- **`LSSharedFileList` for sidebar**: also macOS-specific. Windows
  Explorer's "Quick access" / navigation pane entries come from the
  Windows Registry or a shell namespace extension. Deferred — same
  "drag manually" story as macOS.
- **`/Volumes/<share>` convention**: macOS-specific. ProjFS roots
  live under a drive letter (e.g. `U:\`) or a mount point the user
  picks. Our Windows build already uses `C:\Volumes\ufb\{share}` via
  the cloud-files sync_root layer — reuse that directory layout for
  ProjFS; swap the backend, keep the path.
- **`LegacyDomainCleanup`**: there's no analogous per-app "domain"
  concept on Windows. Cloud Files `SyncRoot` registrations *do* need
  cleanup on upgrade — that path already exists in the Windows build
  and stays as-is.

### Finder-sidebar-style integration question

On Windows, "does the projected root appear in Explorer's navigation
pane?" has a cleaner answer: yes, automatically, because ProjFS roots
are real directories on a real drive. Same model as our `/Volumes/`
realization on macOS — the OS handles discovery. No bespoke sidebar
code to write.

### Rough slice plan (sketch, not committed)

Mirrors the macOS cutover structure:

1. **Slice 0 — spike**: new crate `mediamount-projfs-spike`, a bare
   passthrough provider over a local SMB mount. Measure Explorer
   directory-open latency vs plain SMB. Go/no-go gate: at most 2×
   plain-SMB, cold.
2. **Slice 1 — metadata cache authority**: `PassthroughProvider` reads
   from SQLite for enumeration / get_file_info. First-visit cold path
   does SMB `read_dir` and populates.
3. **Slice 2 — block-level content cache**: chunk bitmap, `get_file_data`
   serves from cache on hit, SMB on miss + write-through to cache.
4. **Slice 3 — writes + correctness**: filter callbacks for
   create/write/remove/rename. Existing `sync/conflict.rs` drives
   conflict sidecars.
5. **Slice 4 — UI drain + stats**: reuse the `DrainShareCache` /
   `GetCacheStats` IPC we already built. Frontend change: none.
6. **Slice 5 — cutover**: retire `sync/filter.rs` + the Cloud Files
   code path; ProjFS is the sole Windows backend.

Effort estimate (revised with macOS experience baked in):
- Slice 0: 2–3 days (mostly crate wrangling + measurement).
- Slices 1–4: 2 weeks if the cache-core extraction goes smoothly.
- Slice 5: 1 week (mechanical deletion plus compatibility pass
  against Premiere, Resolve, After Effects on Windows).
- **~3 weeks total**, down from the 3–4 week estimate above, because
  the Rust side is now mature and there's almost no redesign to do.

### Gotchas to surface early

- **ProjFS feature must be enabled**: `Enable-WindowsOptionalFeature
  -Online -FeatureName "Client-ProjFS"`. Admin-gated. Our installer
  needs to check and prompt (same UAC pattern the Cloud Files path
  uses today for sync root registration). Document in setup flow.
- **Placeholder API vs notification API**: `windows-projfs` abstracts
  both but we must not assume they're equivalent. Notifications are
  async and unordered; placeholder callbacks are synchronous. Write
  paths use notifications → our write-through code needs to be
  idempotent under out-of-order delivery (it already is, by
  construction — SMB is the source of truth).
- **Case sensitivity**: NTFS is case-insensitive by default. Our
  cache schema uses case-sensitive paths (`path TEXT NOT NULL UNIQUE`).
  When porting, decide:
  - keep case-sensitive and normalize at the boundary (simpler DB,
    more boundary translation), or
  - COLLATE NOCASE on the path index (one-line schema change,
    subtle cross-platform divergence risk).
  Lean option 1. Explicit is better.
- **Alternate data streams + reparse points**: ProjFS passes these
  through natively via its framework. We don't touch them; SMB does.
  Test that our cache doesn't accidentally hash them into chunk
  bitmaps.
- **Windows Defender real-time scan**: new hydrations will fire AV
  scans. That pushes latency during first-read. FileProvider had the
  same problem. Document "add `%LOCALAPPDATA%/ufb/cache/` to Defender
  exclusions" in the ops runbook.

### Related crate links

- [windows-projfs on docs.rs](https://docs.rs/windows-projfs/)
- [windows-projfs on crates.io](https://crates.io/crates/windows-projfs)
- Alternative: [projfs on crates.io](https://crates.io/crates/projfs) (MIT)

## Changelog

- **2026-04-16** — Initial notes during macOS Phase 4 work. Three
  alternatives evaluated; WinFsp ruled out (POSIX/NT semantic pain);
  ProjFS identified as the right target; port deferred until real
  Windows user pain motivates it.
- **2026-04-16 (later)** — After macOS NFS cutover v0.4.1 shipped,
  added concrete crate recommendation (`windows-projfs`), op-to-
  callback mapping table, what-ports-what-doesn't from the macOS
  work, and a refined slice plan informed by what we actually
  built rather than what we planned to build.
