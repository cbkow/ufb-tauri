import { For, Show, createMemo, createSignal, onMount } from "solid-js";
import type { BrowserStore, TreeRow } from "../../stores/fileStore";
import type { FileEntry } from "../../lib/types";
import { getFileIcon } from "../../lib/fileIcons";
import { getSystemIconCached } from "../../lib/systemIconCache";
import { makeColumnResizer } from "../../lib/useColumnResize";
import { settingsStore } from "../../stores/settingsStore";
import { adjustMenuPosition } from "../../lib/contextMenuPosition";

/// Tree view for a file browser — Finder-style list with disclosure
/// triangles on folders. Shares header + column widths with FileListView
/// (localStorage keys `ufb-file-col:*`), so switching list↔tree preserves
/// layout. Expansion + children cache live on the BrowserStore.

function formatSize(bytes: number): string {
  if (bytes === 0) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  const val = bytes / Math.pow(1024, i);
  return `${val.toFixed(i > 0 ? 1 : 0)} ${units[i]}`;
}

function formatDate(ms: number | null): string {
  if (!ms) return "";
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, {
    year: "numeric", month: "short", day: "numeric",
    hour: "2-digit", minute: "2-digit",
  });
}

function FileIconCell(props: { extension: string; isDir: boolean }) {
  const [sysIcon, setSysIcon] = createSignal<string | null>(null);
  const icon = () => getFileIcon(props.extension, props.isDir);
  onMount(() => {
    const ext = props.isDir ? "folder" : props.extension;
    if (!ext) return;
    getSystemIconCached(ext, 32).then((url) => { if (url) setSysIcon(url); });
  });
  return (
    <div class="file-cell col-icon file-icon" style={{ color: sysIcon() ? undefined : icon().color }}>
      {sysIcon() ? (
        <img src={sysIcon()!} alt="" width={16} height={16} style={{ "vertical-align": "middle" }} draggable={false} />
      ) : (
        <span class="icon">{icon().icon}</span>
      )}
    </div>
  );
}

interface FileTreeViewProps {
  store: BrowserStore;
  isProjectFolder: boolean;
  isSubscribed: (path: string) => boolean;
  onItemContextMenu: (e: MouseEvent, entry: FileEntry) => void;
  onItemDoubleClick: (entry: FileEntry) => void;
}

function loadColWidth(colId: string, fallback: number): number {
  try {
    const v = localStorage.getItem(`ufb-file-col:${colId}`);
    return v ? Number(v) : fallback;
  } catch { return fallback; }
}
function saveColWidth(colId: string, w: number) {
  try { localStorage.setItem(`ufb-file-col:${colId}`, String(Math.round(w))); } catch { /* */ }
}

/// Leading indent per tree depth, in pixels. Matches Finder's roughly
/// 18-20px step. Name column has a fixed width; indent eats into it.
const INDENT_STEP_PX = 18;

