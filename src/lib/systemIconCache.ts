/// Cached system file type icon loader.
/// Deduplicates in-flight requests so 50 .mp4 files only trigger one IPC call.

import { getSystemIcon } from "./tauri";

const cache = new Map<string, string | null>();
const pending = new Map<string, Promise<string | null>>();

/// Bucket a requested pixel size into the Windows SHIL tier the agent
/// actually returns. Must match `size_bucket` in `src-tauri/src/system_icons.rs`
/// so the cache keys line up with what the backend caches.
function bucketForSize(size: number): number {
  if (size <= 16) return 16;
  if (size <= 32) return 32;
  if (size <= 48) return 48;
  return 256;
}

/**
 * Get the OS-native icon for a file extension as a data URL.
 * Returns null if the OS has no specific icon (use Material Symbol fallback).
 * Results are cached by (extension, size-bucket) so a small-icon request
 * from the list view doesn't starve the grid view of its 256px version.
 */
export function getSystemIconCached(
  extension: string,
  size: number = 32
): Promise<string | null> {
  const bucket = bucketForSize(size);
  const key = `${extension.toLowerCase()}:${bucket}`;
  if (cache.has(key)) return Promise.resolve(cache.get(key)!);
  if (pending.has(key)) return pending.get(key)!;

  const promise = getSystemIcon(extension.toLowerCase(), bucket)
    .then((result) => {
      cache.set(key, result);
      pending.delete(key);
      return result;
    })
    .catch(() => {
      cache.set(key, null);
      pending.delete(key);
      return null;
    });

  pending.set(key, promise);
  return promise;
}
