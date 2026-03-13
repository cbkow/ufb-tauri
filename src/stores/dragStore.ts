import { createSignal } from "solid-js";

export interface DragData {
  /** File paths being dragged */
  paths: string[];
  /** Source browser store ID (so we know where it came from) */
  sourceBrowserId: string;
  /** Whether this is a move (cut) or copy */
  isMove: boolean;
}

const [dragData, setDragData] = createSignal<DragData | null>(null);
const [dropTargetPath, setDropTargetPath] = createSignal<string | null>(null);

export const dragStore = {
  /** Current internal drag data (null when no drag active) */
  get data() {
    return dragData();
  },

  /** Path currently being hovered as a drop target */
  get dropTarget() {
    return dropTargetPath();
  },

  startDrag(paths: string[], sourceBrowserId: string, isMove: boolean) {
    setDragData({ paths, sourceBrowserId, isMove });
  },

  setDropTarget(path: string | null) {
    setDropTargetPath(path);
  },

  endDrag() {
    setDragData(null);
    setDropTargetPath(null);
  },
};
