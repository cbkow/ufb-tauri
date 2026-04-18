# Windows ERROR_UNTRUSTED_MOUNT_POINT (448) on first launch

**Status**: partially mitigated, not fully resolved. **Real fix requires code-signing.**

## Symptom

First launch of the installed app after running `ufb-tauri-setup-*.exe`:

- Bookmarks panel populates correctly with mount configs + states
- IPC works, `mount:connection` fires, state updates flow
- Clicks on any bookmark fail with:
  ```
  Failed to list directory: Not a directory or cannot read:
  The path cannot be traversed because it contains an untrusted mount
  point. (os error 448)
  ```
- Affects every mount path (sync AND non-sync, WinFsp AND plain symlinks)
- Closing and relaunching the app fixes it; second launch onward is fine

## Root cause (confirmed for symptom, cause narrowed but not pinpointed)

Windows 10/11 `ERROR_UNTRUSTED_MOUNT_POINT (0x1C0 = 448)` is raised by the NT FS filter when a process traverses a reparse point (symlink, junction, WinFsp mount) and the kernel considers the process not-yet-trusted for that mount.

The trigger on first launch is most likely **SmartScreen / Application Reputation** — it runs unsigned EXEs with hashes it hasn't seen before in a restricted integrity context while it scans. Reparse-point traversal is blocked for the scan's duration. Once SmartScreen marks the hash OK (per-machine, per-hash), subsequent launches skip the restricted phase and everything works.

Observed behavior supporting SmartScreen as the cause:
- Occurs on **every** machine tested, always on the first launch after install.
- Goes away on the second launch (same install, same hash).
- Unchanged by Developer Mode being enabled (Dev Mode affects `CreateSymbolicLinkW` privileges, not SmartScreen).
- Independent of whether mounts are sync (WinFsp reparse point) or non-sync (UNC symlink created by the agent in user context — no admin/user ownership mismatch).

Things that would rule out SmartScreen but haven't been observed: 448 persisting across multiple launches, or failing in a freshly-started agent process (the agent has no issue traversing the mounts — only UFB's Tauri process does on first run).

## Mitigations tried

### Retry on 448 (shipped, `file_ops.rs:read_dir_with_448_retry`)
Transparent retry on exactly errno 448: 4 attempts, backoff 200/400/800ms (max ~1.4s). Leaves other errors (not-found, permission-denied) untouched.

**Result**: does not fully resolve the issue. The SmartScreen scan takes longer than our retry window in practice. The helper stays in place — it's harmless and covers the minority of cases where the scan finishes fast.

### Things we evaluated and rejected
- **Frontend reachability probe** — removed. `fs::metadata` on reparse points is also affected by 448 during the scan window, so the probe itself was reporting false "Unavailable" state. Feature disabled; plumbing kept in `mountStore.ts` for possible future agent-side reimplementation.
- **Config retry loop** — added then reverted. Turned out configs load fine; the actual failure was listDirectory, not config loading.
- **Changing SDDL owner on WinFsp mount** — not tried. We already use the canonical WinFsp permissive SDDL; changing owner from BA to BU wouldn't affect SmartScreen scanning of the UFB process itself.

## The real fix (not done)

**Code-sign the binaries.** A signed UFB.exe + mediamount-agent.exe bypasses the SmartScreen restricted-first-launch phase entirely. Reparse points work on first click.

Not pursued in v0.5.1 because of certificate cost. Microsoft EV code-signing certificates run ~$300-600/year from commercial CAs (no Apple-equivalent $99 option on Windows). This is an internal app and the cost was out of scope.

If we ever have a cert:
- Sign `ufb-tauri.exe` and `mediamount-agent.exe` in the release build step
- Update `build.rs` / CI to inject signing
- Potentially also sign the Inno Setup `.exe` output

## User-facing workaround

Tell users: **if bookmarks don't open on the very first launch after installing, quit UFB and relaunch it.** Second launch works reliably. Once the SmartScreen reputation has been established for the installed hash, this doesn't recur until the next install/upgrade.

Consider adding this to the installer's "finished" page and/or the README.

## Related: CSP warnings in release

The release console also shows:
```
Fetch API cannot load http://ipc.localhost/plugin%3Aevent%7Clisten.
Refused to connect because it violates the document's Content Security Policy.
IPC custom protocol failed, Tauri will now use the postMessage interface instead
```

Tauri's preferred IPC is via `http://ipc.localhost/` — blocked by our CSP. Tauri falls back to `postMessage` automatically. Functional but slower on first IPC calls. Not related to 448 but visible in the same console.

To silence: add `http://ipc.localhost` to `tauri.conf.json` CSP `connect-src` allowlist. Low priority; not blocking anything today.
