import { createSignal, createEffect, For, onMount, onCleanup, Show, on } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import {
  listDirectory,
  buildUfbUri,
  buildUnionUri,
  revealInFileManager,
  deleteToTrash,
  renamePath,
  getColumnDefs,
  getFolderMetadata,
  upsertItemMetadata,
  updateColumn,
  clipboardPaste,
  clipboardCopyPaths,
} from "../../lib/tauri";
import type { FileEntry, ColumnDefinition } from "../../lib/types";
import { renderCellValue as sharedRenderCellValue, formatDate } from "../../lib/cellRenderers";
import { makeColumnResizer } from "../../lib/useColumnResize";
import { ColumnManagerDialog } from "./ColumnManagerDialog";
import { workspaceStore } from "../../stores/workspaceStore";
import { adjustMenuPosition } from "../../lib/contextMenuPosition";
import "./ItemListPanel.css";

interface ItemListPanelProps {
  jobPath: string;
  folderPath: string;
  folderName: string;
  selectedItem: () => string | null;
  onSelectItem: (path: string) => void;
  onDoubleClickItem: (path: string) => void;
  onAddItem: () => void;
  hideAddButton?: boolean;
  /** Increment this signal to trigger a refresh */
  refreshTrigger?: () => number;
  /** Unique ID for drag-drop integration */
  browserId?: string;
}

/** Template/placeholder folder names to hide from the item list */
const TEMPLATE_FOLDER_NAMES = new Set([
  "_t_project_name",
  "000000a_shorthand_description",
  "000000_asset_nametype",
]);

type SortField = "name" | "modified" | string; // string for metadata column names

// ── Local visibility persistence via localStorage ──

function visibilityKey(jobPath: string, folderName: string): string {
  return `ufb-col-vis:${jobPath}:${folderName}`;
}

function loadVisibility(jobPath: string, folderName: string): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(visibilityKey(jobPath, folderName));
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function saveVisibility(jobPath: string, folderName: string, vis: Record<string, boolean>) {
  try {
    localStorage.setItem(visibilityKey(jobPath, folderName), JSON.stringify(vis));
  } catch { /* ignore */ }
}

