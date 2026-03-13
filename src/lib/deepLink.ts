import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { resolveUfbUri } from "./tauri";

export type NavigateFn = (path: string) => void;
let navigateCallback: NavigateFn | undefined;

export function setDeepLinkNavigate(fn: NavigateFn) {
  navigateCallback = fn;
}

export async function initDeepLinkListener() {
  // Listen for deep-link events emitted from Rust
  await listen<string>("deep-link-uri", async (event) => {
    try {
      const localPath = await resolveUfbUri(event.payload);
      navigateCallback?.(localPath);
    } catch (e) {
      console.error("Failed to resolve deep link:", e);
    }
  });

  // Check for a cold-start pending deep link stored in AppState
  try {
    const pending = await invoke<string | null>("get_pending_deep_link");
    if (pending) {
      const localPath = await resolveUfbUri(pending);
      navigateCallback?.(localPath);
    }
  } catch (e) {
    console.error("Failed to check pending deep link:", e);
  }
}
