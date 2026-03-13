/**
 * Pointer-capture based column resize utility.
 * Same pattern as Splitter.tsx: pointerdown → capture → pointermove → pointerup.
 */
export function makeColumnResizer(opts: {
  getWidth: () => number;
  setWidth: (w: number) => void;
  onDone: (w: number) => void;
  minWidth?: number;
}): { onPointerDown: (e: PointerEvent) => void } {
  const min = opts.minWidth ?? 40;

  return {
    onPointerDown(e: PointerEvent) {
      e.preventDefault();
      e.stopPropagation();
      const startX = e.clientX;
      const startW = opts.getWidth();
      const el = e.target as HTMLElement;
      el.setPointerCapture(e.pointerId);

      function onMove(ev: PointerEvent) {
        const delta = ev.clientX - startX;
        const w = Math.max(min, startW + delta);
        opts.setWidth(w);
      }

      function onUp(ev: PointerEvent) {
        el.releasePointerCapture(ev.pointerId);
        el.removeEventListener("pointermove", onMove);
        el.removeEventListener("pointerup", onUp);
        const delta = ev.clientX - startX;
        const w = Math.max(min, startW + delta);
        opts.onDone(w);
      }

      el.addEventListener("pointermove", onMove);
      el.addEventListener("pointerup", onUp);
    },
  };
}
