import { createSignal, onCleanup } from "solid-js";
import { createStore } from "solid-js/store";
import type { FileEntry } from "../lib/types";
import { listDirectory } from "../lib/tauri";
import { listen } from "@tauri-apps/api/event";

export type SortField = "name" | "size" | "modified" | "extension";
export type SortDirection = "asc" | "desc";
export type ViewMode = "list" | "grid";

let browserIdCounter = 0;

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
    // Cancel pending thumbnail requests from previous directory
    const { clearThumbnailQueue } = await import("../components/FileBrowser/ThumbnailImage");
    clearThumbnailQueue();
    try {
      const entries = await listDirectory(path);
      setState("entries", entries);
      setState("selection", new Set());
      setState("lastSelectedPath", null);
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
        const entries = prev.entries;
        const lastIdx = entries.findIndex((e) => e.path === prev.lastSelectedPath);
        const curIdx = entries.findIndex((e) => e.path === path);
        if (lastIdx !== -1 && curIdx !== -1) {
          const [start, end] = lastIdx < curIdx ? [lastIdx, curIdx] : [curIdx, lastIdx];
          for (let i = start; i <= end; i++) {
            newSelection.add(entries[i].path);
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

  function getSortedEntries(): FileEntry[] {
    const entries = [...state.entries];
    const field = sortField();
    const dir = sortDirection() === "asc" ? 1 : -1;

    return entries.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      switch (field) {
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
    });
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