export function FileTreeView(props: FileTreeViewProps) {
  const store = () => props.store;

  const [nameW, setNameW] = createSignal(loadColWidth("name", 300));
  const [sizeW, setSizeW] = createSignal(loadColWidth("size", 100));
  const [modifiedW, setModifiedW] = createSignal(loadColWidth("modified", 160));
  const [extW, setExtW] = createSignal(loadColWidth("ext", 80));
  const [syncedW, setSyncedW] = createSignal(loadColWidth("synced", 60));

  const nameResizer = makeColumnResizer({ getWidth: nameW, setWidth: setNameW, onDone: (w) => saveColWidth("name", w) });
  const sizeResizer = makeColumnResizer({ getWidth: sizeW, setWidth: setSizeW, onDone: (w) => saveColWidth("size", w) });
  const modifiedResizer = makeColumnResizer({ getWidth: modifiedW, setWidth: setModifiedW, onDone: (w) => saveColWidth("modified", w) });
  const extResizer = makeColumnResizer({ getWidth: extW, setWidth: setExtW, onDone: (w) => saveColWidth("ext", w) });
  const syncedResizer = makeColumnResizer({ getWidth: syncedW, setWidth: setSyncedW, onDone: (w) => saveColWidth("synced", w) });

  const showSize = () => settingsStore.settings.ui.browserColumns?.size ?? true;
  const showModified = () => settingsStore.settings.ui.browserColumns?.modified ?? true;
  const showType = () => settingsStore.settings.ui.browserColumns?.type ?? true;

  const [headerMenu, setHeaderMenu] = createSignal<{ x: number; y: number } | null>(null);

  function onHeaderContextMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    setHeaderMenu({ x: e.clientX, y: e.clientY });
  }

  function toggleColumn(col: "size" | "modified" | "type") {
    const current = settingsStore.settings.ui.browserColumns ?? { size: true, modified: true, type: true };
    settingsStore.setSettings("ui", "browserColumns", { ...current, [col]: !current[col] });
    settingsStore.save();
  }

  const filteredRows = createMemo(() => {
    const query = store().searchQuery().toLowerCase();
    const rows = store().treeList();
    if (!query) return rows;
    // Search filters the compiled tree; we keep any row whose name
    // matches. A stricter model would also include ancestors of matches
    // for context, but that's a separate iteration.
    return rows.filter((r) => r.entry.name.toLowerCase().includes(query));
  });

  function handleClick(entry: FileEntry, e: MouseEvent) {
    if (e.detail === 2) {
      props.onItemDoubleClick(entry);
      return;
    }
    store().selectItem(entry.path, e.ctrlKey || e.metaKey, e.shiftKey);
  }

  function onChevronClick(e: MouseEvent, row: TreeRow) {
    e.stopPropagation();
    if (!row.entry.isDir) return;
    store().toggleTreeExpand(row.entry.path);
  }

  return (
    <div class="file-list file-tree" onContextMenu={(e) => e.stopPropagation()}>
      <div class="file-list-header" onContextMenu={onHeaderContextMenu}>
        <div class="file-list-header-cell col-icon" />
        <div
          class="file-list-header-cell col-name"
          style={{ width: `${nameW()}px` }}
          onClick={() => store().toggleSort("name")}
        >
          Name
          {store().sortField() === "name" && (
            <span class="sort-arrow">
              {store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}
            </span>
          )}
        </div>
        <div class="col-resize-handle" onPointerDown={nameResizer.onPointerDown} />
        <Show when={showSize()}>
          <div class="file-list-header-cell col-size" style={{ width: `${sizeW()}px` }} onClick={() => store().toggleSort("size")}>
            Size
            {store().sortField() === "size" && <span class="sort-arrow">{store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
          <div class="col-resize-handle" onPointerDown={sizeResizer.onPointerDown} />
        </Show>
        <Show when={showModified()}>
          <div class="file-list-header-cell col-modified" style={{ width: `${modifiedW()}px` }} onClick={() => store().toggleSort("modified")}>
            Modified
            {store().sortField() === "modified" && <span class="sort-arrow">{store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
          <div class="col-resize-handle" onPointerDown={modifiedResizer.onPointerDown} />
        </Show>
        <Show when={showType()}>
          <div class="file-list-header-cell col-ext" style={{ width: `${extW()}px` }} onClick={() => store().toggleSort("extension")}>
            Type
            {store().sortField() === "extension" && <span class="sort-arrow">{store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
          <div class="col-resize-handle" onPointerDown={extResizer.onPointerDown} />
        </Show>
        <Show when={props.isProjectFolder}>
          <div class="file-list-header-cell col-synced" style={{ width: `${syncedW()}px` }}>Synced</div>
          <div class="col-resize-handle" onPointerDown={syncedResizer.onPointerDown} />
        </Show>
      </div>

      <For each={filteredRows()}>
        {(row) => (
          <>
            <div
              class={`file-row tree-row ${store().selection.has(row.entry.path) ? "selected" : ""}`}
              data-is-dir={row.entry.isDir ? "true" : "false"}
              data-path={row.entry.path}
              data-tree-depth={row.depth}
              onClick={(e) => handleClick(row.entry, e)}
              onContextMenu={(e) => props.onItemContextMenu(e, row.entry)}
            >
              <div
                class="tree-indent"
                style={{ width: `${row.depth * INDENT_STEP_PX}px` }}
              />
              <button
                class={`tree-chevron ${row.entry.isDir ? "" : "tree-chevron-hidden"} ${row.isExpanded ? "expanded" : ""}`}
                onClick={(e) => onChevronClick(e, row)}
                tabindex="-1"
                aria-label={row.isExpanded ? "Collapse" : "Expand"}
              >
                <span class="icon">chevron_right</span>
              </button>
              <FileIconCell extension={row.entry.extension} isDir={row.entry.isDir} />
              <div class="file-cell file-name" style={{ width: `${nameW()}px` }}>{row.entry.name}</div>
              <Show when={showSize()}>
                <div class="file-cell file-size" style={{ width: `${sizeW()}px` }}>
                  {row.entry.isDir ? "" : formatSize(row.entry.size)}
                </div>
              </Show>
              <Show when={showModified()}>
                <div class="file-cell file-modified" style={{ width: `${modifiedW()}px` }}>
                  {formatDate(row.entry.modified)}
                </div>
              </Show>
              <Show when={showType()}>
                <div class="file-cell file-ext" style={{ width: `${extW()}px` }}>
                  {row.entry.isDir ? "Folder" : row.entry.extension.toUpperCase()}
                </div>
              </Show>
              <Show when={props.isProjectFolder}>
                <div class="file-cell col-synced synced-cell" style={{ width: `${syncedW()}px` }}>
                  {row.entry.isDir && props.isSubscribed(row.entry.path) ? (
                    <span class="synced-check">{"\u2713"}</span>
                  ) : null}
                </div>
              </Show>
            </div>
            <Show when={row.isLoading}>
              <div class="file-row tree-loading-row" data-tree-depth={row.depth + 1}>
                <div class="tree-indent" style={{ width: `${(row.depth + 1) * INDENT_STEP_PX}px` }} />
                <div class="tree-spinner" />
                <span class="tree-loading-text">Loading…</span>
              </div>
            </Show>
          </>
        )}
      </For>

      <Show when={headerMenu()}>
        {(menu) => (
          <div
            class="col-vis-menu"
            style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
            ref={adjustMenuPosition}
            onClick={(e) => e.stopPropagation()}
          >
            <div class="col-vis-header">Show Columns</div>
            <div class="col-vis-item" onClick={() => toggleColumn("size")}>
              <span class="icon">{showSize() ? "check_box" : "check_box_outline_blank"}</span>
              Size
            </div>
            <div class="col-vis-item" onClick={() => toggleColumn("modified")}>
              <span class="icon">{showModified() ? "check_box" : "check_box_outline_blank"}</span>
              Modified
            </div>
            <div class="col-vis-item" onClick={() => toggleColumn("type")}>
              <span class="icon">{showType() ? "check_box" : "check_box_outline_blank"}</span>
              Type
            </div>
          </div>
        )}
      </Show>
      <Show when={headerMenu()}>
        <div class="col-vis-backdrop" onClick={() => setHeaderMenu(null)} />
      </Show>
    </div>
  );
}
