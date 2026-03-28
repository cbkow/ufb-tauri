import { createSignal, onMount, onCleanup } from "solid-js";
import type { BrowserStore } from "../stores/fileStore";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { copyFiles, moveFiles, startNativeDrag } from "./tauri";
import { platformStore } from "../stores/platformStore";

interface BrowserDragDropConfig {
  /** Look up a BrowserStore by its DOM data-browser-id */
  getBrowserStore: (browserId: string) => BrowserStore | null;
  /** Look up the external-drop handler registered for a given browser */
  getExternalDropHandler: (browserId: string) => ((paths: string[]) => void) | undefined;
  /** Enable cross-browser drag (copy/move between two browsers). True for DualBrowserView. */
  enableCrossBrowserDrag?: boolean;
  /**
   * Handle drops on a panel identified by data-drop-path (e.g. ItemListPanel).
   * Called with (droppedPaths, destinationPath).
   */
  onDropToPath?: (paths: string[], destPath: string) => void;
}

interface InternalDrag {
  paths: string[];
  sourceBrowserId: string;
  startX: number;
  startY: number;
}

interface ActiveDrag {
  paths: string[];
  sourceBrowserId: string;
  x: number;
  y: number;
}

// ── Singleton global drop listener ──

const handlerRegistry = new Map<string, { config: BrowserDragDropConfig }>();
let globalListenerInit = false;

