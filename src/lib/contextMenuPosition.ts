/**
 * Adjust a context menu element so it doesn't overflow the viewport.
 * Call this as a ref callback on the menu div.
 */
export function adjustMenuPosition(el: HTMLDivElement) {
  requestAnimationFrame(() => {
    const rect = el.getBoundingClientRect();
    const viewportHeight = window.innerHeight;
    const viewportWidth = window.innerWidth;

    if (rect.bottom > viewportHeight) {
      el.style.top = `${Math.max(0, viewportHeight - rect.height - 4)}px`;
    }
    if (rect.right > viewportWidth) {
      el.style.left = `${Math.max(0, viewportWidth - rect.width - 4)}px`;
    }
  });
}
