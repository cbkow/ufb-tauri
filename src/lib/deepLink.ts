import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { resolveUfbUri, revealInFileManager } from "./tauri";

export type NavigateFn = (path: string) => void;
let navigateCallback: NavigateFn | undefined;

export function setDeepLinkNavigate(fn: NavigateFn) {
  navigateCallback = fn;
}

async function handleDeepLinkUri(uri: string) {
  if (uri.startsWith("union://")) {
    // union:// links open the file manager at the resolved path
    const localPath = await resolveUfbUri(uri);
    await revealInFileManager(localPath);
  } else if (uri.startsWith("ufb://")) {
    // ufb:// links navigate within the app
    const localPath = await resolveUfbUri(uri);
    navigateCallback?.(localPath);
  }
}

export async function initDeepLinkListener() {
  // Listen for deep-link events emitted from Rust
  await listen<string>("deep-link-uri", async (event) => {
    try {
      await handleDeepLinkUri(event.payload);
    } catch (e) {
      console.error("Failed to resolve deep link:", e);
    }
  });

  // Check for a cold-start pending deep link stored in AppState
  try {
    const pending = await invoke<string | null>("get_pending_deep_link");
    if (pending) {
      await handleDeepLinkUri(pending);
    }
  } catch (e) {
    console.error("Failed to check pending deep link:", e);
  }
}
