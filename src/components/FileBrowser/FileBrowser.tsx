import { createSignal, createMemo, Show, onMount, onCleanup } from "solid-js";
import type { BrowserStore } from "../../stores/fileStore";
import type { FileEntry } from "../../lib/types";
import { subscriptionStore } from "../../stores/subscriptionStore";
import {
  openFile,
  revealInFileManager,
  clipboardCopyPaths,
  clipboardPaste,
  deleteToTrash,
  renamePath,
  createDirectory,
  copyFiles,
  buildUfbUri,
  buildUnionUri,
  createJobFromTemplate,
} from "../../lib/tauri";
import { transcodeStore } from "../../stores/transcodeStore";
import { workspaceStore } from "../../stores/workspaceStore";
import { adjustMenuPosition } from "../../lib/contextMenuPosition";
import { NavigationBar } from "./NavigationBar";
import { FileListView } from "./FileListView";
import { FileGridView } from "./FileGridView";
import { useRubberBandSelect } from "../../lib/useRubberBandSelect";
import "./FileBrowser.css";

export interface FileBrowserCallbacks {
  onOpenInOtherBrowser?: (path: string) => void;
  onOpenInNewTab?: (path: string) => void;
}

interface FileBrowserProps {
  store: BrowserStore;
  callbacks?: FileBrowserCallbacks;
  /** Register a handler for external file drops (from Explorer via Tauri) */
  onExternalDrop?: (handler: (paths: string[]) => void) => void;
}

