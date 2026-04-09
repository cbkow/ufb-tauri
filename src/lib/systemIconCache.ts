/// Cached system file type icon loader.
/// Deduplicates in-flight requests so 50 .mp4 files only trigger one IPC call.

import { getSystemIcon } from "./tauri";

const cache = new Map<string, string | null>();
const pending = new Map<string, Promise<string | null>>();

/**
 * Get the OS-native icon for a file extension as a data URL.
 * Returns null if the OS has no specific icon (use Material Symbol fallback).
 * Results are cached — subsequent calls for the same extension are instant.
 */
export function getSystemIconCached(
  extension: string,
  size: number = 32
): Promise<string | null> {
  const key = extension.toLowerCase();
  if (cache.has(key)) return Promise.resolve(cache.get(key)!);
  if (pending.has(key)) return pending.get(key)!;

  const promise = getSystemIcon(key, size)
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
