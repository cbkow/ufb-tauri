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

## Changelog

- **2026-04-16** — Initial notes during macOS Phase 4 work. Three
  alternatives evaluated; WinFsp ruled out (POSIX/NT semantic pain);
  ProjFS identified as the right target; port deferred until real
  Windows user pain motivates it.
