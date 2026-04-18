import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import type { BrowserStore } from "../../stores/fileStore";

interface NavigationBarProps {
  store: BrowserStore;
  onNewFolder?: () => void;
  onNewUFBFolder?: () => void;
  onNewDateFolder?: () => void;
  onNewTimeFolder?: () => void;
  onNewJob?: () => void;
  isProjectFolder?: boolean;
}

export function NavigationBar(props: NavigationBarProps) {
  const store = () => props.store;
  const [editing, setEditing] = createSignal(false);
  const [editPath, setEditPath] = createSignal("");

  const pathSegments = createMemo(() => {
    const path = store().currentPath();
    if (!path) return [];
    const sep = path.includes("/") ? "/" : "\\";
    const parts = path.split(sep).filter(Boolean);
    const segments: { label: string; path: string }[] = [];

    // Determine the root prefix that split+filter strips away:
    // Linux/macOS: "/" → prefix "/"
    // Windows drive: "C:\..." → no prefix (drive letter handles it)
    // UNC: "\\server\share" → prefix "\\"
    let prefix = "";
    if (sep === "/" && path.startsWith("/")) {
      prefix = "/";
    } else if (sep === "\\" && path.startsWith("\\\\")) {
      prefix = "\\\\";
    }

    let cumulative = prefix;
    for (const part of parts) {
      if (cumulative === prefix) {
        cumulative += part;
      } else {
        cumulative += sep + part;
      }
      let segPath = cumulative;
      // Windows drive root needs trailing backslash (e.g. "C:\")
      if (segments.length === 0 && part.endsWith(":")) {
        segPath += sep;
      }
      segments.push({ label: part, path: segPath });
    }
    return segments;
  });

  function startEditing() {
    setEditPath(store().currentPath());
    setEditing(true);
  }

  function commitPath() {
    const path = editPath().trim();
    if (path) {
      store().navigateTo(path);
    }
    setEditing(false);
  }

  function cancelEditing() {
    setEditing(false);
  }

  let scrollRef: HTMLDivElement | undefined;

  createEffect(() => {
    pathSegments();
    if (scrollRef) {
      requestAnimationFrame(() => {
        scrollRef!.scrollLeft = scrollRef!.scrollWidth;
      });
    }
  });

  function onPathKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      commitPath();
    } else if (e.key === "Escape") {
      cancelEditing();
    }
  }

  return (
    <div class="nav-bar-wrapper">
      {/* Row 1: Nav buttons + breadcrumb/path input + view toggle + refresh */}
      <div class="nav-bar">
        <button
          class="nav-btn"
          onClick={() => store().goBack()}
          disabled={!store().canGoBack()}
          title="Back"
        >
          <span class="icon">arrow_back</span>
        </button>
        <button
          class="nav-btn"
          onClick={() => store().goForward()}
          disabled={!store().canGoForward()}
          title="Forward"
        >
          <span class="icon">arrow_forward</span>
        </button>
        <button class="nav-btn" onClick={() => store().goUp()} title="Up">
          <span class="icon">arrow_upward</span>
        </button>

        <Show
          when={!editing()}
          fallback={
            <input
              class="path-input"
              type="text"
              value={editPath()}
              onInput={(e) => setEditPath(e.currentTarget.value)}
              onKeyDown={onPathKeyDown}
              onBlur={() => {
                setTimeout(() => {
                  if (editing()) commitPath();
                }, 100);
              }}
              ref={(el) => {
                requestAnimationFrame(() => {
                  el.focus();
                  el.select();
                });
              }}
            />
          }
        >
          <div class="breadcrumb" onClick={startEditing}>
            <div class="breadcrumb-scroll" ref={scrollRef}>
              <For each={pathSegments()}>
                {(seg, i) => (
                  <>
                    {i() > 0 && <span class="breadcrumb-separator"><span class="icon">chevron_right</span></span>}
                    <span
                      class="breadcrumb-segment"
                      onClick={(e) => {
                        e.stopPropagation();
                        store().navigateTo(seg.path);
                      }}
                    >
                      {seg.label}
                    </span>
                  </>
                )}
              </For>
            </div>
          </div>
        </Show>

        <div class="view-toggle">
          <button
            class={`nav-btn ${store().viewMode() === "list" ? "active" : ""}`}
            onClick={() => store().setViewMode("list")}
            title="List view"
          >
            <span class="icon">view_list</span>
          </button>
          <button
            class={`nav-btn ${store().viewMode() === "grid" ? "active" : ""}`}
            onClick={() => store().setViewMode("grid")}
            title="Grid view"
          >
            <span class="icon">grid_view</span>
          </button>
          <button
            class={`nav-btn ${store().viewMode() === "tree" ? "active" : ""}`}
            onClick={() => store().setViewMode("tree")}
            title="Tree view"
          >
            <span class="icon">account_tree</span>
          </button>
        </div>

        <button
          class="nav-btn"
          onClick={() => store().refresh()}
          title="Refresh"
        >
          <span class="icon">refresh</span>
        </button>
      </div>

      {/* Row 2: New folder buttons + Search + grid size slider */}
      <div class="nav-toolbar">
        <div class="toolbar-actions">
          <Show when={props.isProjectFolder && props.onNewJob}>
            <button class="nav-btn muted" onClick={props.onNewJob} title="New Job">
              <span class="icon">work</span>
            </button>
          </Show>
          <button class="nav-btn muted" onClick={props.onNewFolder} title="New Folder (Ctrl+Shift+N)">
            <span class="icon">create_new_folder</span>
          </button>
          <button class="nav-btn muted" onClick={props.onNewUFBFolder} title="New UFB Folder (YYMMDDx_Name)">
            <span class="icon">drive_file_rename_outline</span>
          </button>
          <button class="nav-btn muted" onClick={props.onNewDateFolder} title="New Date Folder (YYMMDD)">
            <span class="icon">calendar_add_on</span>
          </button>
          <button class="nav-btn muted" onClick={props.onNewTimeFolder} title="New Time Folder (HHMM)">
            <span class="icon">more_time</span>
          </button>
        </div>
        <input
          class="search-input"
          type="text"
          placeholder="Search..."
          value={store().searchQuery()}
          onInput={(e) => store().setSearchQuery(e.currentTarget.value)}
        />
        <Show when={store().viewMode() === "grid"}>
          <div class="sort-control">
            <button
              class={`nav-btn muted ${store().sortField() === "name" ? "active" : ""}`}
              onClick={() => store().toggleSort("name")}
              title="Sort by name"
            >
              <span class="icon">sort_by_alpha</span>
            </button>
            <button
              class={`nav-btn muted ${store().sortField() === "modified" ? "active" : ""}`}
              onClick={() => store().toggleSort("modified")}
              title="Sort by date"
            >
              <span class="icon">schedule</span>
            </button>
            <button
              class={`nav-btn muted ${store().sortField() === "size" ? "active" : ""}`}
              onClick={() => store().toggleSort("size")}
              title="Sort by size"
            >
              <span class="icon">straighten</span>
            </button>
          </div>
          <div class="grid-size-control">
            <span class="grid-size-label">Size</span>
            <input
              type="range"
              class="grid-size-slider"
              min="48"
              max="256"
              step="8"
              value={store().gridSize()}
              onInput={(e) => store().setGridSize(parseInt(e.currentTarget.value))}
            />
          </div>
        </Show>
      </div>
    </div>
  );
}
