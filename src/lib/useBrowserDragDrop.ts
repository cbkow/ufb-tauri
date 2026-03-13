import { createSignal, onMount, onCleanup } from "solid-js";
import type { BrowserStore } from "../stores/fileStore";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { copyFiles, moveFiles, startNativeDrag } from "./tauri";

interface BrowserDragDropConfig {
  /** Look up a BrowserStore by its DOM data-browser-id */
  getBrowserStore: (browserId: string) => BrowserStore | null;
  /** Look up the external-drop handler registered for a given browser */
  getExternalDropHandler: (browserId: string) => ((paths: string[]) => void) | undefined;
  /** Enable cross-browser drag (copy/move between two browsers). True for DualBrowserView. */
  enableCrossBrowserDrag?: boolean;
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

/**
 * Shared drag/drop hook used by DualBrowserView and FolderTabView.
 * Handles:
 *   A. Drop from Explorer (Tauri onDragDropEvent)
 *   B. Drag to Explorer (mouse near window edge → startNativeDrag)
 *   C. Same-browser drag onto a subfolder (move)
 *   D. Cross-browser drag (copy/move, DualBrowserView only)
 */
export function useBrowserDragDrop(config: BrowserDragDropConfig) {
  let pendingDrag: InternalDrag | null = null;
  let nativeDragActive = false;

  const [activeDrag, setActiveDrag] = createSignal<ActiveDrag | null>(null);

  onMount(() => {
    let unlisten: (() => void) | undefined;

    // ── A. External drop from Explorer ──
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        const scale = window.devicePixelRatio || 1;
        const pos = "position" in payload ? payload.position : null;
        const logicalX = pos ? pos.x / scale : 0;
        const logicalY = pos ? pos.y / scale : 0;

        if (payload.type === "over" || payload.type === "enter") {
          document.querySelectorAll(".file-browser-content.drop-zone-active")
            .forEach((el) => el.classList.remove("drop-zone-active"));
          const el = document.elementFromPoint(logicalX, logicalY);
          const content = el?.closest(".file-browser")?.querySelector(".file-browser-content");
          content?.classList.add("drop-zone-active");
        } else if (payload.type === "leave") {
          document.querySelectorAll(".file-browser-content.drop-zone-active")
            .forEach((el) => el.classList.remove("drop-zone-active"));
        } else if (payload.type === "drop" && payload.paths.length > 0) {
          document.querySelectorAll(".file-browser-content.drop-zone-active")
            .forEach((el) => el.classList.remove("drop-zone-active"));

          const el = document.elementFromPoint(logicalX, logicalY);
          const browserEl = el?.closest(".file-browser");
          const browserId = browserEl?.getAttribute("data-browser-id");

          if (browserId) {
            const handler = config.getExternalDropHandler(browserId);
            if (handler) {
              handler(payload.paths);
              return;
            }
          }
          // Fallback: try first browser we can find a handler for
          const firstHandler = config.getExternalDropHandler("");
          firstHandler?.(payload.paths);
        }
      })
      .then((fn) => { unlisten = fn; });

    // ── B/C/D. Internal drag: mousedown → mousemove → mouseup ──

    function onMouseDown(e: MouseEvent) {
      if (e.button !== 0) return;
      if (e.ctrlKey || e.metaKey || e.shiftKey) return;

      const target = e.target as HTMLElement;
      const row = target.closest(".file-row, .grid-item");
      if (!row) return;
      const browserEl = target.closest(".file-browser");
      const browserId = browserEl?.getAttribute("data-browser-id");
      if (!browserId) return;

      const store = config.getBrowserStore(browserId);
      if (!store) return;

      const entryPath = row.getAttribute("data-path");
      if (!entryPath) return;

      pendingDrag = {
        paths: store.selection.has(entryPath)
          ? [...store.selection]
          : [entryPath],
        sourceBrowserId: browserId,
        startX: e.clientX,
        startY: e.clientY,
      };
    }

    function updateDropTarget(x: number, y: number, sourceBrowserId: string) {
      // Clear old highlights
      document.querySelectorAll(".file-browser-content.drop-zone-active")
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
      }
    }

    function clearDropTarget() {
      document.querySelectorAll(".file-browser-content.drop-zone-active")
        .forEach((el) => el.classList.remove("drop-zone-active"));
      document.querySelectorAll(".file-row.drop-target, .grid-item.drop-target")
        .forEach((el) => el.classList.remove("drop-target"));
    }

    function onMouseMove(e: MouseEvent) {
      if (activeDrag()) {
        setActiveDrag((prev) => prev ? { ...prev, x: e.clientX, y: e.clientY } : null);
        updateDropTarget(e.clientX, e.clientY, activeDrag()!.sourceBrowserId);

        // B. If cursor reaches window edge, hand off to native OS drag-out
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
      unlisten?.();
      document.removeEventListener("mousedown", onMouseDown, true);
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    });
  });

  return activeDrag;
}
