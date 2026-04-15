# mediamount-nfs-spike

Phase 0 de-risk spike for the NFS loopback migration (see
`docs/nfs-loopback-plan.md`). Throwaway. Delete this crate after the
measurement decision lands.

## What it does

Exposes any local directory — including an SMB mount point — as a localhost
NFS3 share. No caching, no SQLite, no UFB integration. Pure passthrough.
The only question we're answering: is the kernel NFS client + a user-space
Rust server **dramatically faster than FileProvider** for directory navigation
under a real SMB-backed workload?

## Build

```bash
cd mediamount-nfs-spike
cargo build --release
```

## Run

Pick an existing SMB mount as the root. For your current setup that would be
one of:

- `/Volumes/Jobs_Live-1`
- `/Users/chris/.local/share/ufb/smb-mounts/GFX_Dropbox`
- `/Volumes/FLAME_JOBS`

Then, in one terminal:

```bash
./target/release/mediamount-nfs-spike \
    --root /Volumes/Jobs_Live-1 \
    --bind 127.0.0.1:12345
```

In another terminal, mount it:

```bash
mkdir -p ~/ufb/vfs-spike
mount -t nfs \
    -o "port=12345,mountport=12345,nolocks,vers=3,tcp,nobrowse,actimeo=1,rsize=1048576,wsize=1048576" \
    localhost:/spike ~/ufb/vfs-spike
```

Verify:

```bash
ls ~/ufb/vfs-spike          # should show the same entries as the SMB share
```

To unmount:

```bash
umount ~/ufb/vfs-spike
```

### About `sudo`

macOS's `mount_nfs` typically does **not** require sudo when mounting with
`nobrowse` + user-owned target. If it does on your setup, that's a real
finding for the Phase 0 report — note it and we'll solve at Phase 1.

## The measurement

Three-way `time ls -R` on the same subtree, cold caches each time
(restart the spike between the second and third runs so attribute caches
don't pollute).

Pick a subtree you know well. Something with ~100 folders and ~1000 files
total. For example:

```bash
TARGET_SMB='/Volumes/Jobs_Live-1/<some_project>'
TARGET_FP='/Users/chris/Library/CloudStorage/com.unionfiles.mediamount-tray.FileProvider-Jobs_Live/<some_project>'
TARGET_NFS="$HOME/ufb/vfs-spike/<some_project>"
```

Run:

```bash
# 1. Plain SMB baseline
echo "=== plain SMB ===" && time ls -R "$TARGET_SMB" > /dev/null

# 2. Current FileProvider
echo "=== FileProvider ===" && time ls -R "$TARGET_FP" > /dev/null

# 3. NFS loopback passthrough
echo "=== NFS loopback ===" && time ls -R "$TARGET_NFS" > /dev/null
```

Record the real times.

### GO/NO-GO

Justify moving to Phase 1 only if:

- NFS loopback is **≤ 2× plain SMB**, AND
- FileProvider is significantly slower (**≥ 5× plain SMB**).

If NFS loopback is itself slow (e.g. within 20% of FileProvider), the
bottleneck is somewhere we haven't identified — abandon the NFS migration
plan and look elsewhere (thumbnailer? NAS itself?).

If FileProvider is already close to plain SMB (e.g. within 2×), we don't
need to migrate — the Waves 1–4 work closed the gap, and the architectural
rewrite isn't worth its cost.

## Also measure / sanity-check

- `mount_nfs` worked without sudo? (yes/no)
- Does Finder navigate `~/ufb/vfs-spike` normally?
- Does QuickLook (spacebar preview) work on a movie file there?
- Large file (> 4 GB) readable via `cat file.mov > /dev/null`?
- `umount` returns immediately?

## Known limitations of the spike

- **Read-only.** Writes return `EROFS`. Don't try to drag files in.
- **No cache, no freshness.** Kernel's `actimeo=1` attribute cache is all
  we have. Every `ls` hits the SMB mount.
- **Hot id-map.** The `by_id` / `by_path` hash map grows without bound for
  the duration of the process. Fine for a spike. Not fine for Phase 1.
- **No persistent file handles.** Restart the spike and the kernel's cached
  NFS handles become stale. That's fine for the measurement; Phase 1 will
  fix this by keying handles from SQLite rowids.
