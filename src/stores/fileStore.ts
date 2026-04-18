import { createSignal, onCleanup } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { FileEntry } from "../lib/types";
import { listDirectory } from "../lib/tauri";
import { listen } from "@tauri-apps/api/event";

export type SortField = "name" | "size" | "modified" | "extension";
export type SortDirection = "asc" | "desc";
export type ViewMode = "list" | "grid" | "tree";

/// Row in the tree-mode flat list. Produced by `BrowserStore.treeList()` —
/// one entry per visible row across all expanded levels. `depth` drives the
/// leading indent; `isLoading` shows a spinner while children fetch.
export interface TreeRow {
  entry: FileEntry;
  depth: number;
  isExpanded: boolean;
  isLoading: boolean;
}

let browserIdCounter = 0;

/// Shallow content comparison of two entry arrays. Returns true iff the
/// lists are the same length and each corresponding entry matches on the
/// fields that actually matter for rendering (path, size, mtime, isDir).
/// Used to short-circuit refresh-loops when the folder hasn't changed.
function entriesEqual(a: readonly FileEntry[], b: readonly FileEntry[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (
      x.path !== y.path ||
      x.size !== y.size ||
      x.modified !== y.modified ||
      x.isDir !== y.isDir
    ) {
      return false;
    }
  }
  return true;
}

export interface BrowserStore {
  /** Unique identifier for this browser instance */
  id: string;
  // Reactive state (call as functions)
  currentPath: () => string;
  viewMode: () => ViewMode;
  sortField: () => SortField;
  sortDirection: () => SortDirection;
  searchQuery: () => string;
  isLoading: () => boolean;
  canGoBack: () => boolean;
  canGoForward: () => boolean;
  gridSize: () => number;

  // Direct access (reactive via store)
  entries: FileEntry[];
  selection: Set<string>;
  sortedEntries: () => FileEntry[];

  // Tree-view state (viewMode === "tree")
  treeList: () => TreeRow[];
  isPathExpanded: (path: string) => boolean;
  toggleTreeExpand: (path: string) => void;

  // Actions
  navigateTo: (path: string, addToHistory?: boolean) => Promise<void>;
  goBack: () => void;
  goForward: () => void;
  goUp: () => void;
  selectItem: (path: string, multi?: boolean, range?: boolean) => void;
  selectAll: () => void;
  clearSelection: () => void;
  setSelection: (paths: Set<string>) => void;
  setViewMode: (mode: ViewMode) => void;
  setSearchQuery: (query: string) => void;
  setGridSize: (size: number) => void;
  toggleSort: (field: SortField) => void;
  refresh: () => void;
}

interface BrowserState {
  entries: FileEntry[];
  selection: Set<string>;
  lastSelectedPath: string | null;
}

/**
 * Factory: creates an independent browser store instance.
 * Each file browser panel gets its own store.
 */
