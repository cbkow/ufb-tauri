# Windows I/O Backend Evaluation

Living progress log for the post-cutover architecture decision. Supersedes `docs/windows-projfs-plan.md` as the plan of record.

## Status

- **Phase:** Step 0 complete → Step 1 next (ProjFS truncated-response test)
- **Last updated:** 2026-04-17
- **Branch:** `main` (ProjFS slices 0–5 coded, junction-wiring fix committed this session)
- **Decision horizon:** after Step 2 completes or Step 1 succeeds

## Context

The macOS side shipped v0.4.1 (2026-04-16) replacing FileProvider with a user-space NFS3 loopback VFS. That architecture works because the NFS server is a **smart I/O interceptor**, not a **state-of-truth manager** — each NFS op is answered on the fly (serve from cache or pass through to SMB), no persistent per-file hydration state is tracked on the client, no offline-reconciliation database exists. This is what made the cutover succeed where CF API historically struggled: the "reconcile client state with server state after an offline period" problem is structurally absent.

The Windows ProjFS port (6 slices, coded in one session 2026-04-16) assumed the same pattern was achievable on ProjFS. This session's empirical testing shows it is not. ProjFS is a placeholder-state machine that tracks hydration per-range in NTFS reparse metadata and forces us to own reconciliation — the same class of problem we abandoned CF to escape. Before either accepting ProjFS's model or pivoting architectures, this doc drives the evaluation.

## Framing principle (load-bearing)

> "The macOS NFS loopback works because it's a smart I/O interceptor, not a state-of-truth manager. This is what we are trying to accomplish." — user, this session

> "We will never be able to do, you and I, [build] a full database layer that tracks every possible file state on a server and efficiently catches up to offline changes after being offline for a duration. That is not a great use of local resources. That is a server task, and that is why our cloud API never really worked out."

Any Windows backend must satisfy this principle to be considered viable.

## Session findings (2026-04-17)

### File size distribution (from user)

- 5 MB JPEGs → 300 MB EXRs → 19 GB RAW video. Partial/block-level hydration is load-bearing for the large end.

### Latency gate — cold recursive enum, ProjFS spike vs plain SMB

