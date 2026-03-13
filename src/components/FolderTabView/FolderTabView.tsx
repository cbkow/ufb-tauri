import { createSignal, onMount, Show } from "solid-js";
import { createBrowserStore } from "../../stores/fileStore";
import { Splitter } from "../shared/Splitter";
import { FileBrowser } from "../FileBrowser/FileBrowser";
import { ItemListPanel } from "./ItemListPanel";
import {
  listDirectory,
  createDirectory,
  createItemFromTemplate,
  createDatePrefixedItem,
  detectFolderLayoutMode,
  getFolderAddMode,
} from "../../lib/tauri";
import { useBrowserDragDrop } from "../../lib/useBrowserDragDrop";
import "./FolderTabView.css";

interface FolderTabViewProps {
  jobPath: string;
  folderPath: string;
  folderName: string;
}

export function FolderTabView(props: FolderTabViewProps) {
  const [selectedItem, setSelectedItem] = createSignal<string | null>(null);
  const [refreshTrigger, setRefreshTrigger] = createSignal(0);
  const [layoutMode, setLayoutMode] = createSignal<"B" | "C" | null>(null);

  // ── Add item modal ──
  const [showAddModal, setShowAddModal] = createSignal(false);
  const [addItemName, setAddItemName] = createSignal("");
  const [addMode, setAddMode] = createSignal<"shot" | "date_prefixed" | "folder" | "none">("folder");

  // Create browser stores at component init (stable references)
  const mainBrowser = createBrowserStore();
  const rendersBrowser = createBrowserStore();

  // ── Drag/drop support ──
  let externalDropMain: ((paths: string[]) => void) | undefined;
  let externalDropRenders: ((paths: string[]) => void) | undefined;

  const activeDrag = useBrowserDragDrop({
    getBrowserStore: (id) => {
      if (id === mainBrowser.id) return mainBrowser;
      if (id === rendersBrowser.id) return rendersBrowser;
      return null;
    },
    getExternalDropHandler: (id) => {
      if (id === mainBrowser.id) return externalDropMain;
      if (id === rendersBrowser.id) return externalDropRenders;
      return externalDropMain;
    },
    enableCrossBrowserDrag: false,
  });

  onMount(async () => {
    // Detect layout mode
    try {
      const mode = await detectFolderLayoutMode(props.jobPath, props.folderName);
      setLayoutMode(mode === "C" ? "C" : "B");
    } catch {
      setLayoutMode("B");
    }

    // Detect add-item mode from bundled template
    try {
      const mode = await getFolderAddMode(props.folderName);
      setAddMode(mode as "shot" | "date_prefixed" | "folder" | "none");
    } catch {
      setAddMode("folder");
    }
  });

  const PROJECT_NAMES = new Set([
    "project", "projects", "scenes", "scene", "work", "source", "src",
    "maya", "houdini", "nuke", "ae", "c4d", "blender", "flame", "fusion",
    "resolve", "premiere", "aftereffects", "pfx", "matchmove", "roto",
  ]);
  const RENDER_NAMES = new Set([
    "renders", "render", "output", "outputs", "comp", "comps",
    "export", "exports", "deliverables", "plates", "precomp",
  ]);

  async function handleSelect(itemPath: string) {
    setSelectedItem(itemPath);

    if (layoutMode() === "C") {
      try {
        const children = await listDirectory(itemPath);
        const childDirs = children
          .filter((e) => e.isDir)
          .map((e) => ({ name: e.name.toLowerCase(), path: e.path }));

        const projectDir = childDirs.find((c) => PROJECT_NAMES.has(c.name));
        mainBrowser.navigateTo(projectDir ? projectDir.path : itemPath);

        const rendersDir = childDirs.find((c) => RENDER_NAMES.has(c.name));
        rendersBrowser.navigateTo(rendersDir ? rendersDir.path : itemPath);
      } catch (err) {
        console.error("Failed to inspect item subfolders:", err);
        mainBrowser.navigateTo(itemPath);
        rendersBrowser.navigateTo(itemPath);
      }
    } else {
      mainBrowser.navigateTo(itemPath);
    }
  }

  function handleDoubleClick(itemPath: string) {
    handleSelect(itemPath);
  }

  function getDatePrefix(): string {
    const now = new Date();
    const yy = String(now.getFullYear()).slice(2);
    const mm = String(now.getMonth() + 1).padStart(2, "0");
    const dd = String(now.getDate()).padStart(2, "0");
    return `${yy}${mm}${dd}`;
  }

  function nextSuffix(): string {
    return "a"; // Approximate preview; backend picks actual available suffix
  }

  function addModalTitle(): string {
    const mode = addMode();
    if (mode === "shot") return "New Shot";
    if (mode === "date_prefixed") return "New Item";
    return "New Folder";
  }

  function addModalPlaceholder(): string {
    const mode = addMode();
    if (mode === "shot") return "Shot name...";
    if (mode === "date_prefixed") return "Item base name...";
    return "Folder name...";
  }

  function handleAddItem() {
    setAddItemName("");
    setShowAddModal(true);
  }

  async function submitAddItem() {
    const name = addItemName().trim();
    if (!name) { setShowAddModal(false); return; }
    const mode = addMode();

    try {
      if (mode === "shot") {
        const created = await createItemFromTemplate(props.jobPath, props.folderPath, name);
        setRefreshTrigger((n) => n + 1);
        handleSelect(created);
      } else if (mode === "date_prefixed") {
        const created = await createDatePrefixedItem(props.folderPath, name);
        setRefreshTrigger((n) => n + 1);
        handleSelect(created);
      } else {
        const sep = props.folderPath.includes("/") ? "/" : "\\";
        const newPath = `${props.folderPath}${sep}${name}`;
        await createDirectory(newPath);
        setRefreshTrigger((n) => n + 1);
        handleSelect(newPath);
      }
    } catch (err) {
      console.error("Failed to create item:", err);
    }
    setShowAddModal(false);
  }

  const itemListPanel = () => (
    <ItemListPanel
      jobPath={props.jobPath}
      folderPath={props.folderPath}
      folderName={props.folderName}
      selectedItem={selectedItem}
      onSelectItem={handleSelect}
      onDoubleClickItem={handleDoubleClick}
      onAddItem={handleAddItem}
      hideAddButton={addMode() === "none"}
      refreshTrigger={refreshTrigger}
    />
  );

  return (
    <div class="folder-tab-view">
      <Show when={layoutMode() === "C"} fallback={
        <Show when={layoutMode() === "B"}>
          <Splitter
            direction="horizontal"
            initialSize={420}
            minSize={200}
            minSecondSize={300}
            first={itemListPanel()}
            second={
              <FileBrowser
                store={mainBrowser}
                onExternalDrop={(h) => { externalDropMain = h; }}
              />
            }
          />
        </Show>
      }>
        <Splitter
          direction="horizontal"
          initialSize={420}
          minSize={200}
          minSecondSize={400}
          first={itemListPanel()}
          second={
            <Splitter
              direction="vertical"
              initialRatio={0.5}
              minSize={150}
              minSecondSize={150}
              first={
                <FileBrowser
                  store={mainBrowser}
                  onExternalDrop={(h) => { externalDropMain = h; }}
                />
              }
              second={
                <FileBrowser
                  store={rendersBrowser}
                  onExternalDrop={(h) => { externalDropRenders = h; }}
                />
              }
            />
          }
        />
      </Show>
      {/* Drag overlay indicator */}
      <Show when={activeDrag()}>
        {(drag) => (
          <div
            class="drag-overlay"
            style={{
              left: `${drag().x + 12}px`,
              top: `${drag().y + 12}px`,
            }}
          >
            {drag().paths.length === 1
              ? drag().paths[0].split(/[\\/]/).pop()
              : `${drag().paths.length} items`}
          </div>
        )}
      </Show>
      {/* ── Add Item Modal ── */}
      <Show when={showAddModal()}>
        <div class="modal-overlay">
          <div class="browser-modal">
            <div class="browser-modal-title">{addModalTitle()}</div>
            <div class="browser-modal-body">
              <Show when={addMode() === "date_prefixed"}>
                <span class="browser-modal-hint">
                  Creates: {getDatePrefix()}{nextSuffix()}_{addItemName() || "name"}
                </span>
              </Show>
              <input
                class="browser-modal-input"
                type="text"
                value={addItemName()}
                onInput={(e) => setAddItemName(e.currentTarget.value)}
                placeholder={addModalPlaceholder()}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitAddItem();
                  if (e.key === "Escape") setShowAddModal(false);
                }}
                ref={(el) => requestAnimationFrame(() => {
                  el.focus();
                  el.select();
                })}
              />
            </div>
            <div class="browser-modal-actions">
              <button class="modal-btn" onClick={() => setShowAddModal(false)}>Cancel</button>
              <button
                class="modal-btn modal-btn-primary"
                onClick={submitAddItem}
                disabled={!addItemName().trim()}
              >
                Create
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