export function createBrowserStore(initialPath?: string): BrowserStore {
  const storeId = `browser-${++browserIdCounter}`;
  const [currentPath, setCurrentPath] = createSignal(initialPath ?? "");
  const [viewMode, setViewMode] = createSignal<ViewMode>("list");
  const [sortField, setSortField] = createSignal<SortField>("name");
  const [sortDirection, setSortDirection] = createSignal<SortDirection>("asc");
  const [searchQuery, setSearchQuery] = createSignal("");
  const [isLoading, setIsLoading] = createSignal(false);
  const [gridSize, setGridSize] = createSignal(100);

  const [state, setState] = createStore<BrowserState>({
    entries: [],
    selection: new Set(),
    lastSelectedPath: null,
  });

  const [history, setHistory] = createSignal<string[]>([]);
  const [historyIndex, setHistoryIndex] = createSignal(-1);

  async function navigateTo(path: string, addToHistory = true) {
    setIsLoading(true);
    const isRefresh = path === currentPath() && !addToHistory;
    // Cancel pending thumbnail requests from previous directory. Only on
    // a FRESH navigation — on refresh (agent's 2s sync heartbeat), the
    // current folder's ThumbnailImage components are preserved, so
    // resolving their pending requests with `null` would pollute
    // `noThumbPaths` permanently for files whose thumbnails hadn't
    // finished generating yet.
    if (!isRefresh) {
      const { clearThumbnailQueue } = await import("../components/FileBrowser/ThumbnailImage");
      clearThumbnailQueue();
    }
    try {
      const entries = await listDirectory(path);

      // If this is a refresh and the entries are content-identical to
      // the current list (same paths, sizes, mtimes in the same order),
      // SKIP setState entirely. The mount-state heartbeat fires every 2s
      // and each state-update used to cascade into a full grid re-render
      // (even with reconcile, any in-flight thumbnail request got
      // interrupted by the setState pass), which was the "loads 2, pause,
      // 2 more, pause" pattern users saw.
      if (isRefresh && entriesEqual(state.entries, entries)) {
        setIsLoading(false);
        return;
      }
      if (isRefresh) {
        // Stable merge keyed by path: unchanged entries keep the SAME
        // object reference, so <For> doesn't unmount+remount grid rows
        // (and their ThumbnailImage children) every time the agent's 2s
        // sync heartbeat fires. Without this, every state-update poisons
        // the thumbnail cache via the backend's in-flight dedup and kills
        // thumbnail rendering entirely.
        setState("entries", reconcile(entries, { key: "path" }));
      } else {
        setState("entries", entries);
      }

      if (isRefresh) {
        // Preserve selection on same-directory refresh, but prune paths
        // that no longer exist (e.g. deleted files).
        //
        // Tree view: the selection can include paths several levels deep
        // (inside expanded subfolders), so we must include cached children
        // in the valid-path set. Without this, the 1-second mount-state
        // refresh tick wipes every selection that isn't at root level —
        // "selects then unselects fast" from the user's perspective.
        const validPaths = new Set<string>(entries.map((e) => e.path));
        for (const children of childrenCache().values()) {
          for (const child of children) {
            validPaths.add(child.path);
          }
        }
        const pruned = new Set(
          [...state.selection].filter((p) => validPaths.has(p))
        );
        setState("selection", pruned);
        if (state.lastSelectedPath && !validPaths.has(state.lastSelectedPath)) {
          setState("lastSelectedPath", null);
        }
      } else {
        setState("selection", new Set());
        setState("lastSelectedPath", null);
        // Fresh navigation — drop tree state from the previous root.
        // Refreshes (addToHistory=false, same path) keep tree state so
        // the user's expanded subtree survives a refresh tick.
        setExpandedPaths(new Set<string>());
        setChildrenCache(new Map<string, FileEntry[]>());
        setLoadingPaths(new Set<string>());
      }
      setCurrentPath(path);

      if (addToHistory) {
        const h = history();
        const idx = historyIndex();
        const newHistory = [...h.slice(0, idx + 1), path];
        setHistory(newHistory);
        setHistoryIndex(newHistory.length - 1);
      }
    } catch (err) {
      console.error("Failed to list directory:", err);
    } finally {
      setIsLoading(false);
    }
  }

  function goBack() {
    const idx = historyIndex();
    if (idx > 0) {
      setHistoryIndex(idx - 1);
      navigateTo(history()[idx - 1], false);
    }
  }

  function goForward() {
    const idx = historyIndex();
    const h = history();
    if (idx < h.length - 1) {
      setHistoryIndex(idx + 1);
      navigateTo(h[idx + 1], false);
    }
  }

  function goUp() {
    const path = currentPath();
    const sep = path.includes("/") ? "/" : "\\";
    const parts = path.split(sep).filter(Boolean);
    if (parts.length > 1) {
      parts.pop();
      let parent = parts.join(sep);
      if (parent.length === 2 && parent[1] === ":") {
        parent += sep;
      }
      navigateTo(parent);
    }
  }

  function selectItem(path: string, multi = false, range = false) {
    setState((prev) => {
      const newSelection = new Set(multi ? prev.selection : []);

      if (range && prev.lastSelectedPath) {
        // Shift-range walks the ORDERED SEQUENCE of rows currently visible
        // to the user. In list/grid that's `sortedEntries` (root only). In
        // tree view it's the flat `treeList` which also includes expanded
        // children — so shift-clicks that span across expanded subfolders
        // select every row in between, not just root-level items.
        const orderedPaths: string[] =
          viewMode() === "tree"
            ? treeList().map((r) => r.entry.path)
            : getSortedEntries().map((e) => e.path);

        const lastIdx = orderedPaths.indexOf(prev.lastSelectedPath);
        const curIdx = orderedPaths.indexOf(path);
        if (lastIdx !== -1 && curIdx !== -1) {
          const [start, end] = lastIdx < curIdx ? [lastIdx, curIdx] : [curIdx, lastIdx];
          for (let i = start; i <= end; i++) {
            newSelection.add(orderedPaths[i]);
          }
        }
      } else if (multi && newSelection.has(path)) {
        newSelection.delete(path);
      } else {
        newSelection.add(path);
      }

      return {
        ...prev,
        selection: newSelection,
        lastSelectedPath: path,
      };
    });
  }

  function selectAll() {
    setState("selection", new Set(state.entries.map((e) => e.path)));
  }

  function clearSelection() {
    setState("selection", new Set());
  }

  function setSelection(paths: Set<string>) {
    setState("selection", paths);
  }

  function toggleSort(field: SortField) {
    if (sortField() === field) {
      setSortDirection((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortField(field);
      setSortDirection("asc");
    }
  }

  /// Compare two entries per the browser's current sort state. Folders
  /// always come before files regardless of field. Used by list/grid and
  /// by each expansion level in tree view.
  function compareBySort(a: FileEntry, b: FileEntry): number {
    if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
    const dir = sortDirection() === "asc" ? 1 : -1;
    switch (sortField()) {
      case "name":
        return dir * a.name.toLowerCase().localeCompare(b.name.toLowerCase());
      case "size":
        return dir * (a.size - b.size);
      case "modified":
        return dir * ((a.modified ?? 0) - (b.modified ?? 0));
      case "extension":
        return dir * a.extension.toLowerCase().localeCompare(b.extension.toLowerCase());
      default:
        return 0;
    }
  }

  function getSortedEntries(): FileEntry[] {
    return [...state.entries].sort(compareBySort);
  }

  // ── Tree view state ──────────────────────────────────────────────────
  //
  // In-memory, session-scoped: expansion set + per-path children cache +
  // loading set. Lives for the lifetime of this BrowserStore instance.
  // Closing the tab / reloading the app discards it — matches "flat initial
  // load" behavior the user asked for. Each Set/Map is updated by replacing
  // the whole container so signal equality triggers re-render.

  const [expandedPaths, setExpandedPaths] = createSignal<Set<string>>(new Set());
  const [childrenCache, setChildrenCache] = createSignal<Map<string, FileEntry[]>>(new Map());
  const [loadingPaths, setLoadingPaths] = createSignal<Set<string>>(new Set());

  function isPathExpanded(path: string): boolean {
    return expandedPaths().has(path);
  }

  async function loadChildren(path: string) {
    // Delay the spinner by 150ms so fast loads don't flash. If the
    // listDirectory call finishes before that, we clear the timer and
    // never add the path to loadingPaths — no flicker for empty folders
    // or cached SMB paths.
    const spinnerTimer = setTimeout(() => {
      setLoadingPaths((prev) => {
        const next = new Set(prev);
        next.add(path);
        return next;
      });
    }, 150);
    try {
      const entries = await listDirectory(path);
      clearTimeout(spinnerTimer);
      setChildrenCache((prev) => {
        const next = new Map(prev);
        next.set(path, entries);
        return next;
      });
    } catch (err) {
      clearTimeout(spinnerTimer);
      console.error(`[tree] failed to load children of ${path}:`, err);
      // Leave the path expanded-but-empty; user can collapse/retry.
      setChildrenCache((prev) => {
        const next = new Map(prev);
        next.set(path, []);
        return next;
      });
    } finally {
      // Always clean loadingPaths in case the spinner already showed.
      setLoadingPaths((prev) => {
        if (!prev.has(path)) return prev;
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
    }
  }

  function toggleTreeExpand(path: string) {
    setExpandedPaths((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
        // First-time expand: fetch children asynchronously.
        if (!childrenCache().has(path)) {
          void loadChildren(path);
        }
      }
      return next;
    });
  }

  /// Compiled flat list for tree view — walks root entries and recursively
  /// inlines children of expanded paths. Per-level sort.
  ///
  /// Row identity is stable across calls: same path + same (entry, depth,
  /// isExpanded, isLoading) tuple returns the SAME TreeRow reference. Solid's
  /// `<For>` reconciles by reference, so stable identity prevents every row
  /// from unmounting+remounting on each toggle (which was resetting scroll
  /// position to the top of the tree).
  const rowCache = new Map<string, TreeRow>();
  function treeList(): TreeRow[] {
    const out: TreeRow[] = [];
    const expanded = expandedPaths();
    const cache = childrenCache();
    const loading = loadingPaths();
    const seen = new Set<string>();

    function rowFor(entry: FileEntry, depth: number, isExp: boolean, isLoad: boolean): TreeRow {
      const existing = rowCache.get(entry.path);
      if (
        existing &&
        existing.entry === entry &&
        existing.depth === depth &&
        existing.isExpanded === isExp &&
        existing.isLoading === isLoad
      ) {
        return existing;
      }
      const row: TreeRow = { entry, depth, isExpanded: isExp, isLoading: isLoad };
      rowCache.set(entry.path, row);
      return row;
    }

    function visit(entries: FileEntry[], depth: number) {
      const sorted = [...entries].sort(compareBySort);
      for (const entry of sorted) {
        const isExp = expanded.has(entry.path);
        out.push(rowFor(entry, depth, isExp, loading.has(entry.path)));
        seen.add(entry.path);
        if (entry.isDir && isExp) {
          const children = cache.get(entry.path);
          if (children) {
            visit(children, depth + 1);
          }
          // else: loading — spinner row is rendered in the view from
          // `isLoading` on the parent entry; no placeholder row here.
        }
      }
    }

    visit(state.entries, 0);
    // Evict rows that no longer appear (folder collapsed, tree truncated).
    // Without this, the cache grows unbounded across heavy expand/collapse.
    for (const key of rowCache.keys()) {
      if (!seen.has(key)) rowCache.delete(key);
    }
    return out;
  }

  // Auto-refresh when mount state changes (e.g. symlink created/switched)
  let refreshDebounce: ReturnType<typeof setTimeout> | null = null;
  const unlisten = listen("mount:state-update", () => {
    if (currentPath() && !refreshDebounce) {
      refreshDebounce = setTimeout(() => {
        refreshDebounce = null;
        navigateTo(currentPath(), false);
      }, 1000);
    }
  });

  // Refresh on global ufb:refresh event (F5, window focus, tab switch)
  let globalRefreshDebounce: ReturnType<typeof setTimeout> | null = null;
  function onGlobalRefresh() {
    if (currentPath() && !globalRefreshDebounce) {
      globalRefreshDebounce = setTimeout(() => {
        globalRefreshDebounce = null;
        navigateTo(currentPath(), false);
      }, 300);
    }
  }
  window.addEventListener("ufb:refresh", onGlobalRefresh);

  // Clean up listeners if store is used inside a reactive owner
  try {
    onCleanup(() => {
      unlisten.then(fn => fn());
      window.removeEventListener("ufb:refresh", onGlobalRefresh);
    });
  } catch {}

  // Init with path if provided
  if (initialPath) {
    navigateTo(initialPath);
  }

  return {
    id: storeId,
    currentPath,
    viewMode,
    sortField,
    sortDirection,
    searchQuery,
    isLoading,
    gridSize,
    canGoBack: () => historyIndex() > 0,
    canGoForward: () => historyIndex() < history().length - 1,
    get entries() {
      return state.entries;
    },
    get selection() {
      return state.selection;
    },
    sortedEntries: getSortedEntries,
    treeList,
    isPathExpanded,
    toggleTreeExpand,
    navigateTo,
    goBack,
    goForward,
    goUp,
    selectItem,
    selectAll,
    clearSelection,
    setSelection,
    setViewMode,
    setSearchQuery,
    setGridSize,
    toggleSort,
    refresh: () => navigateTo(currentPath(), false),
  };
}
