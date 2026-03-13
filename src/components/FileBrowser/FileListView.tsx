import { For, Show, createMemo, createSignal } from "solid-js";
import type { BrowserStore } from "../../stores/fileStore";
import type { FileEntry } from "../../lib/types";
import { getFileIcon } from "../../lib/fileIcons";
import { makeColumnResizer } from "../../lib/useColumnResize";

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
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

interface FileListViewProps {
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

export function FileListView(props: FileListViewProps) {
  const store = () => props.store;

  // Resizable column widths
  const [sizeW, setSizeW] = createSignal(loadColWidth("size", 100));
  const [modifiedW, setModifiedW] = createSignal(loadColWidth("modified", 160));
  const [extW, setExtW] = createSignal(loadColWidth("ext", 80));

  const sizeResizer = makeColumnResizer({
    getWidth: sizeW, setWidth: setSizeW,
    onDone: (w) => saveColWidth("size", w),
  });
  const modifiedResizer = makeColumnResizer({
    getWidth: modifiedW, setWidth: setModifiedW,
    onDone: (w) => saveColWidth("modified", w),
  });
  const extResizer = makeColumnResizer({
    getWidth: extW, setWidth: setExtW,
    onDone: (w) => saveColWidth("ext", w),
  });

  const filteredEntries = createMemo(() => {
    const query = store().searchQuery().toLowerCase();
    const entries = store().sortedEntries();
    if (!query) return entries;
    return entries.filter((e) => e.name.toLowerCase().includes(query));
  });

  function handleClick(entry: FileEntry, e: MouseEvent) {
    if (e.detail === 2) {
      props.onItemDoubleClick(entry);
      return;
    }
    store().selectItem(entry.path, e.ctrlKey || e.metaKey, e.shiftKey);
  }

  return (
    <div class="file-list" onContextMenu={(e) => e.stopPropagation()}>
      <div class="file-list-header">
        <div class="file-list-header-cell col-icon" />
        <div
          class="file-list-header-cell col-name"
          onClick={() => store().toggleSort("name")}
        >
          Name
          {store().sortField() === "name" && (
            <span class="sort-arrow">
              {store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}
            </span>
          )}
        </div>
        <div
          class="file-list-header-cell col-size"
          style={{ width: `${sizeW()}px` }}
          onClick={() => store().toggleSort("size")}
        >
          Size
          {store().sortField() === "size" && (
            <span class="sort-arrow">
              {store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}
            </span>
          )}
        </div>
        <div class="col-resize-handle" onPointerDown={sizeResizer.onPointerDown} />
        <div
          class="file-list-header-cell col-modified"
          style={{ width: `${modifiedW()}px` }}
          onClick={() => store().toggleSort("modified")}
        >
          Modified
          {store().sortField() === "modified" && (
            <span class="sort-arrow">
              {store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}
            </span>
          )}
        </div>
        <div class="col-resize-handle" onPointerDown={modifiedResizer.onPointerDown} />
        <div
          class="file-list-header-cell col-ext"
          style={{ width: `${extW()}px` }}
          onClick={() => store().toggleSort("extension")}
        >
          Type
          {store().sortField() === "extension" && (
            <span class="sort-arrow">
              {store().sortDirection() === "asc" ? "\u25B2" : "\u25BC"}
            </span>
          )}
        </div>
        <div class="col-resize-handle" onPointerDown={extResizer.onPointerDown} />
        <Show when={props.isProjectFolder}>
          <div class="file-list-header-cell col-synced">Synced</div>
        </Show>
      </div>

      <For each={filteredEntries()}>
        {(entry) => (
          <div
            class={`file-row ${store().selection.has(entry.path) ? "selected" : ""}`}
            data-is-dir={entry.isDir ? "true" : "false"}
            data-path={entry.path}
            onClick={(e) => handleClick(entry, e)}
            onContextMenu={(e) => props.onItemContextMenu(e, entry)}
          >
            {(() => { const icon = getFileIcon(entry.extension, entry.isDir); return (
                <div class="file-cell col-icon file-icon" style={{ color: icon.color }}>
                  <span class="icon">{icon.icon}</span>
                </div>
              ); })()}
            <div class="file-cell file-name">{entry.name}</div>
            <div class="file-cell file-size" style={{ width: `${sizeW()}px` }}>
              {entry.isDir ? "" : formatSize(entry.size)}
            </div>
            <div class="file-cell file-modified" style={{ width: `${modifiedW()}px` }}>
              {formatDate(entry.modified)}
            </div>
            <div class="file-cell file-ext" style={{ width: `${extW()}px` }}>
              {entry.isDir ? "Folder" : entry.extension.toUpperCase()}
            </div>
            <Show when={props.isProjectFolder}>
              <div class="file-cell col-synced synced-cell">
                {entry.isDir && props.isSubscribed(entry.path) ? (
                  <span class="synced-check">{"\u2713"}</span>
                ) : null}
              </div>
            </Show>
          </div>
        )}
      </For>
    </div>
  );
}
