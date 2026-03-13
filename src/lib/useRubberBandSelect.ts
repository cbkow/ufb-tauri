import { createSignal, onCleanup } from "solid-js";
import type { BrowserStore } from "../stores/fileStore";

export interface RubberBandRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Rubber-band (lasso/marquee) selection for a scrollable file list/grid container.
 *
 * Attach to the `.file-browser-content` element. Draws a selection rectangle
 * on mousedown+drag over the background (not on file items), and selects all
 * items whose bounding boxes intersect the rectangle.
 */
export function useRubberBandSelect(
  getContainer: () => HTMLElement | undefined,
  getStore: () => BrowserStore,
) {
  const [rect, setRect] = createSignal<RubberBandRect | null>(null);

  let active = false;
  // Anchor point in viewport coordinates
  let anchorX = 0;
  let anchorY = 0;
  // Scroll offset at anchor time
  let anchorScrollTop = 0;
  let anchorScrollLeft = 0;
  // Baseline selection (paths already selected before rubber-band started, for Ctrl+drag)
  let baselineSelection = new Set<string>();
  // Auto-scroll interval
  let scrollInterval: ReturnType<typeof setInterval> | null = null;
  // Last known mouse position (for auto-scroll updates)
  let lastMouseX = 0;
  let lastMouseY = 0;

  function onMouseDown(e: MouseEvent) {
    if (e.button !== 0) return;
    const container = getContainer();
    if (!container) return;

    // Only start rubber-band on background — not on file items
    const target = e.target as HTMLElement;
    if (target.closest(".file-row, .grid-item, .file-list-header, .nav-bar-wrapper")) return;

    // Must be within the content area
    if (!container.contains(target)) return;

    e.preventDefault();

    anchorX = e.clientX;
    anchorY = e.clientY;
    anchorScrollTop = container.scrollTop;
    anchorScrollLeft = container.scrollLeft;
    lastMouseX = e.clientX;
    lastMouseY = e.clientY;

    // Ctrl/Meta: additive selection; otherwise start fresh
    if (e.ctrlKey || e.metaKey) {
      baselineSelection = new Set(getStore().selection);
    } else {
      baselineSelection = new Set();
      getStore().clearSelection();
    }

    active = true;

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

  function getSelectionRect(container: HTMLElement, mx: number, my: number): RubberBandRect {
    const cr = container.getBoundingClientRect();
    // Convert anchor to current content-relative position
    const ax = anchorX - cr.left + anchorScrollLeft;
    const ay = anchorY - cr.top + anchorScrollTop;

    // Current mouse in content-relative position
    const cx = mx - cr.left + container.scrollLeft;
    const cy = my - cr.top + container.scrollTop;

    const left = Math.min(ax, cx);
    const top = Math.min(ay, cy);
    const width = Math.abs(cx - ax);
    const height = Math.abs(cy - ay);

    return { left, top, width, height };
  }

  function updateSelection(container: HTMLElement, selRect: RubberBandRect) {
    const store = getStore();
    const items = container.querySelectorAll<HTMLElement>(".file-row, .grid-item");
    const newSelection = new Set(baselineSelection);

    for (const item of items) {
      const path = item.getAttribute("data-path");
      if (!path) continue;

      const itemRect = item.getBoundingClientRect();
      const cr = container.getBoundingClientRect();

      // Convert item rect to content-relative coordinates
      const itemLeft = itemRect.left - cr.left + container.scrollLeft;
      const itemTop = itemRect.top - cr.top + container.scrollTop;
      const itemRight = itemLeft + itemRect.width;
      const itemBottom = itemTop + itemRect.height;

      // Check intersection
      const intersects =
        selRect.left < itemRight &&
        selRect.left + selRect.width > itemLeft &&
        selRect.top < itemBottom &&
        selRect.top + selRect.height > itemTop;

      if (intersects) {
        newSelection.add(path);
      }
    }

    store.setSelection(newSelection);
  }

  function onMouseMove(e: MouseEvent) {
    if (!active) return;
    const container = getContainer();
    if (!container) return;

    lastMouseX = e.clientX;
    lastMouseY = e.clientY;

    const selRect = getSelectionRect(container, e.clientX, e.clientY);

    // Only show rect if we've moved a meaningful distance (avoid flicker on clicks)
    if (selRect.width > 3 || selRect.height > 3) {
      setRect(selRect);
      updateSelection(container, selRect);
    }

    // Auto-scroll when near edges
    const cr = container.getBoundingClientRect();
    const edgeMargin = 30;
    const maxSpeed = 12;

    let scrollY = 0;
    let scrollX = 0;
    if (e.clientY < cr.top + edgeMargin && e.clientY >= cr.top) {
      scrollY = -maxSpeed * (1 - (e.clientY - cr.top) / edgeMargin);
    } else if (e.clientY > cr.bottom - edgeMargin && e.clientY <= cr.bottom) {
      scrollY = maxSpeed * (1 - (cr.bottom - e.clientY) / edgeMargin);
    }
    if (e.clientX < cr.left + edgeMargin && e.clientX >= cr.left) {
      scrollX = -maxSpeed * (1 - (e.clientX - cr.left) / edgeMargin);
    } else if (e.clientX > cr.right - edgeMargin && e.clientX <= cr.right) {
      scrollX = maxSpeed * (1 - (cr.right - e.clientX) / edgeMargin);
    }

    if ((scrollX !== 0 || scrollY !== 0) && !scrollInterval) {
      scrollInterval = setInterval(() => {
        if (!active) { stopAutoScroll(); return; }
        const c = getContainer();
        if (!c) return;
        c.scrollTop += scrollY;
        c.scrollLeft += scrollX;
        // Re-compute selection after scroll
        const sr = getSelectionRect(c, lastMouseX, lastMouseY);
        setRect(sr);
        updateSelection(c, sr);
      }, 16);
    } else if (scrollX === 0 && scrollY === 0) {
      stopAutoScroll();
    }
  }

  function stopAutoScroll() {
    if (scrollInterval) {
      clearInterval(scrollInterval);
      scrollInterval = null;
    }
  }

  function onMouseUp() {
    active = false;
    setRect(null);
    stopAutoScroll();
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
  }

  onCleanup(() => {
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
    stopAutoScroll();
  });

  return { rect, onMouseDown };
}