export function ItemListPanel(props: ItemListPanelProps) {
  const [items, setItems] = createSignal<FileEntry[]>([]);
  const [columns, setColumns] = createSignal<ColumnDefinition[]>([]);
  const [metadataMap, setMetadataMap] = createSignal<Record<string, { json: Record<string, unknown>; isTracked: boolean }>>({});
  const [colVisibility, setColVisibility] = createSignal<Record<string, boolean>>({});
  const [sortField, setSortField] = createSignal<SortField>("name");
  const [sortDir, setSortDir] = createSignal<"asc" | "desc">("asc");
  const [ctxMenu, setCtxMenu] = createSignal<{ x: number; y: number; item: FileEntry } | null>(null);
  const [colHeaderMenu, setColHeaderMenu] = createSignal<{ x: number; y: number } | null>(null);
  const [editingCell, setEditingCell] = createSignal<{ itemPath: string; colName: string } | null>(null);
  const [showColumnManager, setShowColumnManager] = createSignal(false);
  const [overrideWidths, setOverrideWidths] = createSignal<Record<number, number>>({});
  const [saveError, setSaveError] = createSignal<string | null>(null);

  let panelRef: HTMLDivElement | undefined;

  // ── Keyboard shortcuts (paste) ──

  function onKeyDown(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;

    const modKey = e.ctrlKey || e.metaKey;

    if (modKey && e.key === "c") {
      e.preventDefault();
      const sel = props.selectedItem();
      if (sel) {
        clipboardCopyPaths([sel]).catch((err) => console.error("Copy failed:", err));
      }
    } else if (modKey && e.key === "v") {
      e.preventDefault();
      doPaste();
    }
  }

  async function doPaste() {
    try {
      await clipboardPaste(props.folderPath);
      refresh();
    } catch (err) {
      console.error("Paste failed:", err);
    }
  }

  function showError(msg: string) {
    setSaveError(msg);
    setTimeout(() => setSaveError(null), 4000);
  }

  function colWidth(col: ColumnDefinition): number {
    return overrideWidths()[col.id!] ?? col.columnWidth;
  }

  function makeMetaResizer(col: ColumnDefinition) {
    return makeColumnResizer({
      getWidth: () => colWidth(col),
      setWidth: (w) => setOverrideWidths(prev => ({ ...prev, [col.id!]: w })),
      onDone: (w) => {
        updateColumn({ ...col, columnWidth: w });
      },
    });
  }

  const visibleColumns = () => {
    const vis = colVisibility();
    return columns().filter(c => vis[c.columnName] !== false); // default visible
  };

  const sortedItems = () => {
    const list = [...items()];
    const field = sortField();
    const dir = sortDir() === "asc" ? 1 : -1;
    const meta = metadataMap();

    return list.sort((a, b) => {
      if (field === "name") {
        return dir * a.name.toLowerCase().localeCompare(b.name.toLowerCase());
      }
      if (field === "modified") {
        return dir * ((a.modified ?? 0) - (b.modified ?? 0));
      }
      // Sort by metadata column
      const aVal = meta[a.path]?.json?.[field] ?? "";
      const bVal = meta[b.path]?.json?.[field] ?? "";
      if (typeof aVal === "number" && typeof bVal === "number") {
        return dir * (aVal - bVal);
      }
      return dir * String(aVal).localeCompare(String(bVal));
    });
  };

  function toggleSort(field: SortField) {
    if (sortField() === field) {
      setSortDir(d => d === "asc" ? "desc" : "asc");
    } else {
      setSortField(field);
      setSortDir("asc");
    }
  }

  async function refresh() {
    try {
      const entries = await listDirectory(props.folderPath);
      const folders = entries
        .filter((e) => e.isDir && !e.name.startsWith(".") && !TEMPLATE_FOLDER_NAMES.has(e.name));
      setItems(folders);
    } catch (err) {
      console.error("Failed to load item list:", err);
    }
  }

  async function loadColumns() {
    try {
      const defs = await getColumnDefs(props.jobPath, props.folderName);
      setColumns(defs);
      // Load local visibility prefs
      const vis = loadVisibility(props.jobPath, props.folderName);
      setColVisibility(vis);
    } catch (err) {
      console.error("Failed to load columns:", err);
    }
  }

  async function loadMetadata() {
    try {
      const records = await getFolderMetadata(props.jobPath, props.folderName);
      const map: Record<string, { json: Record<string, unknown>; isTracked: boolean }> = {};
      for (const rec of records) {
        try {
          map[rec.itemPath] = {
            json: JSON.parse(rec.metadataJson || "{}"),
            isTracked: rec.isTracked,
          };
        } catch {
          map[rec.itemPath] = { json: {}, isTracked: rec.isTracked };
        }
      }
      setMetadataMap(map);
    } catch (err) {
      console.error("Failed to load metadata:", err);
    }
  }

  // Refresh on global ufb:refresh event (F5, window focus, tab switch)
  let globalRefreshDebounce: ReturnType<typeof setTimeout> | null = null;
  function onGlobalRefresh() {
    if (!globalRefreshDebounce) {
      globalRefreshDebounce = setTimeout(() => {
        globalRefreshDebounce = null;
        refresh();
        loadMetadata();
      }, 300);
    }
  }

  onMount(() => {
    refresh();
    loadColumns();
    loadMetadata();
    panelRef?.addEventListener("keydown", onKeyDown);
    window.addEventListener("ufb:refresh", onGlobalRefresh);
  });

  // Listen for mesh sync changes and refresh UI
  const unlistens: Promise<() => void>[] = [];

  unlistens.push(listen("mesh:table-changed", (event: any) => {
    const action = event?.payload?.action ?? "";
    if (action.startsWith("col_")) {
      loadColumns();
    }
    if (action === "sub_add" || action === "sub_remove") {
      loadMetadata();
    }
  }));

  // Metadata changed on a peer — reload metadata and folder listing
  unlistens.push(listen("mesh:metadata-changed", (event: any) => {
    const payload = event?.payload;
    // Reload if it's for our job, or if no filter info available
    if (!payload?.job_path || payload.job_path === props.jobPath) {
      loadMetadata();
      refresh();
    }
  }));

  // Full data refresh after snapshot restore
  unlistens.push(listen("mesh:data-refreshed", () => {
    refresh();
    loadColumns();
    loadMetadata();
    // Also refresh FileBrowser stores (right-side panels)
    window.dispatchEvent(new CustomEvent("ufb:refresh"));
  }));

  onCleanup(() => {
    unlistens.forEach(p => p.then(fn => fn()));
    panelRef?.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("ufb:refresh", onGlobalRefresh);
  });

  // Re-fetch when folder changes
  createEffect(on(() => props.folderPath, () => {
    refresh();
    loadColumns();
    loadMetadata();
  }, { defer: true }));

  // Watch the refresh trigger
  if (props.refreshTrigger) {
    createEffect(() => {
      props.refreshTrigger!();
      refresh();
      loadColumns();
      loadMetadata();
    });
  }

  // ── Metadata editing ──

  async function saveMetadata(itemPath: string, newJson: Record<string, unknown>, isTracked: boolean) {
    const jsonStr = JSON.stringify(newJson);
    try {
      await upsertItemMetadata(props.jobPath, itemPath, props.folderName, jsonStr, isTracked);
      // Update local map
      setMetadataMap(prev => ({
        ...prev,
        [itemPath]: { json: newJson, isTracked },
      }));
      // Notify other components (e.g. TrackerView) of the change
      window.dispatchEvent(new CustomEvent("ufb:metadata-changed", {
        detail: { jobPath: props.jobPath, itemPath, isTracked },
      }));
    } catch (err) {
      console.error("Failed to save metadata:", err);
      showError("Save failed");
    }
  }

  function updateCellValue(itemPath: string, colName: string, value: unknown) {
    const meta = metadataMap();
    const existing = meta[itemPath] ?? { json: {}, isTracked: false };
    const newJson = { ...existing.json, [colName]: value };
    saveMetadata(itemPath, newJson, existing.isTracked);
  }

  function toggleTracked(itemPath: string) {
    const meta = metadataMap();
    const existing = meta[itemPath] ?? { json: {}, isTracked: false };
    const newTracked = !existing.isTracked;
    const newJson = { ...existing.json };
    delete newJson.is_tracked; // DB column is authoritative
    saveMetadata(itemPath, newJson, newTracked);
  }

  // ── Column visibility ──

  function toggleColVisibility(colName: string) {
    const vis = { ...colVisibility() };
    vis[colName] = vis[colName] === false ? true : false;
    setColVisibility(vis);
    saveVisibility(props.jobPath, props.folderName, vis);
  }

  // ── Cell renderers (delegated to shared cellRenderers.tsx) ──

  function renderCellValue(item: FileEntry, col: ColumnDefinition) {
    const meta = metadataMap();
    const entry = meta[item.path];
    const val = entry?.json?.[col.columnName];
    const editing = editingCell();
    const isEditing = editing?.itemPath === item.path && editing?.colName === col.columnName;

    return sharedRenderCellValue({
      itemPath: item.path,
      value: val,
      col,
      isEditing,
      onUpdate: (v) => updateCellValue(item.path, col.columnName, v),
      onStartEdit: () => setEditingCell({ itemPath: item.path, colName: col.columnName }),
      onStopEdit: () => setEditingCell(null),
    });
  }

  // ── Context menu ──

  function onItemContextMenu(e: MouseEvent, item: FileEntry) {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ x: e.clientX, y: e.clientY, item });
  }

  function closeCtxMenu() { setCtxMenu(null); }

  async function ctxCopyPath() {
    const m = ctxMenu(); if (m) await navigator.clipboard.writeText(m.item.path);
    closeCtxMenu();
  }
  async function ctxCopyUfbLink() {
    const m = ctxMenu(); if (m) { const uri = await buildUfbUri(m.item.path); await navigator.clipboard.writeText(uri); }
    closeCtxMenu();
  }
  async function ctxCopyUnionLink() {
    const m = ctxMenu(); if (m) { const uri = await buildUnionUri(m.item.path); await navigator.clipboard.writeText(uri); }
    closeCtxMenu();
  }
  async function ctxReveal() {
    const m = ctxMenu(); if (m) await revealInFileManager(m.item.path);
    closeCtxMenu();
  }
  async function ctxRename() {
    const m = ctxMenu(); if (!m) return;
    closeCtxMenu();
    const newName = window.prompt("Rename to:", m.item.name);
    if (!newName || newName.trim() === m.item.name) return;
    const sep = m.item.path.includes("/") ? "/" : "\\";
    const parent = m.item.path.substring(0, m.item.path.lastIndexOf(sep));
    const newPath = `${parent}${sep}${newName.trim()}`;
    try {
      await renamePath(m.item.path, newPath);
      refresh();
    } catch (err) {
      console.error("Failed to rename:", err);
    }
  }
  async function ctxDelete() {
    const m = ctxMenu(); if (!m) return;
    closeCtxMenu();
    if (!window.confirm(`Delete "${m.item.name}"?`)) return;
    try {
      await deleteToTrash([m.item.path]);
      refresh();
      if (props.selectedItem() === m.item.path) {
        props.onSelectItem("");
      }
    } catch (err) {
      console.error("Failed to delete:", err);
    }
  }
  function ctxToggleTracked() {
    const m = ctxMenu(); if (!m) return;
    toggleTracked(m.item.path);
    closeCtxMenu();
  }

  function onColHeaderContextMenu(e: MouseEvent) {
    e.preventDefault();
    setColHeaderMenu({ x: e.clientX, y: e.clientY });
  }

  return (
    <div
      class="item-list-panel"
      ref={panelRef}
      tabIndex={0}
      data-drop-path={props.folderPath}
      data-browser-id={props.browserId}
      onClick={() => { closeCtxMenu(); setColHeaderMenu(null); }}
      onMouseDown={() => panelRef?.focus()}
    >
      <div class="item-list-header">
        <span class="item-list-header-title">
          {props.folderPath.split(/[\\/]/).pop() ?? "Items"}
        </span>
        <Show when={!props.hideAddButton}>
          <button class="item-list-add-btn" onClick={() => props.onAddItem()} title="Add item">
            +
          </button>
        </Show>
      </div>

      {/* Column headers */}
      <div class="item-list-columns" onContextMenu={onColHeaderContextMenu}>
        <div class="item-col-header col-track" title="Tracked">
          <span class="icon" style={{ "font-size": "14px" }}>star</span>
        </div>
        <div class="item-col-header col-name" onClick={() => toggleSort("name")}>
          Name
          {sortField() === "name" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
        </div>
        <For each={visibleColumns()}>
          {(col) => {
            const resizer = makeMetaResizer(col);
            return (
              <>
                <div
                  class="item-col-header col-meta"
                  style={{ width: `${colWidth(col)}px`, "min-width": `${Math.min(colWidth(col), 60)}px` }}
                  onClick={() => toggleSort(col.columnName)}
                >
                  {col.columnName}
                  {sortField() === col.columnName && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
                </div>
                <div class="col-resize-handle" onPointerDown={resizer.onPointerDown} />
              </>
            );
          }}
        </For>
        <div class="item-col-header col-date" onClick={() => toggleSort("modified")}>
          Modified
          {sortField() === "modified" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
        </div>
      </div>

      {/* Rows */}
      <div class="item-list-scroll">
        <Show when={sortedItems().length > 0} fallback={<div class="item-list-empty">No items</div>}>
          <For each={sortedItems()}>
            {(item) => {
              const meta = () => metadataMap()[item.path];
              const isTracked = () => meta()?.isTracked ?? false;
              return (
                <div
                  class={`item-row ${props.selectedItem() === item.path ? "selected" : ""}`}
                  data-path={item.path}
                  data-is-dir="true"
                  onClick={() => props.onSelectItem(item.path)}
                  onDblClick={() => props.onDoubleClickItem(item.path)}
                  onContextMenu={(e) => onItemContextMenu(e, item)}
                >
                  <span
                    class={`item-row-track ${isTracked() ? "tracked" : ""}`}
                    onClick={(e) => { e.stopPropagation(); toggleTracked(item.path); }}
                    title={isTracked() ? "Untrack" : "Track"}
                  >
                    <span class="icon">{isTracked() ? "star" : "star_border"}</span>
                  </span>
                  <span class="item-row-name">{item.name}</span>
                  <For each={visibleColumns()}>
                    {(col) => (
                      <span
                        class="item-row-meta"
                        style={{ width: `${colWidth(col)}px`, "min-width": `${Math.min(colWidth(col), 60)}px` }}
                      >
                        {renderCellValue(item, col)}
                      </span>
                    )}
                  </For>
                  <span class="item-row-date">{formatDate(item.modified)}</span>
                </div>
              );
            }}
          </For>
        </Show>
      </div>

      {/* Item context menu */}
      <Show when={ctxMenu()}>
        {(menu) => (
          <div class="ctx-menu" style={{ left: `${menu().x}px`, top: `${menu().y}px` }} ref={adjustMenuPosition}>
            <div class="ctx-menu-header truncate">{menu().item.name}</div>
            <div class="ctx-menu-item" onClick={() => { props.onSelectItem(menu().item.path); closeCtxMenu(); }}>
              <span class="icon">open_in_new</span> Open
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={() => { workspaceStore.navigateMainLeft(menu().item.path); closeCtxMenu(); }}>
              <span class="icon">arrow_back</span> Open in Left Browser
            </div>
            <div class="ctx-menu-item" onClick={() => { workspaceStore.navigateMainRight(menu().item.path); closeCtxMenu(); }}>
              <span class="icon">arrow_forward</span> Open in Right Browser
            </div>
            <div class="ctx-menu-item" onClick={() => { workspaceStore.openBrowserTab(menu().item.path); closeCtxMenu(); }}>
              <span class="icon">tab</span> Open in New Tab
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={ctxToggleTracked}>
              <span class="icon">{metadataMap()[menu().item.path]?.isTracked ? "star" : "star_border"}</span>
              {metadataMap()[menu().item.path]?.isTracked ? "Untrack" : "Track"}
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={ctxCopyPath}>
              <span class="icon">content_copy</span> Copy Path
            </div>
            <div class="ctx-menu-item" onClick={() => { closeCtxMenu(); doPaste(); }}>
              <span class="icon">content_paste</span> Paste Here
            </div>
            <div class="ctx-menu-item" onClick={ctxCopyUfbLink}>
              <span class="icon">link</span> Copy ufb:/// Link
            </div>
            <div class="ctx-menu-item" onClick={ctxCopyUnionLink}>
              <span class="icon">link</span> Copy union:/// Link
            </div>
            <div class="ctx-menu-item" onClick={ctxReveal}>
              <span class="icon">folder_open</span> Reveal in Explorer
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={ctxRename}>
              <span class="icon">edit</span> Rename
            </div>
            <div class="ctx-menu-item ctx-menu-danger" onClick={ctxDelete}>
              <span class="icon">delete</span> Delete
            </div>
          </div>
        )}
      </Show>

      {/* Column visibility menu (right-click on header) */}
      <Show when={colHeaderMenu()}>
        {(menu) => (
          <div class="ctx-menu col-vis-menu" style={{ left: `${menu().x}px`, top: `${menu().y}px` }} ref={adjustMenuPosition}>
            <div class="ctx-menu-header">Show Columns</div>
            <For each={columns()}>
              {(col) => {
                const vis = () => colVisibility()[col.columnName] !== false;
                return (
                  <div class="ctx-menu-item" onClick={(e) => { e.stopPropagation(); toggleColVisibility(col.columnName); }}>
                    <span class="icon">{vis() ? "check_box" : "check_box_outline_blank"}</span>
                    {col.columnName}
                  </div>
                );
              }}
            </For>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={(e) => { e.stopPropagation(); setColHeaderMenu(null); setShowColumnManager(true); }}>
              <span class="icon">settings</span> Manage Columns...
            </div>
          </div>
        )}
      </Show>

      {/* Column Manager Dialog */}
      <Show when={showColumnManager()}>
        <ColumnManagerDialog
          jobPath={props.jobPath}
          folderName={props.folderName}
          columns={columns()}
          onClose={() => setShowColumnManager(false)}
          onColumnsChanged={() => { loadColumns(); }}
        />
      </Show>

      <Show when={saveError()}>
        <div class="item-save-error">{saveError()}</div>
      </Show>
    </div>
  );
}
