import { For, Show, createMemo } from "solid-js";
import type { BrowserStore } from "../../stores/fileStore";
import type { FileEntry } from "../../lib/types";
import { ThumbnailImage } from "./ThumbnailImage";

interface FileGridViewProps {
  store: BrowserStore;
  isProjectFolder: boolean;
  isSubscribed: (path: string) => boolean;
  onItemContextMenu: (e: MouseEvent, entry: FileEntry) => void;
  onItemDoubleClick: (entry: FileEntry) => void;
}

export function FileGridView(props: FileGridViewProps) {
  const store = () => props.store;

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

  const thumbSize = () => store().gridSize();

  return (
    <div
      class="file-grid"
      style={{
        "grid-template-columns": `repeat(auto-fill, minmax(${thumbSize() + 24}px, 1fr))`,
      }}
      onContextMenu={(e) => e.stopPropagation()}
    >
      <For each={filteredEntries()}>
        {(entry) => (
          <div
            class={`grid-item ${store().selection.has(entry.path) ? "selected" : ""}`}
            data-is-dir={entry.isDir ? "true" : "false"}
            data-path={entry.path}
            onClick={(e) => handleClick(entry, e)}
            onContextMenu={(e) => props.onItemContextMenu(e, entry)}
          >
            <div class="grid-thumbnail-wrapper" style={{ position: "relative" }}>
              <ThumbnailImage
                filePath={entry.path}
                extension={entry.extension}
                isDir={entry.isDir}
                size={thumbSize()}
              />
              <Show when={props.isProjectFolder && entry.isDir && props.isSubscribed(entry.path)}>
                <span class="grid-synced-badge">{"\u2713"}</span>
              </Show>
            </div>
            <div class="grid-name">{entry.name}</div>
          </div>
        )}
      </For>
    </div>
  );
}