export function FileBrowser(props: FileBrowserProps) {
  const store = () => props.store;

  // ── Project folder detection ──
  const isProjectFolder = createMemo(() => {
    const currentPath = store().currentPath();
    if (!currentPath) return false;
    const normalized = currentPath.replace(/[\\/]+$/, "").toLowerCase();
    return subscriptionStore.bookmarks.some(
      (b) => b.isProjectFolder && b.path.replace(/[\\/]+$/, "").toLowerCase() === normalized
    );
  });

  // ── Set of subscribed job paths for quick lookup ──
  const subscribedPaths = createMemo(() => {
    const set = new Set<string>();
    for (const sub of subscriptionStore.subscriptions) {
      set.add(sub.jobPath.replace(/[\\/]+$/, "").toLowerCase());
    }
    return set;
  });

  function isSubscribed(entryPath: string): boolean {
    return subscribedPaths().has(entryPath.replace(/[\\/]+$/, "").toLowerCase());
  }

  async function syncAsJob(entry: FileEntry) {
    if (!entry.isDir) return;
    await subscriptionStore.subscribe(entry.path, entry.name);
    closeCtxMenu();
  }

  // ── Context menu state ──
  const [ctxMenu, setCtxMenu] = createSignal<{
    x: number;
    y: number;
    entry: FileEntry | null; // null = background click
  } | null>(null);

  // ── Rename modal ──
  const [renameTarget, setRenameTarget] = createSignal<FileEntry | null>(null);
  const [renameName, setRenameName] = createSignal("");

  // ── New folder modal ──
  type NewFolderMode = "folder" | "ufb" | null;
  const [newFolderMode, setNewFolderMode] = createSignal<NewFolderMode>(null);
  const [newFolderName, setNewFolderName] = createSignal("");

  // ── New Job modal ──
  const [showNewJobModal, setShowNewJobModal] = createSignal(false);
  const [jobNumber, setJobNumber] = createSignal("");
  const [jobName, setJobName] = createSignal("");

  // ── Cut state ──
  const [cutPaths, setCutPaths] = createSignal<string[]>([]);

  let containerRef: HTMLDivElement | undefined;
  let contentRef: HTMLDivElement | undefined;

  // ── Rubber-band selection ──
  const rubberBand = useRubberBandSelect(
    () => contentRef,
    () => store(),
  );

  // ── Close context menu on any click ──
  function closeCtxMenu() {
    setCtxMenu(null);
  }

  // ── File actions ──

  function openEntry(entry: FileEntry) {
    if (entry.isDir) {
      store().navigateTo(entry.path);
    } else {
      openFile(entry.path).catch((e) => console.error("Failed to open file:", e));
    }
  }

  function getSelectedPaths(): string[] {
    const sel = store().selection;
    return sel.size > 0 ? [...sel] : [];
  }

  function getContextPaths(entry: FileEntry | null): string[] {
    if (!entry) return [];
    const sel = store().selection;
    // If right-clicked item is in selection, operate on whole selection
    if (sel.has(entry.path)) return [...sel];
    return [entry.path];
  }

  async function doCopy(entry: FileEntry | null) {
    const paths = getContextPaths(entry);
    if (paths.length === 0) return;
    setCutPaths([]);
    await clipboardCopyPaths(paths).catch((e) => console.error("Copy failed:", e));
    closeCtxMenu();
  }

  async function doCut(entry: FileEntry | null) {
    const paths = getContextPaths(entry);
    if (paths.length === 0) return;
    setCutPaths(paths);
    await clipboardCopyPaths(paths).catch((e) => console.error("Cut failed:", e));
    closeCtxMenu();
  }

  async function doPaste() {
    const dest = store().currentPath();
    try {
      await clipboardPaste(dest);
      // If this was a cut operation, delete originals
      const cuts = cutPaths();
      if (cuts.length > 0) {
        await deleteToTrash(cuts);
        setCutPaths([]);
      }
      store().refresh();
    } catch (e) {
      console.error("Paste failed:", e);
    }
    closeCtxMenu();
  }

  async function doDelete(entry: FileEntry | null) {
    const paths = getContextPaths(entry);
    if (paths.length === 0) return;
    try {
      await deleteToTrash(paths);
      store().refresh();
    } catch (e) {
      console.error("Delete failed:", e);
    }
    closeCtxMenu();
  }

  function startRename(entry: FileEntry | null) {
    if (!entry) return;
    setRenameName(entry.name);
    setRenameTarget(entry);
    closeCtxMenu();
  }

  async function submitRename() {
    const target = renameTarget();
    const name = renameName().trim();
    if (!target || !name || name === target.name) {
      setRenameTarget(null);
      return;
    }
    const sep = target.path.includes("/") ? "/" : "\\";
    const parentParts = target.path.split(sep);
    parentParts.pop();
    const newPath = parentParts.join(sep) + sep + name;
    try {
      await renamePath(target.path, newPath);
      store().refresh();
    } catch (e) {
      console.error("Rename failed:", e);
    }
    setRenameTarget(null);
  }

  async function doCopyPath(entry: FileEntry | null) {
    const paths = getContextPaths(entry);
    const text = paths.join("\n");
    await navigator.clipboard.writeText(text).catch(() => {});
    closeCtxMenu();
  }

  async function doCopyFilename(entry: FileEntry | null) {
    if (!entry) return;
    await navigator.clipboard.writeText(entry.name).catch(() => {});
    closeCtxMenu();
  }

  async function doCopyUfbLink(entry: FileEntry | null) {
    const path = entry?.path ?? store().currentPath();
    try {
      const uri = await buildUfbUri(path);
      await navigator.clipboard.writeText(uri);
    } catch (e) {
      console.error("Copy ufb link failed:", e);
    }
    closeCtxMenu();
  }

  async function doCopyUnionLink(entry: FileEntry | null) {
    const path = entry?.path ?? store().currentPath();
    try {
      const uri = await buildUnionUri(path);
      await navigator.clipboard.writeText(uri);
    } catch (e) {
      console.error("Copy union link failed:", e);
    }
    closeCtxMenu();
  }

  const VIDEO_EXTENSIONS = new Set([".mov", ".avi", ".mkv", ".mxf", ".mpg", ".wmv", ".mp4", ".m4v", ".webm", ".flv", ".ts"]);

  function isVideoFile(entry: FileEntry | null): boolean {
    if (!entry || entry.isDir) return false;
    const ext = entry.name.lastIndexOf(".") >= 0
      ? entry.name.slice(entry.name.lastIndexOf(".")).toLowerCase()
      : "";
    return VIDEO_EXTENSIONS.has(ext);
  }

  function hasVideoInSelection(): boolean {
    const ctx = ctxMenu();
    const entry = ctx?.entry;
    if (entry && !entry.isDir) return isVideoFile(entry);
    // Check if any selected items are video files
    const sel = store().selection;
    if (sel.size === 0) return false;
    return store().entries.some(
      (e) => sel.has(e.path) && isVideoFile(e)
    );
  }

  function doTranscode(entry: FileEntry | null) {
    const paths = getContextPaths(entry).filter((p) => {
      const ext = p.lastIndexOf(".") >= 0 ? p.slice(p.lastIndexOf(".")).toLowerCase() : "";
      return VIDEO_EXTENSIONS.has(ext);
    });
    if (paths.length > 0) {
      transcodeStore.addJobs(paths);
      workspaceStore.openTranscodeQueue();
    }
    closeCtxMenu();
  }

  function doReveal(entry: FileEntry | null) {
    const path = entry?.path ?? store().currentPath();
    revealInFileManager(path).catch((e) => console.error("Reveal failed:", e));
    closeCtxMenu();
  }

  function doOpenInOther(entry: FileEntry | null) {
    const path = entry?.isDir ? entry.path : store().currentPath();
    props.callbacks?.onOpenInOtherBrowser?.(path);
    closeCtxMenu();
  }

  function doOpenInNewTab(entry: FileEntry | null) {
    const path = entry?.isDir ? entry.path : store().currentPath();
    props.callbacks?.onOpenInNewTab?.(path);
    closeCtxMenu();
  }

  // ── New folder helpers ──

  function openNewFolderModal() {
    setNewFolderName("New Folder");
    setNewFolderMode("folder");
    closeCtxMenu();
  }

  function openNewUFBFolderModal() {
    setNewFolderName("");
    setNewFolderMode("ufb");
    closeCtxMenu();
  }

  async function submitNewFolder() {
    const mode = newFolderMode();
    const name = newFolderName().trim();
    if (!name) { setNewFolderMode(null); return; }
    const sep = store().currentPath().includes("/") ? "/" : "\\";
    let folderName = name;
    if (mode === "ufb") {
      folderName = makeUFBFolderName(name);
    }
    const fullPath = store().currentPath() + sep + folderName;
    try {
      await createDirectory(fullPath);
      store().refresh();
    } catch (e) {
      console.error("Create folder failed:", e);
    }
    setNewFolderMode(null);
  }

  function getDatePrefix(): string {
    const now = new Date();
    const yy = String(now.getFullYear()).slice(2);
    const mm = String(now.getMonth() + 1).padStart(2, "0");
    const dd = String(now.getDate()).padStart(2, "0");
    return `${yy}${mm}${dd}`;
  }

  /** Find the next available letter suffix (a-z) for a given prefix among existing dirs */
  function nextLetterSuffix(prefix: string): string {
    const existingNames = store().entries
      .filter((e) => e.isDir)
      .map((e) => e.name.toLowerCase());
    for (let c = 97; c <= 122; c++) {
      const letter = String.fromCharCode(c);
      const probe = `${prefix}${letter}`.toLowerCase();
      // Check if any existing folder starts with this prefix+letter
      if (!existingNames.some((n) => n.startsWith(probe))) {
        return letter;
      }
    }
    return "z"; // fallback
  }

  async function createDateFolder() {
    const prefix = getDatePrefix();
    const sep = store().currentPath().includes("/") ? "/" : "\\";
    const existingNames = store().entries
      .filter((e) => e.isDir)
      .map((e) => e.name.toLowerCase());
    let name: string;
    // If bare date folder doesn't exist, use it; otherwise increment with letter suffix
    if (!existingNames.includes(prefix.toLowerCase())) {
      name = prefix;
    } else {
      const letter = nextLetterSuffix(prefix);
      name = `${prefix}${letter}`;
    }
    try {
      await createDirectory(store().currentPath() + sep + name);
      store().refresh();
    } catch (e) {
      console.error("Create date folder failed:", e);
    }
  }

  async function createTimeFolder() {
    const now = new Date();
    const hh = String(now.getHours()).padStart(2, "0");
    const min = String(now.getMinutes()).padStart(2, "0");
    const name = `${hh}${min}`;
    const sep = store().currentPath().includes("/") ? "/" : "\\";
    try {
      await createDirectory(store().currentPath() + sep + name);
      store().refresh();
    } catch (e) {
      console.error("Create time folder failed:", e);
    }
  }

  function openNewJobModal() {
    setJobNumber("");
    setJobName("");
    setShowNewJobModal(true);
  }

  async function submitNewJob() {
    const num = jobNumber().trim();
    const name = jobName().trim();
    if (!num || !name) { setShowNewJobModal(false); return; }
    try {
      await createJobFromTemplate(store().currentPath(), num, name);
      store().refresh();
      await subscriptionStore.loadSubscriptions();
    } catch (e) {
      console.error("Create job failed:", e);
    }
    setShowNewJobModal(false);
  }

  function makeUFBFolderName(label: string): string {
    const prefix = getDatePrefix();
    const letter = nextLetterSuffix(prefix);
    return `${prefix}${letter}_${label}`;
  }

  // ── Context menu handlers from child views ──

  function onItemContextMenu(e: MouseEvent, entry: FileEntry) {
    e.preventDefault();
    e.stopPropagation();
    // Select the item if not already selected
    if (!store().selection.has(entry.path)) {
      store().selectItem(entry.path);
    }
    setCtxMenu({ x: e.clientX, y: e.clientY, entry });
  }

  function onBackgroundContextMenu(e: MouseEvent) {
    e.preventDefault();
    setCtxMenu({ x: e.clientX, y: e.clientY, entry: null });
  }

  function onItemDoubleClick(entry: FileEntry) {
    openEntry(entry);
  }

  // ── Keyboard shortcuts ──

  function onKeyDown(e: KeyboardEvent) {
    // Don't intercept when typing in inputs
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;

    if (e.ctrlKey && e.key === "a") {
      e.preventDefault();
      store().selectAll();
    } else if (e.ctrlKey && e.key === "c") {
      e.preventDefault();
      const paths = getSelectedPaths();
      if (paths.length > 0) {
        setCutPaths([]);
        clipboardCopyPaths(paths);
      }
    } else if (e.ctrlKey && e.key === "x") {
      e.preventDefault();
      const paths = getSelectedPaths();
      if (paths.length > 0) {
        setCutPaths(paths);
        clipboardCopyPaths(paths);
      }
    } else if (e.ctrlKey && e.key === "v") {
      e.preventDefault();
      doPaste();
    } else if (e.key === "Delete") {
      e.preventDefault();
      const paths = getSelectedPaths();
      if (paths.length > 0) {
        deleteToTrash(paths).then(() => store().refresh());
      }
    } else if (e.key === "F2") {
      e.preventDefault();
      const sel = store().selection;
      if (sel.size === 1) {
        const path = [...sel][0];
        const entry = store().entries.find((en) => en.path === path);
        if (entry) startRename(entry);
      }
    } else if (e.ctrlKey && e.shiftKey && e.key === "N") {
      e.preventDefault();
      openNewFolderModal();
    }
  }

  /** Called by parent (DualBrowserView) when external files are dropped onto this browser */
  function handleExternalDrop(paths: string[]) {
    if (paths.length === 0) return;
    copyFiles(paths, store().currentPath())
      .then(() => store().refresh())
      .catch((err) => console.error("External drop failed:", err));
  }

  // Expose for parent to call
  props.onExternalDrop?.(handleExternalDrop);

  onMount(() => {
    containerRef?.addEventListener("keydown", onKeyDown);
  });
  onCleanup(() => {
    containerRef?.removeEventListener("keydown", onKeyDown);
  });

  const ctx = () => ctxMenu();
  const ctxEntry = () => ctx()?.entry ?? null;

  return (
    <div
      class="file-browser"
      ref={containerRef}
      tabIndex={0}
      onClick={closeCtxMenu}
      onMouseDown={() => containerRef?.focus()}
      data-browser-id={store().id}
    >
      <NavigationBar
        store={props.store}
        onNewFolder={openNewFolderModal}
        onNewUFBFolder={openNewUFBFolderModal}
        onNewDateFolder={createDateFolder}
        onNewTimeFolder={createTimeFolder}
        onNewJob={openNewJobModal}
        isProjectFolder={isProjectFolder()}
      />
      <div
        class="file-browser-content"
        ref={contentRef}
        onContextMenu={onBackgroundContextMenu}
        onMouseDown={rubberBand.onMouseDown}
      >
        <Show
          when={props.store.viewMode() === "list"}
          fallback={
            <FileGridView
              store={props.store}
              isProjectFolder={isProjectFolder()}
              isSubscribed={isSubscribed}
              onItemContextMenu={onItemContextMenu}
              onItemDoubleClick={onItemDoubleClick}
            />
          }
        >
          <FileListView
            store={props.store}
            isProjectFolder={isProjectFolder()}
            isSubscribed={isSubscribed}
            onItemContextMenu={onItemContextMenu}
            onItemDoubleClick={onItemDoubleClick}
          />
        </Show>
        <Show when={rubberBand.rect()}>
          {(r) => (
            <div
              class="rubber-band"
              style={{
                left: `${r().left}px`,
                top: `${r().top}px`,
                width: `${r().width}px`,
                height: `${r().height}px`,
              }}
            />
          )}
        </Show>
      </div>
      <div class="file-browser-statusbar">
        <span>{props.store.entries.length} items</span>
        <Show when={props.store.selection.size > 0}>
          <span>{props.store.selection.size} selected</span>
        </Show>
      </div>

      {/* ── Context Menu ── */}
      <Show when={ctx()}>
        {(menu) => (
          <div
            class="browser-ctx-menu"
            style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
            ref={adjustMenuPosition}
            onClick={(e) => e.stopPropagation()}
          >
            <Show when={menu().entry !== null}>
              {/* File/folder context menu */}
              <div class="ctx-item" onClick={() => openEntry(menu().entry!)}>
                {menu().entry!.isDir ? "Open" : "Open File"}
              </div>
              <Show when={isProjectFolder() && menu().entry!.isDir}>
                <div
                  class={`ctx-item ${isSubscribed(menu().entry!.path) ? "ctx-subscribed" : "ctx-accent"}`}
                  onClick={() => {
                    if (!isSubscribed(menu().entry!.path)) syncAsJob(menu().entry!);
                    else closeCtxMenu();
                  }}
                >
                  {isSubscribed(menu().entry!.path) ? "\u2713 Synced" : "Sync as Job"}
                </div>
              </Show>
              <Show when={props.callbacks?.onOpenInOtherBrowser}>
                <div class="ctx-item" onClick={() => doOpenInOther(ctxEntry())}>
                  Open in Other Browser
                </div>
              </Show>
              <Show when={props.callbacks?.onOpenInNewTab}>
                <div class="ctx-item" onClick={() => doOpenInNewTab(ctxEntry())}>
                  Open in New Tab
                </div>
              </Show>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => doCopy(ctxEntry())}>Copy</div>
              <div class="ctx-item" onClick={() => doCut(ctxEntry())}>Cut</div>
              <div class="ctx-item" onClick={() => doPaste()}>Paste</div>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => doCopyPath(ctxEntry())}>Copy Full Path</div>
              <div class="ctx-item" onClick={() => doCopyFilename(ctxEntry())}>Copy Filename</div>
              <div class="ctx-item" onClick={() => doCopyUfbLink(ctxEntry())}>Copy ufb:/// Link</div>
              <div class="ctx-item" onClick={() => doCopyUnionLink(ctxEntry())}>Copy union:/// Link</div>
              <div class="ctx-item" onClick={() => doReveal(ctxEntry())}>Reveal in Explorer</div>
              <Show when={hasVideoInSelection()}>
                <div class="ctx-sep" />
                <div class="ctx-item" onClick={() => doTranscode(ctxEntry())}>Transcode to MP4</div>
              </Show>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => startRename(ctxEntry())}>Rename</div>
              <div class="ctx-item ctx-danger" onClick={() => doDelete(ctxEntry())}>Delete</div>
            </Show>
            <Show when={menu().entry === null}>
              {/* Background context menu */}
              <div class="ctx-item" onClick={openNewFolderModal}>New Folder</div>
              <div class="ctx-item" onClick={openNewUFBFolderModal}>New UFB Folder</div>
              <div class="ctx-item" onClick={createDateFolder}>New Date Folder</div>
              <div class="ctx-item" onClick={createTimeFolder}>New Time Folder</div>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => doPaste()}>Paste</div>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => doCopyPath(null)}>Copy File Path</div>
              <div class="ctx-item" onClick={() => doCopyUfbLink(null)}>Copy ufb:/// Link</div>
              <div class="ctx-item" onClick={() => doCopyUnionLink(null)}>Copy union:/// Link</div>
              <div class="ctx-sep" />
              <div class="ctx-item" onClick={() => store().refresh()}>Refresh</div>
            </Show>
          </div>
        )}
      </Show>

      {/* ── Rename Modal ── */}
      <Show when={renameTarget()}>
        {(target) => (
          <div class="modal-overlay">
            <div class="browser-modal">
              <div class="browser-modal-title">Rename</div>
              <div class="browser-modal-body">
                <input
                  class="browser-modal-input"
                  type="text"
                  value={renameName()}
                  onInput={(e) => setRenameName(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") submitRename();
                    if (e.key === "Escape") setRenameTarget(null);
                  }}
                  ref={(el) => requestAnimationFrame(() => {
                    el.focus();
                    // Select name without extension for files
                    const name = target().name;
                    const dotIdx = target().isDir ? -1 : name.lastIndexOf(".");
                    el.setSelectionRange(0, dotIdx > 0 ? dotIdx : name.length);
                  })}
                />
              </div>
              <div class="browser-modal-actions">
                <button class="modal-btn" onClick={() => setRenameTarget(null)}>Cancel</button>
                <button
                  class="modal-btn modal-btn-primary"
                  onClick={submitRename}
                  disabled={!renameName().trim() || renameName() === target().name}
                >
                  Rename
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>

      {/* ── New Folder Modal ── */}
      <Show when={newFolderMode()}>
        {(mode) => (
          <div class="modal-overlay">
            <div class="browser-modal">
              <div class="browser-modal-title">
                {mode() === "ufb" ? "New UFB Folder" : "New Folder"}
              </div>
              <div class="browser-modal-body">
                <Show when={mode() === "ufb"}>
                  <span class="browser-modal-hint">
                    Creates: {makeUFBFolderName(newFolderName() || "name")}
                  </span>
                </Show>
                <input
                  class="browser-modal-input"
                  type="text"
                  value={newFolderName()}
                  onInput={(e) => setNewFolderName(e.currentTarget.value)}
                  placeholder={mode() === "ufb" ? "Folder label..." : "Folder name..."}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") submitNewFolder();
                    if (e.key === "Escape") setNewFolderMode(null);
                  }}
                  ref={(el) => requestAnimationFrame(() => {
                    el.focus();
                    el.select();
                  })}
                />
              </div>
              <div class="browser-modal-actions">
                <button class="modal-btn" onClick={() => setNewFolderMode(null)}>Cancel</button>
                <button
                  class="modal-btn modal-btn-primary"
                  onClick={submitNewFolder}
                  disabled={!newFolderName().trim()}
                >
                  Create
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>

      {/* ── New Job Modal ── */}
      <Show when={showNewJobModal()}>
        <div class="modal-overlay">
          <div class="browser-modal">
            <div class="browser-modal-title">New Job</div>
            <div class="browser-modal-body">
              <span class="browser-modal-hint">
                Creates: {jobNumber().trim() || "000"}_{jobName().trim() || "JobName"}
              </span>
              <input
                class="browser-modal-input"
                type="text"
                value={jobNumber()}
                onInput={(e) => setJobNumber(e.currentTarget.value)}
                placeholder="Job number..."
                onKeyDown={(e) => {
                  if (e.key === "Escape") setShowNewJobModal(false);
                }}
                ref={(el) => requestAnimationFrame(() => el.focus())}
              />
              <input
                class="browser-modal-input"
                type="text"
                value={jobName()}
                onInput={(e) => setJobName(e.currentTarget.value)}
                placeholder="Job name..."
                onKeyDown={(e) => {
                  if (e.key === "Enter") submitNewJob();
                  if (e.key === "Escape") setShowNewJobModal(false);
                }}
              />
            </div>
            <div class="browser-modal-actions">
              <button class="modal-btn" onClick={() => setShowNewJobModal(false)}>Cancel</button>
              <button
                class="modal-btn modal-btn-primary"
                onClick={submitNewJob}
                disabled={!jobNumber().trim() || !jobName().trim()}
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