function initGlobalDropListener() {
  if (globalListenerInit) return;
  globalListenerInit = true;

  getCurrentWebview().onDragDropEvent((event) => {
    const payload = event.payload;
    const pos = "position" in payload ? payload.position : null;
    const scale = window.devicePixelRatio || 1;
    const rawX = pos ? pos.x : 0;
    const rawY = pos ? pos.y : 0;
    const isMac = navigator.platform.toLowerCase().includes("mac");
    // macOS: Tauri reports logical coordinates, no scaling needed
    // Windows: Tauri reports physical pixels, divide by scale
    const logicalX = isMac ? rawX : rawX / scale;
    const logicalY = isMac ? rawY : rawY / scale;

    if (payload.type === "over" || payload.type === "enter") {
      document.querySelectorAll(".file-browser-content.drop-zone-active, .item-list-scroll.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));

      // Use bounding rect hit-testing instead of elementFromPoint for hover highlights.
      // This is more reliable across platforms — elementFromPoint can miss due to
      // coordinate system differences between Tauri's reported position and the DOM.
      let highlighted = false;
      const browsers = document.querySelectorAll<HTMLElement>(".file-browser");
      for (const browser of browsers) {
        const rect = browser.getBoundingClientRect();
        if (logicalX >= rect.left && logicalX <= rect.right &&
            logicalY >= rect.top && logicalY <= rect.bottom) {
          const content = browser.querySelector(".file-browser-content");
          if (content) {
            content.classList.add("drop-zone-active");
            highlighted = true;
          }
          break;
        }
      }

      // If no browser matched, check item list panels
      if (!highlighted) {
        const panels = document.querySelectorAll<HTMLElement>(".item-list-panel[data-drop-path]");
        for (const panel of panels) {
          const rect = panel.getBoundingClientRect();
          if (logicalX >= rect.left && logicalX <= rect.right &&
              logicalY >= rect.top && logicalY <= rect.bottom) {
            panel.querySelector(".item-list-scroll")?.classList.add("drop-zone-active");
            break;
          }
        }
      }
    } else if (payload.type === "leave") {
      document.querySelectorAll(".file-browser-content.drop-zone-active, .item-list-scroll.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));
    } else if (payload.type === "drop" && payload.paths.length > 0) {
      document.querySelectorAll(".file-browser-content.drop-zone-active, .item-list-scroll.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));

      const el = document.elementFromPoint(logicalX, logicalY);
      const browserEl = el?.closest(".file-browser");
      const browserId = browserEl?.getAttribute("data-browser-id");

      if (browserId) {
        // Find the config that has this browser registered
        for (const entry of handlerRegistry.values()) {
          const handler = entry.config.getExternalDropHandler(browserId);
          if (handler) {
            handler(payload.paths);
            return;
          }
        }
      }
      // Check if dropped on an ItemListPanel with data-drop-path
      const panelEl = el?.closest(".item-list-panel[data-drop-path]");
      const dropPath = panelEl?.getAttribute("data-drop-path");
      if (dropPath) {
        for (const entry of handlerRegistry.values()) {
          if (entry.config.onDropToPath) {
            entry.config.onDropToPath(payload.paths, dropPath);
            return;
          }
        }
      }
      // Fallback: try first handler we can find
      for (const entry of handlerRegistry.values()) {
        const handler = entry.config.getExternalDropHandler("");
        if (handler) {
          handler(payload.paths);
          return;
        }
      }
    }
  });
  // No need to store unlisten — global listener lives for app lifetime
}

/**
 * Shared drag/drop hook used by DualBrowserView and FolderTabView.
 * Handles:
 *   A. Drop from Explorer (Tauri onDragDropEvent — single global listener)
 *   B. Drag to Explorer (mouse near window edge → startNativeDrag)
 *   C. Same-browser drag onto a subfolder (move)
 *   D. Cross-browser drag (copy/move, DualBrowserView only)
 */
export function useBrowserDragDrop(config: BrowserDragDropConfig) {
  let pendingDrag: InternalDrag | null = null;
  let nativeDragActive = false;

  const [activeDrag, setActiveDrag] = createSignal<ActiveDrag | null>(null);

  // Unique key for this hook instance
  const registryKey = Math.random().toString(36).slice(2);

  onMount(() => {
    // Register with global drop listener
    initGlobalDropListener();
    handlerRegistry.set(registryKey, { config });

    // ── B/C/D. Internal drag: mousedown → mousemove → mouseup ──

    function onMouseDown(e: MouseEvent) {
      if (e.button !== 0) return;
      if (e.ctrlKey || e.metaKey || e.shiftKey) return;

      const target = e.target as HTMLElement;

      // Don't initiate drag from item-list panels (metadata/tracker panels).
      // These panels have editable fields and drag interferes with interaction.
      if (target.closest(".item-list-panel")) return;

      const row = target.closest(".file-row, .grid-item");
      if (!row) return;

      // Only start drag from FileBrowser panels
      const browserEl = target.closest(".file-browser");
      let browserId = browserEl?.getAttribute("data-browser-id") ?? null;
      if (!browserId) return;

      const store = config.getBrowserStore(browserId);
      const entryPath = row.getAttribute("data-path");
      if (!entryPath) return;

      pendingDrag = {
        paths: store && store.selection.has(entryPath)
          ? [...store.selection]
          : [entryPath],
        sourceBrowserId: browserId,
        startX: e.clientX,
        startY: e.clientY,
      };
    }

    function updateDropTarget(x: number, y: number, sourceBrowserId: string) {
      // Clear old highlights
      document.querySelectorAll(".file-browser-content.drop-zone-active, .item-list-scroll.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));
      document.querySelectorAll(".file-row.drop-target, .grid-item.drop-target")
        .forEach((el) => el.classList.remove("drop-target"));

      const el = document.elementFromPoint(x, y);
      const browserEl = el?.closest(".file-browser");
      const targetBrowserId = browserEl?.getAttribute("data-browser-id");

      if (targetBrowserId && targetBrowserId === sourceBrowserId) {
        // Same browser — check if hovering over a directory row
        const row = el?.closest(".file-row, .grid-item");
        if (row && row.getAttribute("data-is-dir") === "true") {
          row.classList.add("drop-target");
        }
      } else if (targetBrowserId && targetBrowserId !== sourceBrowserId && config.enableCrossBrowserDrag) {
        // Different browser — highlight whole content area
        const content = browserEl?.querySelector(".file-browser-content");
        content?.classList.add("drop-zone-active");
      } else if (!targetBrowserId) {
        // Check if hovering over ItemListPanel
        const panelEl = el?.closest(".item-list-panel[data-drop-path]");
        if (panelEl) {
          const panelBrowserId = panelEl.getAttribute("data-browser-id");
          if (panelBrowserId !== sourceBrowserId) {
            panelEl.querySelector(".item-list-scroll")?.classList.add("drop-zone-active");
          }
        }
      }
    }

    function clearDropTarget() {
      document.querySelectorAll(".file-browser-content.drop-zone-active, .item-list-scroll.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));
      document.querySelectorAll(".file-row.drop-target, .grid-item.drop-target")
        .forEach((el) => el.classList.remove("drop-target"));
    }

    function onMouseMove(e: MouseEvent) {
      if (activeDrag()) {
        setActiveDrag((prev) => prev ? { ...prev, x: e.clientX, y: e.clientY } : null);
        updateDropTarget(e.clientX, e.clientY, activeDrag()!.sourceBrowserId);

        // B. If cursor reaches window edge, hand off to native OS drag-out (Windows only)
        const margin = 6;
        const nearEdge =
          e.clientX <= margin ||
          e.clientY <= margin ||
          e.clientX >= window.innerWidth - margin ||
          e.clientY >= window.innerHeight - margin;

        if (nearEdge && !nativeDragActive) {
          nativeDragActive = true;
          const drag = activeDrag()!;
          setActiveDrag(null);
          clearDropTarget();

          startNativeDrag(drag.paths)
            .then((result) => {
              if (result === "moved") {
                const srcStore = config.getBrowserStore(drag.sourceBrowserId);
                srcStore?.refresh();
              }
            })
            .catch((err) => console.error("Native drag-out failed:", err))
            .finally(() => { nativeDragActive = false; });
        }
        return;
      }

      if (!pendingDrag) return;

      const dx = e.clientX - pendingDrag.startX;
      const dy = e.clientY - pendingDrag.startY;
      if (Math.abs(dx) + Math.abs(dy) < 8) return;

      const drag = pendingDrag;
      pendingDrag = null;

      setActiveDrag({
        paths: drag.paths,
        sourceBrowserId: drag.sourceBrowserId,
        x: e.clientX,
        y: e.clientY,
      });
    }

    function onMouseUp(e: MouseEvent) {
      const drag = activeDrag();
      if (drag) {
        const el = document.elementFromPoint(e.clientX, e.clientY);
        const browserEl = el?.closest(".file-browser");
        const targetBrowserId = browserEl?.getAttribute("data-browser-id");

        if (targetBrowserId && targetBrowserId === drag.sourceBrowserId) {
          // C. Same-browser drag — check if dropped on a directory
          const row = el?.closest(".file-row, .grid-item");
          if (row && row.getAttribute("data-is-dir") === "true") {
            const targetPath = row.getAttribute("data-path");
            if (targetPath && !drag.paths.includes(targetPath)) {
              const srcStore = config.getBrowserStore(drag.sourceBrowserId);
              moveFiles(drag.paths, targetPath)
                .then(() => srcStore?.refresh())
                .catch((err) => console.error("Same-browser drag failed:", err));
            }
          }
        } else if (targetBrowserId && targetBrowserId !== drag.sourceBrowserId && config.enableCrossBrowserDrag) {
          // D. Cross-browser drag
          const targetStore = config.getBrowserStore(targetBrowserId);
          if (targetStore) {
            const dest = targetStore.currentPath();
            const isMove = e.shiftKey;
            const op = isMove ? moveFiles : copyFiles;
            op(drag.paths, dest)
              .then(() => {
                targetStore.refresh();
                if (isMove) {
                  const srcStore = config.getBrowserStore(drag.sourceBrowserId);
                  srcStore?.refresh();
                }
              })
              .catch((err) => console.error("Cross-browser drag failed:", err));
          }
        } else if (!targetBrowserId && config.onDropToPath) {
          // E. Drop onto ItemListPanel
          const panelEl = el?.closest(".item-list-panel[data-drop-path]");
          const dropPath = panelEl?.getAttribute("data-drop-path");
          if (dropPath && !drag.paths.includes(dropPath)) {
            config.onDropToPath(drag.paths, dropPath);
            const srcStore = config.getBrowserStore(drag.sourceBrowserId);
            srcStore?.refresh();
          }
        }

        setActiveDrag(null);
        clearDropTarget();
      }
      pendingDrag = null;
    }

    document.addEventListener("mousedown", onMouseDown, true);
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);

    onCleanup(() => {
      handlerRegistry.delete(registryKey);
      document.removeEventListener("mousedown", onMouseDown, true);
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    });
  });

  return activeDrag;
}
