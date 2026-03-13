import { createSignal, JSX, onCleanup, onMount } from "solid-js";
import "./Splitter.css";

interface SplitterProps {
  /** "horizontal" splits left|right, "vertical" splits top|bottom */
  direction?: "horizontal" | "vertical";
  /** Initial size of the first panel in pixels */
  initialSize?: number;
  /** Initial size as fraction of container (0-1). Overrides initialSize. */
  initialRatio?: number;
  /** Minimum size of the first panel */
  minSize?: number;
  /** Minimum size of the second panel */
  minSecondSize?: number;
  /** First panel content */
  first: JSX.Element;
  /** Second panel content */
  second: JSX.Element;
  /** CSS class for the container */
  class?: string;
  /** Called when size changes */
  onResize?: (size: number) => void;
}

export function Splitter(props: SplitterProps) {
  const direction = () => props.direction ?? "horizontal";
  const minSize = () => props.minSize ?? 100;
  const minSecondSize = () => props.minSecondSize ?? 100;
  const [size, setSize] = createSignal(props.initialSize ?? 240);
  const [dragging, setDragging] = createSignal(false);
  let ratioApplied = props.initialRatio == null;

  let containerRef: HTMLDivElement | undefined;

  onMount(() => {
    if (ratioApplied || !containerRef) return;
    // Try immediately
    const rect = containerRef.getBoundingClientRect();
    const total = direction() === "horizontal" ? rect.width : rect.height;
    if (total > 0) {
      setSize(Math.round(props.initialRatio! * total));
      ratioApplied = true;
      return;
    }
    // Container not visible yet (hidden tab) — use ResizeObserver to catch it
    const ro = new ResizeObserver((entries) => {
      if (ratioApplied) { ro.disconnect(); return; }
      for (const entry of entries) {
        const s = direction() === "horizontal" ? entry.contentRect.width : entry.contentRect.height;
        if (s > 0) {
          setSize(Math.round(props.initialRatio! * s));
          ratioApplied = true;
          ro.disconnect();
          break;
        }
      }
    });
    ro.observe(containerRef);
    onCleanup(() => ro.disconnect());
  });

  function onPointerDown(e: PointerEvent) {
    e.preventDefault();
    setDragging(true);
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging() || !containerRef) return;
    const rect = containerRef.getBoundingClientRect();
    let newSize: number;
    if (direction() === "horizontal") {
      newSize = e.clientX - rect.left;
    } else {
      newSize = e.clientY - rect.top;
    }
    const totalSize = direction() === "horizontal" ? rect.width : rect.height;
    const maxSize = totalSize - minSecondSize() - 5;
    newSize = Math.max(minSize(), Math.min(maxSize, newSize));
    setSize(newSize);
    props.onResize?.(newSize);
  }

  function onPointerUp() {
    setDragging(false);
  }

  onCleanup(() => setDragging(false));

  const isH = () => direction() === "horizontal";

  return (
    <div
      ref={containerRef}
      class={`splitter-container ${isH() ? "splitter-h" : "splitter-v"} ${props.class ?? ""}`}
    >
      <div
        class="splitter-pane splitter-first"
        style={{
          [isH() ? "width" : "height"]: `${size()}px`,
          [isH() ? "min-width" : "min-height"]: `${minSize()}px`,
        }}
      >
        {props.first}
      </div>
      <div
        class={`splitter-handle ${isH() ? "splitter-handle-h" : "splitter-handle-v"} ${dragging() ? "splitter-handle-active" : ""}`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
      >
        <div class="splitter-handle-line" />
      </div>
      <div
        class="splitter-pane splitter-second"
        style={{
          [isH() ? "min-width" : "min-height"]: `${minSecondSize()}px`,
        }}
      >
        {props.second}
      </div>
    </div>
  );
}