| Subtree | SMB cold | ProjFS cold | Ratio |
|---|---|---|---|
| Progressive-Casestudy (9 items) | 113 ms | 17 ms | 0.15x |
| Glenlivet (265 items, recursion fails — see #4) | 268 ms | 84 ms\* | 0.31x |
| arm-and-hammer (9 items) | 35 ms | 11 ms | 0.31x |
| Breyers/postings (202 items) | 279 ms | 72 ms | 0.26x |

Gate was ≤ 2× — ProjFS passes 0.15–0.31×. \*Glenlivet's 84 ms is time-to-failure due to the MAX_PATH recursion bug (#4), not time-to-completion.

### Warm re-enum

Once SMB kernel cache is warm: plain UNC 13–14 ms; ProjFS spike 99–100 ms. ~7× gap is pure per-call `fs::read_dir` overhead in spike mode. Cache-backed (Slice 2 metadata cache) closes this in cache-backed mode.

### Metadata cache (Slice 2) — validated

Running the full agent with `syncEnabled: true` on `gfx-nas` pointing at `\\192.168.1.170\Jobs_Live`:

- Schema migration applied: `parent_path`, `chunk_bitmap`, `name`, `is_dir`, `nas_created` columns added cleanly.
- DB at `%LOCALAPPDATA%\ufb\cache\Jobs_Live.db` populates on browse.
- `known_files` tracks entries with correct parent/child wiring. 21 root entries + subdirs populating as user enumerates.
- `visited_folders` correctly gates warm vs cold enum.

### Block-level hydration (Slice 3) — does NOT work as designed

**Critical finding.** Three tests at progressively lower user-space caching tiers:

| Test | Read size | Read offset | Result on-disk | `stream_file_content` log |
|---|---|---|---|---|
| FileStream buffered | 64 KiB | 0 | Full 16.8 MB hydrated | offset=0, length=16,801,061 |
| FileOptions.RandomAccess | 1 MiB | 1.5 GB | Full 2.99 GB hydrated | offset=0, length=2,994,203,959 |
| `FILE_FLAG_NO_BUFFERING` | 4 KiB | 150 MB | Full 300 MB hydrated | offset=0, length=300,879,736 |

Even bypassing NTFS cache manager entirely (`NO_BUFFERING`), ProjFS's filter driver inflates the callback to full-file on first hydration. Every test produced a **single** `stream_file_content` callback with `offset=0, length=full_file_size`.

Our `by_key/` cache files match full logical size (non-sparse, fully allocated). The `chunk_bitmap` code path runs but is meaningless — every file ends up fully hydrated in one pass. The block-level design from `docs/windows-projfs-plan.md` is effectively dead code on Windows.

### Research conclusion (via general-purpose agent, 2026-04-17)

Per Microsoft docs and VFSForGit reference:

- ProjFS per-range hydration is a real kernel feature — the filter driver tracks hydrated extents and only callbacks for unwritten ranges.
- But the kernel decides callback-range size, not us. NTFS cache manager + ProjFS's own placeholder-hydration policy coalesce small user reads into full-file IRPs when the placeholder has never been touched.
- `FILE_FLAG_NO_BUFFERING` bypasses the cache manager but NOT ProjFS's own hydration policy.
- Microsoft explicitly steers slow/remote backends to **CF API**, not ProjFS. ProjFS is for "high-speed backing stores" (VFSForGit's local-ish Git object store).
- VFSForGit gets away with small callback ranges because Git objects are small — they fit within the coalescing window.

Citations collected in agent result (session log):
- https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/nc-projectedfslib-prj_get_file_data_cb
- https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/nf-projectedfslib-prjwritefiledata
- https://learn.microsoft.com/en-us/windows/win32/projfs/providing-file-data
- https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/ne-projectedfslib-prj_file_state
- https://learn.microsoft.com/en-us/windows/win32/projfs/projected-file-system

### Junction wiring (Slice 5 loose end) — fixed this session

`orchestrator::start_sync` was pointing the junction at the old CF cache dir (`%LOCALAPPDATA%\ufb\sync\{share}`) instead of the ProjFS VFS root (`%LOCALAPPDATA%\ufb\vfs\{share}`). Added `MountConfig::vfs_dir()` helper at `config.rs`; both `main.rs` and `orchestrator.rs` now consume it. Compiles clean.

### Known issue unrelated to decision: recursion bug (#4)

`Get-ChildItem -Recurse` on some subtrees (Glenlivet, parts of Breyers) fails with "The system cannot find the file specified" after a few levels. Likely MAX_PATH (260-char limit; Glenlivet has 262-char paths) plus possibly special chars (`&`, `'`, `(Footage)`). Tracked separately. Orthogonal to architecture choice — WinFsp would need to handle long paths correctly too; ProjFS currently doesn't.

## Plan

Three sequential evaluation steps, stop when one yields a clean verdict. Full plan at `C:\Users\chris\.claude\plans\groovy-splashing-muffin.md`.

### Step 1 — ProjFS truncated-response hypothesis (planned, ~0.5 day)

**Hypothesis:** If we respond with partial `PrjWriteFileData` (cap at 1 MiB even when callback asks for more), ProjFS's kernel extent tracking re-issues callbacks for unwritten ranges on subsequent reads.

**Method:** vendor `windows-projfs` 0.1.7 locally, patch `get_file_data_callback`'s write loop to stop after 1 MiB, rerun `no-buffer-test.ps1`, check `GetCompressedFileSize`.

**Success criterion:** on-disk ≤ 2 MiB after one 4 KiB read + completed read + subsequent reads at different offsets trigger new callbacks.

**Expected outcome:** failure. Worth testing because success eliminates a multi-week WinFsp port.

**Even if successful — still doesn't solve state-of-truth problem.** Partial hydration via ProjFS still means NTFS placeholder state persists across restarts, still means reconciliation-after-offline is our problem. Step 1 is a narrow test of the hydration mechanics, not the architecture.

### Step 2 — WinFsp native-API spike (planned, 3–5 days, only if Step 1 fails)

Spike behind `--winfsp-spike` flag, parallel to `--projfs-spike`. Native `FSP_FILE_SYSTEM_INTERFACE` **not** FUSE compat (FUSE is what bit rclone-on-Windows).

**Measurements:** enum latency gate (≤ 2× SMB), partial-read allocation check, compat sanity (Notepad roundtrip, Explorer rename).

**Gate:** all three must pass.

### Step 3 — Decision

Append **Decision** section to this doc with recommendation + licensing + port sketch. Actual port is a separate plan.

## Step 1 status — FAILED (2026-04-17)

**Hypothesis rejected.** Partial `PrjWriteFileData` response does not trigger re-callbacks; it causes silent data corruption.

**What we did:**
- Vendored `windows-projfs` 0.1.7 into `mediamount-agent/vendor/windows-projfs/`.
- Patched `fs.rs::get_file_data_callback` to cap `effective_length = length.min(1_048_576)` — only the first 1 MiB of each callback-range is written via `PrjWriteFileData`; the rest is dropped.
- Confirmed patch fired via log: `[windows-projfs patched] get_file_data: requested=300879736, capping to 1048576`.
- Primed the 300 MB `DeadlineClient-10.3.0.9-windows-installer.exe` placeholder by enumerating its containing folder.
- Read 4 KiB at offset 150 MB via `FILE_FLAG_NO_BUFFERING` → on-disk allocation went from 0 to 1,048,576 bytes (exactly our cap). Looked encouraging.
- Read 4 KiB at offset 50 MB via same API → on-disk unchanged. **No new callback fired** (only one `stream_file_content` log entry total).
- Verified the bytes returned for the second read:

| Source | Offset 50 MB, first 32 bytes |
|---|---|
| VFS (ProjFS placeholder) | `0000000000000000000000000000000000000000000000000000000000000000` |
| SMB UNC (ground truth) | `70c3d714224c4cf55efa3621dc614a9db07fb8d826c53087d015f7d145417cd7` |

**Conclusion:** ProjFS tracks placeholder hydration at the **callback-range granularity**, not the actual-bytes-written granularity. Once our callback returns `Ok` for `[offset=0, length=300MB]`, the kernel marks the entire 300 MB range as "hydrated" even though we only wrote bytes `[0, 1MB]`. Reads into the un-written 299 MB return **zeros** — no re-callback, no error, no indication to the user. This is silent corruption.

This confirms the deeper architectural reading: **ProjFS is a placeholder state machine.** The state (hydrated-or-not) is tracked per callback-range, not per byte. The public API gives us no way to split callbacks or extend hydration incrementally. Even the crate-level hack we tried produces broken files.

Combined with the full-file-callback behavior from earlier tests, the picture is complete: ProjFS cannot serve the "smart I/O interceptor" pattern our macOS architecture relies on. Moving to Step 2.

**Cleanup performed:** vendoring reverted; `Cargo.toml` back to registry dep; `vendor/windows-projfs/` deleted. Branch is clean of the experiment.

## Step 2 status — SUCCEEDED (2026-04-17, later)

**Architecture proven.** Switched from `winfsp_wrs` (Scille) to `winfsp` 0.12.4 (SnowflakePowered) after installing LLVM (for `winfsp-sys`'s bindgen) and the WinFsp Developer SDK. New crate's dispatch works cleanly on first try — the earlier failure was specific to `winfsp_wrs`'s trampoline wiring.

### Configuration changes

- `Cargo.toml`: `winfsp_wrs` → `winfsp = { version = "0.12", features = ["debug", "system"] }` (`windows` crate stays via other deps)
- `build.rs`: `winfsp::build::winfsp_link_delayload()` replacing `winfsp_wrs_build::build()`
- `mediamount-agent/src/sync/winfsp_server.rs` — rewritten for `FileSystemContext` trait (different shape from `FileSystemInterface`: no `XXX_DEFINED` constants, `file_info` by `&mut` out-param, DirBuffer-based enumeration, FspError is constructible via `NTSTATUS(i32)` variant).
- Environment: `LIBCLANG_PATH=C:\Program Files\LLVM\bin` required at build time; runtime uses just the WinFsp DLL.

### Directory mount verified

WinFsp supports mounting at a directory path as well as a drive letter. Tested both:
- `N:` (drive letter): works, labelled `ufb-winfsp-spike` in `Get-Volume`.
- `C:\Volumes\ufb\Jobs_Live` (directory): works, reparse tag `0xa0000003` (`IO_REPARSE_TAG_MOUNT_POINT`).

**This eliminates the junction layer from the target architecture.** WinFsp mount point IS the user-facing path — no need for `%LOCALAPPDATA%\ufb\vfs\{share}` + junction. `config::vfs_dir()` helper goes away. `orchestrator::start_sync` gets simpler.

### Partial-read test (the load-bearing architectural question)

Setup: WinFsp mount at `C:\Volumes\ufb\Jobs_Live` → `\\192.168.1.170\Jobs_Live`. Fresh 300 MB `DeadlineClient-10.3.0.9-windows-installer.exe`. Read 4 KiB at offset 150 MiB using `FILE_FLAG_NO_BUFFERING`.

**Our callback log:**
```
offset=157286400 length=4096
```

One read for exactly 4 KiB at exactly 150 MiB. Plus ancillary reads in the 64 KiB-chunk range (at offsets 800 KB – 2 MB) for Windows Defender scanning the PE header — max 64 KiB per callback, never whole-file.

**Bytes returned:**
```
VFS: 193dd3b04eaadd5371c077b15237c28e4307ff661aff4afc5f7ebf6c0182c028
SMB: 193dd3b04eaadd5371c077b15237c28e4307ff661aff4afc5f7ebf6c0182c028
match: True
```

Real data, not zeros. No placeholder-state-machine bookkeeping. No on-disk cache by default (we answer every read from SMB — a future cache layer is our code's decision, not forced by the API).

### Comparison vs ProjFS

| Question | ProjFS (tested Step 1) | WinFsp (tested now) |
|---|---|---|
| 4 KiB user read arrives as | `length=300879736` (full file) | `length=4096` |
| Unwritten ranges | return zeros (corruption) | trigger another callback |
| Per-range callback control | No — one-shot per file | Yes |
| Persistent placeholder state | Yes (NTFS reparse) | No |
| Reconciliation-after-offline problem | Yes, owned by us | No — each read is fresh |
| Matches macOS NFS loopback semantics | No | **Yes** |

### Open items before a full port

These aren't blockers for the decision, but need attention in the port phase:

1. **Latency gate on enumeration.** Haven't timed `Get-ChildItem -Recurse` on WinFsp yet; the research doc flagged "many-entry directory listing" as WinFsp's weakest spot. Should measure against SMB/NTFS before ship. Mitigation is a metadata cache (we have `cache_core` ready).
2. **Compatibility pass.** Notepad/Explorer/Premiere on a WinFsp mount point — some apps care about oplocks, ADS, case sensitivity. Needs dedicated testing with real workflows.
3. **Sync toggle semantics.** The macOS flow uses `host.unmount()` + `host.mount()` to switch sync on/off. WinFsp has the same pattern. `orchestrator::start_sync` + `stop_sync` re-implement around this.
4. **Licensing switchover.** UFB `Cargo.toml` + `LICENSE` + README need to go from MIT → GPL-3.0 before any code that links WinFsp ships. Dep audit already confirmed no GPL-3-incompatible transitive deps (only `windows-projfs` GPL-2.0-only, which goes away when we remove the ProjFS path).

### Decision input

WinFsp via SnowflakePowered `winfsp` crate is the clear winner over ProjFS for this use case. It's the native Windows equivalent of the macOS NFS3 loopback — same architectural pattern, same per-op control, no state-of-truth database to maintain.

## Step 2 status (earlier attempt — kept for record)

**Licensing resolved:** user confirmed UFB will ship under GPL-3.0 (currently MIT). That satisfies the WinFsp FLOSS exception, no commercial license needed. Only blocking dep is `windows-projfs` (GPL-2.0-only) which is removed when ProjFS backend goes away anyway.

**Spike infrastructure built:**
- `winfsp_wrs = "0.4"` + `winfsp_wrs_build = "0.4"` deps added, build.rs wired for delay-load
- `winfsp-x64.lib` vendored from `winfsp-sys-0.12.1+winfsp-2.1` cache at `mediamount-agent/vendor/winfsp/lib/` (WinFsp runtime-only install on this machine has no SDK)
- `mediamount-agent/src/sync/winfsp_server.rs` — pass-through FileSystemInterface impl with `GetSecurityByName`/`Open`/`Close`/`Read`/`ReadDirectory`/`GetFileInfo`/`GetVolumeInfo`/`Create` callbacks
- `--winfsp-spike <mount_path> <nas_root>` CLI flag in `main.rs` paralleling `--projfs-spike`
- Spike compiles clean, mounts at drive letter (N:) without error per both our log and WinFsp's own debug log

**Callback dispatch blocked — architecture unproven.** With the WinFsp debug feature enabled, we see the kernel IS receiving Create IRPs for the mount, and the dispatcher IS returning responses, but **every request returns `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010) before our Rust callbacks run**. Not a single `log::info!` from inside our callbacks ever fires — neither `get_security_by_name`, `open`, `create`, nor `get_volume_info`. Windows surfaces this as "Incorrect function" when you `dir N:\`.

Sample from WinFsp debug:
```
mediamount-agent[TID=5ff4]: FFFFD38599230BB0: >>Create [UT----] "\", FILE_OPEN, CreateOptions=21, ...
mediamount-agent[TID=5ff4]: FFFFD38599230BB0: <<Create IoStatus=c0000010[0]
```

Minimal-params setup per the Scille memfs/minimal examples (mimicking `set_file_system_name(mount)`, `set_prefix("")`, default everything else) did not change the outcome. Setting `CREATE_DEFINED=true` with a stub returning `STATUS_ACCESS_DENIED` also did not help.

**Likely root cause:** the vendored `winfsp-x64.lib` came from the SnowflakePowered `winfsp-sys` crate's cache (since we have no SDK install) but `winfsp_wrs_sys` may have been compiled expecting a different .lib ABI from the Scille side. Different `FSP_FILE_SYSTEM_INTERFACE` layout between the two crate families would cause silent function-pointer corruption — which matches the symptom exactly (mount succeeds, kernel IRPs arrive, user-mode callbacks never invoked).

**What we learned empirically:**
- WinFsp 2.1 runtime IS correctly installed on this machine.
- Kernel-level mount setup works: Windows registers the drive letter, the service routes IRPs.
- The `winfsp_wrs` crate on crates.io builds without libclang (welcome) but assumes the full WinFsp SDK is installed (unwelcome — the crate's `build.rs` expects `<WinFsp>/lib/winfsp-x64.lib`).
- Cross-vendored .lib from the other crate family appears to not be ABI-compatible in practice.

**What we did NOT learn:**
- Whether WinFsp's per-operation callback model actually delivers partial-range reads (the goal of Step 2).
- Whether latency enum numbers are in the right ballpark.
- Whether Notepad/Premiere compat works.

All of these were gated on the callbacks running, which they don't.

### Unblocking paths (ranked)

1. **Install the full WinFsp developer SDK** (ships headers + import lib in the same MSI, just a different feature set). The MSI at https://github.com/winfsp/winfsp/releases/latest has a "Developer" feature. Admin install, ~1 minute. Most likely fix — gives us the authoritative .lib. Requires user action.
2. **Switch to SnowflakePowered `winfsp` crate.** Native-API, recently updated, GPL-3 (compatible with our planned relicense), passes WinFsp's own conformance tests via ntptfs. Requires installing LLVM (libclang) so `winfsp-sys` can run bindgen at build time. Also requires some API migration since the trait shape differs from `winfsp_wrs`.
3. **Hand-write bindings** against the installed DLL using dynamic `LoadLibraryW` + `GetProcAddress`. Most control, most work. Appropriate if #1 and #2 don't work.
4. **Test the Scille memfs example** as a sanity check: clone https://github.com/Scille/winfsp_wrs, `cargo run -p memfs` on this machine. If it works → our .lib vendoring is the culprit, pursue #1. If it doesn't → this machine's env has a deeper issue.

**Recommended:** #4 first as a cheap sanity check, then #1 if that confirms the theory. Each is a few minutes of work.

### What this does NOT change about the architecture question

The research in the first half of Step 2 still stands: WinFsp's `FSP_FILE_SYSTEM_INTERFACE` is a per-op callback model with `(offset, length)` reads, no persistent placeholder state, used by production software (Parsec, rclone, Cryptomator). The architectural fit for "smart I/O interceptor, not state-of-truth manager" remains correct. The spike stall is an integration issue, not an architecture-disqualification.

Before writing spike code, a research pass on the Rust WinFsp ecosystem surfaced a go/no-go gate we can't bypass.

### Environment check (done)

- WinFsp **v2.1.25156** already installed on dogfood workstation at `C:\Program Files (x86)\WinFsp\`.
- WinFsp.Launcher service: Running.
- All three arch variants present: `winfsp-x64.dll` / `winfsp-a64.dll` / `winfsp-x86.dll`.
- **No SDK installed** (headers/libs absent) — Rust crate would need to bundle bindings or dynamically load the DLL. Both viable.

### Rust crate landscape

| Crate | Version | Maintainer | License | Notes |
|---|---|---|---|---|
| `winfsp` (safe wrapper) | 0.12.4+winfsp-2.1 | chyyran/SnowflakePowered | **GPL-3.0** | Most complete. Passes WinFsp conformance via ntptfs. 55★, solo maintainer. |
| `winfsp-sys` | 0.12.1+winfsp-2.1 | chyyran/SnowflakePowered | MIT-like | Raw bindgen. Same repo. |
| `winfsp_wrs` | 0.4.1 | Scille (Parsec) | MIT | Used by Parsec (real shipping product, but Parsec is GPL itself so it qualifies for the FLOSS exception). |

Neither crate is "production-grade" in the stronger sense (widely adopted, multiple maintainers). `winfsp` is the better-maintained safe wrapper but itself GPL-3.0. `winfsp-sys` is the cleaner FFI-only path (MIT-like), requires us to write our own safe wrapper.

### WinFsp upstream license — **the blocker**

WinFsp's `License.txt` (https://github.com/winfsp/winfsp/blob/master/License.txt):

> "As a special exception to GPLv3 ... Permission to link with a platform specific version of the WinFsp DLL ...
>
> These permissions (and no other) are granted provided that the software:
> 1. Is distributed under a license that satisfies the Free Software Definition ... or the Open Source Definition ...
> 3. **Is not linked or distributed with proprietary (non-FLOSS) software.**
>
> Commercial licensing options are also available: Please contact Bill Zissimopoulos <billziss at navimatics.com>."

**If UFB is distributed as proprietary software to customers, the FLOSS exception does not apply.** Paths forward, in order of realism:

1. **Commercial license from Navimatics.** Email billziss@navimatics.com for a quote. Many proprietary Windows filesystem-using apps go this route.
2. **Internal-use-only interpretation.** GPL applies on *distribution*. If UFB is internal tooling at a single company with no external distribution, the GPL obligations don't trigger. Needs legal review before relying on.
3. **Open-source UFB under a GPL-compatible license.** Largest scope change.
4. **Skip WinFsp.** Accept ProjFS full-file hydration + UI pre-hydration affordance. Documented as the fallback in the original plan.

### Performance data (for when/if we proceed)

Per WinFsp's own benchmarks (ntptfs passthrough vs NTFS, NVMe):

- **Cached read/write:** WinFsp ≥ NTFS (fast I/O path wins).
- **Directory listing (many entries):** NTFS > NTPTFS — author acknowledges this is a weak spot for WinFsp. **Our 200+ entry media directories will feel this.** Mitigation: cache enumerations in our wrapper, same as we're already doing in `cache_core` for the macOS NFS side.
- **Repeated file open:** NTFS > NTPTFS, "distant third" — user-mode round-trip per open is inherent to the WinFsp design.
- **Sequential large reads:** Cached-read numbers suggest WinFsp matches NTFS. 19 GB RAW scrubbing workflow should be fine.

No credible benchmark exists for WinFsp vs SMB on remote media workloads — we'd measure in the spike.

### Tentative spike plan (deferred)

Once licensing is resolved, the spike uses:
- Crate: `winfsp-sys` (MIT-ish FFI) + thin hand-written safe wrapper, to avoid layering GPL from the higher-level `winfsp` crate (even under a commercial WinFsp license, pulling a GPL-3 Rust dep into our Cargo graph poisons the build). If `winfsp-sys` is insufficient, fallback is `winfsp_wrs` (MIT).
- Entry: `--winfsp-spike <mount_path> <nas_root>` CLI flag mirroring the existing `--projfs-spike` at `main.rs:560`.
- Callbacks: minimal viable set — `GetSecurityByName`, `GetVolumeInfo`, `Open`, `Close`, `Read`, `ReadDirectory`, `GetFileInfo`. Pass-through to SMB UNC, no cache in spike phase.
- Measurements: enum latency on 200-item dir, partial read at deep offset on 2.8 GB file, Notepad roundtrip for compat.

## Decision

**Adopt WinFsp (SnowflakePowered `winfsp` crate) as the Windows I/O backend. Delete the ProjFS path. Relicense UFB to GPL-3.0.**

### Why this is the right call

The load-bearing principle from the macOS cutover — "smart I/O interceptor, not a state-of-truth manager" — is satisfied by WinFsp and **fundamentally cannot be satisfied by ProjFS**. This was established with direct evidence:

- ProjFS, Step 1: capping `PrjWriteFileData` responses to 1 MiB causes silent data corruption. Unwritten ranges return zeros — no re-callback. ProjFS marks the full callback-range as hydrated regardless of what we actually wrote. The kernel extent-tracking is not controllable from user-mode the way we need.
- WinFsp, Step 2: a 4 KiB user read arrives as exactly `length=4096`. Each read is its own callback with real offset/length. Our code decides per-op whether to answer from a cache or from SMB. No placeholder state survives between operations. No reconciliation database. This is the macOS NFS loopback pattern, preserved.

### Licensing implication accepted

UFB relicenses from MIT to GPL-3.0 to satisfy WinFsp's FLOSS exception. Dependency audit already done — only `windows-projfs` (GPL-2.0-only) is incompatible, and it goes away when we drop the ProjFS backend. Everything else (tokio, rusqlite, Tauri, etc.) is MIT/Apache-2.0, all GPL-3 compatible. WebView2 is a Microsoft system component — covered by GPLv3 §1 system-library exception.

Requires a pass through the repo to update `Cargo.toml` `license` field, add `LICENSE` file, update README. Not a code change of any meaningful size.

### What the port looks like

Not in scope for this plan — separate work. But the shape is straightforward:

**Keep (reused as-is):**
- `cache_core` — metadata cache DDL, bit ops, LRU policy. Windows backend can decide what to cache independent of WinFsp.
- `write_through` coordinator + worker — platform-neutral.
- `conflict` — sidecar generation.
- `connectivity` — network health probes.
- `platform/windows/mountpoint.rs` — unused with WinFsp (WinFsp IS the mount mechanism), but no harm keeping for fallback.

**Delete:**
- `sync/projfs_server.rs` (replaced by `winfsp_server.rs`, which started life as the Step 2 spike).
- `sync/windows_cache.rs` block-level chunk_bitmap logic — simplifies to plain metadata + optional file-level cache since WinFsp doesn't force a hydration model.
- `config::vfs_dir()` — no separate virtualization root; WinFsp mounts the user-facing path directly.
- `windows-projfs` dep.
- ProjFS-related installer lines.

**Add:**
- `winfsp` crate dep, `winfsp_link_delayload()` in build.rs.
- Installer: bundle the WinFsp MSI + silent-install in the UFB setup.
- Installer build env: LLVM available for `winfsp-sys`'s bindgen. CI needs this too.
- `orchestrator::start_sync` reshaped around `FileSystemHost::new` + `mount(user_facing_path)`, `stop_sync` around `unmount() + stop()`.

**Rewrite:**
- `orchestrator::start_sync` — simpler than the ProjFS version. No junction toggling; WinFsp mount/unmount is the toggle.

### What's still an open question (for the port, not this decision)

1. **Enumeration latency gate** on WinFsp vs SMB vs NTFS. Research flagged large-directory listing as WinFsp's weak spot. Metadata cache closes this, but needs measurement.
2. **Compatibility pass:** Premiere / Resolve / AE / Bridge on WinFsp directory mounts. Oplocks + ADS are the risk areas.
3. **Recycle Bin / Indexer behavior** on WinFsp mounts — Windows Explorer creates `#recycle`, Indexer tries to index, both generate extra I/O we should measure and potentially suppress via volume-param flags.

None of these invalidate the architecture choice; all are implementation-phase tasks.

### Next step

Write a separate implementation plan for the port. This evaluation doc remains the decision record.

## References

- Session transcript: 2026-04-17
- Approved plan: `C:\Users\chris\.claude\plans\groovy-splashing-muffin.md`
- Original ProjFS plan (superseded): `docs/windows-projfs-plan.md`
- macOS NFS cutover (architectural reference): `docs/nfs-loopback-plan.md`
- ProjFS implementation on this branch: `mediamount-agent/src/sync/projfs_server.rs`
- Shared cache core (will reuse regardless of backend): `mediamount-agent/src/sync/cache_core.rs`
- macOS smart-I/O-interceptor reference: `mediamount-agent/src/sync/nfs_server.rs`
