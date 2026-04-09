import { createSignal, createMemo, onMount, For, Show } from "solid-js";
import { subscriptionStore } from "../../stores/subscriptionStore";
import { workspaceStore } from "../../stores/workspaceStore";
import { mountStore } from "../../stores/mountStore";
import { platformStore } from "../../stores/platformStore";
import { buildUfbUri, buildUnionUri, revealInFileManager, showShellContextMenu, pickFolder, getSpecialPaths, getDrives } from "../../lib/tauri";
import type { Subscription } from "../../lib/types";
import { adjustMenuPosition } from "../../lib/contextMenuPosition";
import "./SubscriptionPanel.css";

interface SubscriptionPanelProps {
  onNavigate?: (path: string) => void;
  /** Navigate in the second browser panel */
  onNavigateRight?: (path: string) => void;
}

export function SubscriptionPanel(props: SubscriptionPanelProps) {
  // ── Add Bookmark Modal ──
  const [showAddModal, setShowAddModal] = createSignal(false);
  const [addPath, setAddPath] = createSignal("");
  const [addName, setAddName] = createSignal("");
  const [addIsProject, setAddIsProject] = createSignal(false);

  // ── Bookmark Context Menu ──
  const [ctxMenu, setCtxMenu] = createSignal<{
    x: number;
    y: number;
    bookmark: typeof subscriptionStore.bookmarks[0];
  } | null>(null);

  // ── Subscription Context Menu ──
  const [subCtxMenu, setSubCtxMenu] = createSignal<{
    x: number;
    y: number;
    sub: Subscription;
  } | null>(null);

  const [mountCtxMenu, setMountCtxMenu] = createSignal<{
    x: number;
    y: number;
    mountId: string;
  } | null>(null);

  // ── Confirm Delete (bookmarks) ──
  const [confirmDelete, setConfirmDelete] = createSignal<typeof subscriptionStore.bookmarks[0] | null>(null);

  // ── Confirm Unsubscribe ──
  const [confirmUnsub, setConfirmUnsub] = createSignal<Subscription | null>(null);

  // ── System locations (auto-populated) ──
  const [userFolders, setUserFolders] = createSignal<{ key: string; path: string; label: string; icon: string }[]>([]);
  const [systemDrives, setSystemDrives] = createSignal<{ path: string; label: string }[]>([]);

  onMount(async () => {
    try {
      const [special, drives] = await Promise.all([getSpecialPaths(), getDrives()]);
      const folders: { key: string; path: string; label: string; icon: string }[] = [];
      if (special.desktop) folders.push({ key: "desktop", path: special.desktop, label: "Desktop", icon: "desktop_windows" });
      if (special.documents) folders.push({ key: "documents", path: special.documents, label: "Documents", icon: "description" });
      if (special.downloads) folders.push({ key: "downloads", path: special.downloads, label: "Downloads", icon: "download" });
      setUserFolders(folders);
      setSystemDrives(drives.map(([path, label]) => ({ path, label })));
    } catch (e) { console.error("Failed to load system locations:", e); }
  });

  // Custom bookmarks only (no drive-letter bookmarks — drives are auto-detected now)
  const customBookmarks = createMemo(() =>
    subscriptionStore.bookmarks.filter((b) => !/^[A-Z]:\\?$/i.test(b.path))
  );

  function navigate(path: string) {
    props.onNavigate?.(path);
    // Switch to the main browser tab so the user sees it
    workspaceStore.setActiveTabId("main");
  }

  function navigateRight(path: string) {
    (props.onNavigateRight ?? props.onNavigate)?.(path);
    workspaceStore.setActiveTabId("main");
  }

  function openAddModal() {
    setAddPath("");
    setAddName("");
    setAddIsProject(false);
    setShowAddModal(true);
  }

  async function submitAddBookmark() {
    const path = addPath().trim();
    const name = addName().trim();
    if (!path || !name) return;
    await subscriptionStore.addBookmark(path, name, addIsProject());
    setShowAddModal(false);
  }

  // ── Bookmark context menu handlers ──

  function onContextMenu(e: MouseEvent, bookmark: typeof subscriptionStore.bookmarks[0]) {
    e.preventDefault();
    e.stopPropagation();
    setSubCtxMenu(null);
    setCtxMenu({ x: e.clientX, y: e.clientY, bookmark });
  }

  function closeCtxMenu() {
    setCtxMenu(null);
  }

  function ctxOpenLeft() {
    const menu = ctxMenu();
    if (menu) navigate(menu.bookmark.path);
    closeCtxMenu();
  }

  function ctxOpenRight() {
    const menu = ctxMenu();
    if (menu) navigateRight(menu.bookmark.path);
    closeCtxMenu();
  }

  function ctxDelete() {
    const menu = ctxMenu();
    if (menu) setConfirmDelete(menu.bookmark);
    closeCtxMenu();
  }

  async function doDelete() {
    const bm = confirmDelete();
    if (bm) {
      await subscriptionStore.removeBookmark(bm.path);
    }
    setConfirmDelete(null);
  }

  // ── Subscription context menu handlers ──

  function onSubContextMenu(e: MouseEvent, sub: Subscription) {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu(null);
    setSubCtxMenu({ x: e.clientX, y: e.clientY, sub });
  }

  function closeSubCtxMenu() {
    setSubCtxMenu(null);
  }

  function subCtxOpenJobTab() {
    const menu = subCtxMenu();
    if (menu) workspaceStore.openJobTab(menu.sub.jobPath, menu.sub.jobName);
    closeSubCtxMenu();
  }

  function subCtxOpenLeft() {
    const menu = subCtxMenu();
    if (menu) navigate(menu.sub.jobPath);
    closeSubCtxMenu();
  }

  function subCtxOpenRight() {
    const menu = subCtxMenu();
    if (menu) navigateRight(menu.sub.jobPath);
    closeSubCtxMenu();
  }

  async function subCtxCopyPath() {
    const menu = subCtxMenu();
    if (menu) {
      await navigator.clipboard.writeText(menu.sub.jobPath);
    }
    closeSubCtxMenu();
  }

  async function subCtxCopyUfbLink() {
    const menu = subCtxMenu();
    if (menu) {
      const uri = await buildUfbUri(menu.sub.jobPath);
      await navigator.clipboard.writeText(uri);
    }
    closeSubCtxMenu();
  }

  async function subCtxCopyUnionLink() {
    const menu = subCtxMenu();
    if (menu) {
      const uri = await buildUnionUri(menu.sub.jobPath);
      await navigator.clipboard.writeText(uri);
    }
    closeSubCtxMenu();
  }

  async function subCtxReveal() {
    const menu = subCtxMenu();
    if (menu) {
      await revealInFileManager(menu.sub.jobPath);
    }
    closeSubCtxMenu();
  }

  function subCtxUnsubscribe() {
    const menu = subCtxMenu();
    if (menu) setConfirmUnsub(menu.sub);
    closeSubCtxMenu();
  }

  async function doUnsubscribe() {
    const sub = confirmUnsub();
    if (sub) {
      await subscriptionStore.unsubscribe(sub.jobPath);
    }
    setConfirmUnsub(null);
  }

  // ── Mount context menu handlers ──

  function onMountContextMenu(e: MouseEvent, mountId: string) {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu(null);
    setSubCtxMenu(null);
    setMountCtxMenu({ x: e.clientX, y: e.clientY, mountId });
  }

  function closeMountCtxMenu() {
    setMountCtxMenu(null);
  }

  function mountCtxRestart() {
    const menu = mountCtxMenu();
    if (menu) mountStore.restart(menu.mountId);
    closeMountCtxMenu();
  }


  // Close any open context menu when clicking anywhere
  function onPanelClick() {
    if (ctxMenu()) closeCtxMenu();
    if (subCtxMenu()) closeSubCtxMenu();
    if (mountCtxMenu()) closeMountCtxMenu();
  }

  return (
    <div class="subscription-panel" onClick={onPanelClick}>
      {/* ── Quick Access (user folders) ── */}
      <Show when={userFolders().length > 0}>
        <div class="panel-section">
          <div class="section-header">Quick Access</div>
          <div class="section-content">
            <For each={userFolders()}>
              {(folder) => (
                <div
                  class="panel-item"
                  onClick={() => navigate(folder.path)}
                  onMouseDown={(e) => { if (e.button === 1) { e.preventDefault(); navigateRight(folder.path); } }}
                  title={folder.path}
                >
                  <span class="item-icon"><span class="icon">{folder.icon}</span></span>
                  <span class="item-label truncate">{folder.label}</span>
                </div>
              )}
            </For>
          </div>
        </div>
      </Show>

      {/* ── Bookmarks ── */}
      <div class="panel-section">
        <div class="section-header">
          <span>Bookmarks</span>
          <button class="section-add-btn" onClick={openAddModal} title="Add bookmark"><span class="icon">add</span></button>
        </div>
        <div class="section-content">
          {/* Mount junction paths as dynamic bookmarks with live state */}
          <Show when={mountStore.configs.length > 0 && !mountStore.connected}>
            <For each={mountStore.configs}>
              {(cfg) => (
                <div
                  class="panel-item mount-item-disconnected"
                  onClick={() => mountStore.launchAgent()}
                  title="Agent not running — click to launch"
                >
                  <span class="item-icon"><span class="icon">cloud_off</span></span>
                  <span class="item-label truncate">{cfg.displayName}</span>
                  <span class="item-tag">Launch</span>
                </div>
              )}
            </For>
          </Show>
          <Show when={mountStore.connected}>
            <For each={mountStore.configs}>
              {(cfg) => {
                const ms = () => mountStore.states[cfg.id];
                const isSync = () => cfg.syncEnabled;
                const stateClass = () => {
                  if (isSync()) {
                    const ss = ms()?.syncState;
                    if (ss === "active") return "mount-healthy";
                    if (ss === "registering") return "mount-starting";
                    if (ss === "error") return "mount-error";
                    if (ss === "deregistering" || ss === "disabled") return "mount-warn";
                  }
                  const s = ms()?.state;
                  if (s === "mounted") return "mount-healthy";
                  if (s === "mounting" || s === "initializing") return "mount-starting";
                  if (s === "error") return "mount-error";
                  if (!s || s === "stopped") return "mount-warn";
                  return "mount-error";
                };
                const stateLabel = () => {
                  if (isSync()) {
                    const ss = ms()?.syncState;
                    if (ss === "active") {
                      const detail = ms()?.syncStateDetail;
                      if (detail && detail !== "Active") return detail;
                      return "Sync";
                    }
                    if (ss === "registering") return "Starting";
                    if (ss === "error") return "Error";
                    if (ss === "deregistering") return "Stopping";
                    if (ss === "disabled") return "Disabled";
                  }
                  const s = ms()?.state;
                  if (s === "mounted") return "Mounted";
                  if (s === "mounting" || s === "initializing") return "Starting";
                  if (s === "error") return "Error";
                  if (s === "stopped") return "Stopped";
                  if (!s) return "Unknown";
                  return s;
                };
                const isActive = () => {
                  const s = ms()?.state;
                  return s === "mounted" || s === "mounting" || s === "initializing";
                };
                return (
                  <div
                    class="panel-item"
                    onClick={() => navigate(mountStore.getMountPath(cfg))}
                    onMouseDown={(e) => { if (e.button === 1) { e.preventDefault(); navigateRight(mountStore.getMountPath(cfg)); } }}
                    onContextMenu={(e) => onMountContextMenu(e, cfg.id)}
                    title={ms()?.syncStateDetail ?? ms()?.stateDetail ?? mountStore.getMountPath(cfg)}
                  >
                    <span class={`mount-status-dot ${stateClass()}`} />
                    <span class="item-label truncate">{cfg.displayName}</span>
                    <Show when={isSync()}><span class="item-tag">Sync</span></Show>
                    <Show when={cfg.isJobsFolder && !isSync()}><span class="item-tag">Jobs</span></Show>
                    <span class={`mount-state-label ${stateClass()}`}>{stateLabel()}</span>
                    <span
                      class="mount-toggle-btn"
                      onClick={(e) => { e.stopPropagation(); mountStore.toggleMount(cfg.id); }}
                      title={isActive() ? "Disconnect" : "Connect"}
                    >
                      <span class="icon">{isActive() ? "stop_circle" : "play_circle"}</span>
                    </span>
                  </div>
                );
              }}
            </For>
          </Show>
          <For each={customBookmarks()}>
            {(bookmark) => (
              <div
                class="panel-item"
                onClick={() => navigate(bookmark.path)}
                onMouseDown={(e) => { if (e.button === 1) { e.preventDefault(); navigateRight(bookmark.path); } }}
                onContextMenu={(e) => onContextMenu(e, bookmark)}
                title={bookmark.path}
              >
                <span class="item-icon">
                  <span class="icon">{bookmark.isProjectFolder ? "movie" : "folder"}</span>
                </span>
                <span class="item-label truncate">{bookmark.displayName}</span>
                <Show when={bookmark.isProjectFolder}>
                  <span class="item-tag">Jobs</span>
                </Show>
              </div>
            )}
          </For>
          <Show when={customBookmarks().length === 0 && mountStore.configs.length === 0}>
            <div class="empty-message">No bookmarks</div>
          </Show>
        </div>
      </div>

      {/* ── Drives ── */}
      <Show when={systemDrives().length > 0}>
        <div class="panel-section">
          <div class="section-header">Drives</div>
          <div class="section-content">
            <For each={systemDrives()}>
              {(drive) => (
                <div
                  class="panel-item"
                  onClick={() => navigate(drive.path)}
                  onMouseDown={(e) => { if (e.button === 1) { e.preventDefault(); navigateRight(drive.path); } }}
                  title={drive.path}
                >
                  <span class="item-icon"><span class="icon">hard_drive</span></span>
                  <span class="item-label truncate">{drive.label}</span>
                </div>
              )}
            </For>
          </div>
        </div>
      </Show>

      {/* ── Subscriptions ── */}
      <div class="panel-section">
        <div class="section-header">Subscriptions</div>
        <div class="section-content">
          <For each={subscriptionStore.subscriptions}>
            {(sub) => {
              const mount = () => mountStore.getMountForPath(sub.jobPath);
              const mountIssue = () => {
                const m = mount();
                if (!m) return null;
                if (m.state === "mounted" || m.state === "initializing" || m.state === "mounting") return null;
                return m;
              };
              // On non-Windows, a drive-letter path means no active mapping could translate it
              const isUnresolved = () => {
                if (platformStore.platform === "win") return false;
                return /^[A-Za-z]:[/\\]/.test(sub.jobPath);
              };
              return (
                <div
                  class={`panel-item ${isUnresolved() ? "panel-item-disabled" : ""}`}
                  onClick={() => { if (!isUnresolved()) workspaceStore.openJobTab(sub.jobPath, sub.jobName); }}
                  onMouseDown={(e) => { if (e.button === 1 && !isUnresolved()) { e.preventDefault(); navigateRight(sub.jobPath); } }}
                  onContextMenu={(e) => onSubContextMenu(e, sub)}
                  title={isUnresolved()
                    ? `Path not mapped: ${sub.jobPath}\nEnable a mapping in Settings > Paths`
                    : mountIssue()
                      ? `${sub.jobPath}\nMount: ${mountIssue()!.stateDetail}`
                      : sub.jobPath}
                >
                  <span class={`sync-indicator sync-${sub.syncStatus.toLowerCase()}`} />
                  <span class="item-label truncate">{sub.jobName}</span>
                  <Show when={isUnresolved()}>
                    <span class="mount-badge mount-badge-warn" title="Path mapping unavailable">
                      <span class="icon">link_off</span>
                    </span>
                  </Show>
                  <Show when={!isUnresolved() && mountIssue()}>
                    <span
                      class={`mount-badge mount-badge-${mountIssue()!.state === "error" ? "error" : "warn"}`}
                      title={mountIssue()!.stateDetail}
                    >
                      <span class="icon">{mountIssue()!.state === "error" ? "error" : "warning"}</span>
                    </span>
                  </Show>
                  <span class="item-badge">{sub.shotCount}</span>
                </div>
              );
            }}
          </For>
          <Show when={subscriptionStore.subscriptions.length === 0}>
            <div class="empty-message">No subscriptions</div>
          </Show>
        </div>
      </div>

      {/* ── Bookmark Context Menu ── */}
      <Show when={ctxMenu()}>
        {(menu) => (
          <div
            class="ctx-menu"
            style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
            ref={adjustMenuPosition}
          >
            <div class="ctx-menu-header truncate">{menu().bookmark.displayName}</div>
            <div class="ctx-menu-item" onClick={ctxOpenLeft}><span class="icon">arrow_back</span> Open in Left Browser</div>
            <div class="ctx-menu-item" onClick={ctxOpenRight}><span class="icon">arrow_forward</span> Open in Right Browser</div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={async () => { const m = ctxMenu(); if (m) await showShellContextMenu(m.bookmark.path); closeCtxMenu(); }}><span class="icon">more_horiz</span> More...</div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item ctx-menu-danger" onClick={ctxDelete}><span class="icon">delete</span> Delete</div>
          </div>
        )}
      </Show>

      {/* ── Subscription Context Menu ── */}
      <Show when={subCtxMenu()}>
        {(menu) => (
          <div
            class="ctx-menu"
            style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
            ref={adjustMenuPosition}
          >
            <div class="ctx-menu-header truncate">{menu().sub.jobName}</div>
            <div class="ctx-menu-item" onClick={subCtxOpenJobTab}><span class="icon">movie</span> Open Job Tab</div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={subCtxOpenLeft}><span class="icon">arrow_back</span> Open in Left Browser</div>
            <div class="ctx-menu-item" onClick={subCtxOpenRight}><span class="icon">arrow_forward</span> Open in Right Browser</div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={subCtxCopyPath}><span class="icon">content_copy</span> Copy Path</div>
            <div class="ctx-menu-item" onClick={subCtxCopyUfbLink}><span class="icon">link</span> Copy ufb:/// Link</div>
            <div class="ctx-menu-item" onClick={subCtxCopyUnionLink}><span class="icon">link</span> Copy union:/// Link</div>
            <div class="ctx-menu-item" onClick={subCtxReveal}><span class="icon">folder_open</span> Reveal in Explorer</div>
            <div class="ctx-menu-item" onClick={async () => { const m = subCtxMenu(); if (m) await showShellContextMenu(m.sub.jobPath); closeSubCtxMenu(); }}><span class="icon">more_horiz</span> More...</div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item ctx-menu-danger" onClick={subCtxUnsubscribe}><span class="icon">remove_circle_outline</span> Unsubscribe</div>
          </div>
        )}
      </Show>

      {/* ── Mount Context Menu ── */}
      <Show when={mountCtxMenu()}>
        {(menu) => {
          const ms = () => mountStore.states[menu().mountId];
          const cfg = () => mountStore.configs.find((c) => c.id === menu().mountId);
          const mountPath = () => cfg() ? mountStore.getMountPath(cfg()!) : "";
          const isActive = () => {
            const s = ms()?.state;
            return s === "mounted" || s === "mounting" || s === "initializing";
          };
          return (
            <div
              class="ctx-menu"
              style={{ left: `${menu().x}px`, top: `${menu().y}px` }}
              ref={adjustMenuPosition}
            >
              <div class="ctx-menu-header truncate">{cfg()?.displayName ?? menu().mountId}</div>
              <Show when={ms()?.stateDetail}>
                <div class="ctx-menu-item ctx-menu-disabled"><span class="icon">info</span> {ms()!.stateDetail}</div>
              </Show>
              <div class="ctx-menu-separator" />
              <Show when={isActive()}>
                <div class="ctx-menu-item" onClick={() => { mountStore.stop(menu().mountId); closeMountCtxMenu(); }}><span class="icon">stop_circle</span> Disconnect</div>
              </Show>
              <Show when={!isActive()}>
                <div class="ctx-menu-item" onClick={() => { mountStore.start(menu().mountId); closeMountCtxMenu(); }}><span class="icon">play_circle</span> Connect</div>
              </Show>
              <div class="ctx-menu-item" onClick={mountCtxRestart}><span class="icon">refresh</span> Restart</div>
              <div class="ctx-menu-separator" />
              <div class="ctx-menu-item" onClick={() => { navigate(mountPath()); closeMountCtxMenu(); }}><span class="icon">arrow_back</span> Open in Left Browser</div>
              <div class="ctx-menu-item" onClick={() => { navigateRight(mountPath()); closeMountCtxMenu(); }}><span class="icon">arrow_forward</span> Open in Right Browser</div>
              <div class="ctx-menu-item" onClick={async () => { await revealInFileManager(mountPath()); closeMountCtxMenu(); }}><span class="icon">folder_open</span> Reveal in Explorer</div>
              <div class="ctx-menu-item" onClick={async () => { await showShellContextMenu(mountPath()); closeMountCtxMenu(); }}><span class="icon">more_horiz</span> More...</div>
            </div>
          );
        }}
      </Show>

      {/* ── Add Bookmark Modal ── */}
      <Show when={showAddModal()}>
        <div class="modal-overlay">
          <div class="modal">
            <div class="modal-title">Add Bookmark</div>
            <div class="modal-body">
              <div class="modal-field">
                <span class="modal-field-label">Path</span>
                <div class="modal-field-row-input">
                  <input
                    type="text"
                    class="modal-input"
                    value={addPath()}
                    onInput={(e) => setAddPath(e.currentTarget.value)}
                    placeholder="C:\Projects\MyStudio"
                    ref={(el) => requestAnimationFrame(() => el.focus())}
                  />
                  <button class="modal-btn" onClick={async () => {
                    try {
                      const selected = await pickFolder("Select Bookmark Folder");
                      if (selected) {
                        setAddPath(selected);
                        if (!addName().trim()) {
                          const name = selected.split(/[\\/]/).filter(Boolean).pop() ?? "";
                          setAddName(name);
                        }
                      }
                    } catch (e) { console.error("Browse failed:", e); }
                  }}>Browse...</button>
                </div>
              </div>
              <label class="modal-field">
                <span class="modal-field-label">Name</span>
                <input
                  type="text"
                  class="modal-input"
                  value={addName()}
                  onInput={(e) => setAddName(e.currentTarget.value)}
                  placeholder="My Studio Projects"
                  onKeyDown={(e) => { if (e.key === "Enter") submitAddBookmark(); }}
                />
              </label>
              <label class="modal-field modal-field-row">
                <input
                  type="checkbox"
                  class="modal-checkbox"
                  checked={addIsProject()}
                  onChange={(e) => setAddIsProject(e.currentTarget.checked)}
                />
                <span class="modal-field-label">Jobs Folder</span>
                <span class="modal-field-hint">
                  Subfolders will appear as subscribable jobs
                </span>
              </label>
            </div>
            <div class="modal-actions">
              <button class="modal-btn" onClick={() => setShowAddModal(false)}>Cancel</button>
              <button
                class="modal-btn modal-btn-primary"
                onClick={submitAddBookmark}
                disabled={!addPath().trim() || !addName().trim()}
              >
                Add
              </button>
            </div>
          </div>
        </div>
      </Show>

      {/* ── Confirm Delete Bookmark Modal ── */}
      <Show when={confirmDelete()}>
        {(bm) => (
          <div class="modal-overlay">
            <div class="modal">
              <div class="modal-title">Delete Bookmark</div>
              <div class="modal-body">
                <p>Remove <strong>{bm().displayName}</strong> from bookmarks?</p>
              </div>
              <div class="modal-actions">
                <button class="modal-btn" onClick={() => setConfirmDelete(null)}>Cancel</button>
                <button class="modal-btn modal-btn-danger" onClick={doDelete}>Delete</button>
              </div>
            </div>
          </div>
        )}
      </Show>

      {/* ── Confirm Unsubscribe Modal ── */}
      <Show when={confirmUnsub()}>
        {(sub) => (
          <div class="modal-overlay">
            <div class="modal">
              <div class="modal-title">Unsubscribe</div>
              <div class="modal-body">
                <p>Unsubscribe from <strong>{sub().jobName}</strong>?</p>
              </div>
              <div class="modal-actions">
                <button class="modal-btn" onClick={() => setConfirmUnsub(null)}>Cancel</button>
                <button class="modal-btn modal-btn-danger" onClick={doUnsubscribe}>Unsubscribe</button>
              </div>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
