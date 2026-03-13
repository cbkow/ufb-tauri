import { createSignal, For, onMount, onCleanup, Show } from "solid-js";
import { listen } from "@tauri-apps/api/event";
import {
  getAllTrackedItems,
  getTrackedItems,
  upsertItemMetadata,
  buildUfbUri,
  buildUnionUri,
  revealInFileManager,
} from "../../lib/tauri";
import { adjustMenuPosition } from "../../lib/contextMenuPosition";
import type { TrackedItemRecord, ColumnDefinition } from "../../lib/types";
import { renderCellValue, formatDate } from "../../lib/cellRenderers";
import { buildMergedColumnDefs } from "../../lib/mergeColumns";
import { inferItemType, type ItemType } from "../../lib/inferItemType";
import { makeColumnResizer } from "../../lib/useColumnResize";
import { workspaceStore } from "../../stores/workspaceStore";
import "./TrackerView.css";

interface TrackerViewProps {
  mode: "aggregated" | "job";
  jobPath?: string;
  jobName?: string;
}

type SortField = "type" | "project" | "name" | "modified" | string;

const TYPE_LABELS: Record<ItemType, string> = {
  shot: "Shot",
  asset: "Asset",
  posting: "Posting",
  other: "Other",
};

const TYPE_COLORS: Record<ItemType, string> = {
  shot: "var(--accent-color)",
  asset: "#6c9",
  posting: "#c69",
  other: "var(--text-disabled)",
};

// ── Visibility persistence ──

function visibilityKey(mode: string, jobPath?: string): string {
  return `ufb-tracker-vis:${mode}:${jobPath ?? "all"}`;
}

function loadVisibility(mode: string, jobPath?: string): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(visibilityKey(mode, jobPath));
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

function saveVisibility(mode: string, vis: Record<string, boolean>, jobPath?: string) {
  try {
    localStorage.setItem(visibilityKey(mode, jobPath), JSON.stringify(vis));
  } catch { /* ignore */ }
}

// ── Due date filter presets ──
type DueDatePreset = "all" | "overdue" | "today" | "this_week" | "this_month";

function matchesDueDatePreset(metaJson: Record<string, unknown>, preset: DueDatePreset): boolean {
  if (preset === "all") return true;
  // Look for any date-type value that looks like a due date
  const dueVal = metaJson["dueDate"] ?? metaJson["due_date"] ?? metaJson["Due Date"] ?? metaJson["Due"];
  if (dueVal == null || typeof dueVal !== "number") return false;
  const due = new Date(dueVal);
  const now = new Date();
  const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  switch (preset) {
    case "overdue": return due < startOfToday;
    case "today": return due >= startOfToday && due < new Date(startOfToday.getTime() + 86400000);
    case "this_week": {
      const endOfWeek = new Date(startOfToday);
      endOfWeek.setDate(endOfWeek.getDate() + (7 - endOfWeek.getDay()));
      return due >= startOfToday && due < endOfWeek;
    }
    case "this_month": {
      const endOfMonth = new Date(now.getFullYear(), now.getMonth() + 1, 1);
      return due >= startOfToday && due < endOfMonth;
    }
    default: return true;
  }
}

export function TrackerView(props: TrackerViewProps) {
  const [items, setItems] = createSignal<TrackedItemRecord[]>([]);
  const [mergedColumns, setMergedColumns] = createSignal<ColumnDefinition[]>([]);
  const [metadataMap, setMetadataMap] = createSignal<Record<string, Record<string, unknown>>>({});
  const [typeMap, setTypeMap] = createSignal<Record<string, ItemType>>({});
  const [sortField, setSortField] = createSignal<SortField>("name");
  const [sortDir, setSortDir] = createSignal<"asc" | "desc">("asc");
  const [editingCell, setEditingCell] = createSignal<{ itemPath: string; colName: string } | null>(null);
  const [colVisibility, setColVisibility] = createSignal<Record<string, boolean>>({});
  const [overrideWidths, setOverrideWidths] = createSignal<Record<number, number>>({});
  const [colHeaderMenu, setColHeaderMenu] = createSignal<{ x: number; y: number } | null>(null);
  const [ctxMenu, setCtxMenu] = createSignal<{ x: number; y: number; item: TrackedItemRecord } | null>(null);
  const [isLoading, setIsLoading] = createSignal(true);

  // Filters
  const [filterTypes, setFilterTypes] = createSignal<Set<ItemType>>(new Set(["shot", "asset", "posting", "other"]));
  const [filterProjects, setFilterProjects] = createSignal<Set<string>>(new Set());
  const [filterProjectsAll, setFilterProjectsAll] = createSignal(true);
  const [filterDropdowns, setFilterDropdowns] = createSignal<Record<string, Set<string>>>({});
  const [filterDueDate, setFilterDueDate] = createSignal<DueDatePreset>("all");
  const [showFilterBar, setShowFilterBar] = createSignal(false);

  function colWidth(col: ColumnDefinition): number {
    return overrideWidths()[col.id!] ?? col.columnWidth;
  }

  function makeMetaResizer(col: ColumnDefinition) {
    return makeColumnResizer({
      getWidth: () => colWidth(col),
      setWidth: (w) => setOverrideWidths(prev => ({ ...prev, [col.id!]: w })),
      onDone: () => {},
    });
  }

  const visibleColumns = () => {
    const vis = colVisibility();
    return mergedColumns().filter(c => vis[c.columnName] !== false);
  };

  // ── Available project names for filter ──
  const availableProjects = () => {
    const names = new Set<string>();
    for (const item of items()) {
      names.add(item.jobName);
    }
    return Array.from(names).sort();
  };

  // ── Available dropdown values per column for filters ──
  const dropdownColumns = () => {
    return mergedColumns().filter(c => c.columnType === "dropdown" || c.columnType === "priority");
  };

  // ── Filtered + sorted items ──
  const filteredItems = () => {
    const list = items();
    const types = filterTypes();
    const projects = filterProjects();
    const projAll = filterProjectsAll();
    const dropFilters = filterDropdowns();
    const dueDatePreset = filterDueDate();
    const meta = metadataMap();
    const tMap = typeMap();

    return list.filter(item => {
      const itemType = tMap[item.itemPath] ?? "other";
      if (!types.has(itemType)) return false;

      if (props.mode === "aggregated" && !projAll && projects.size > 0) {
        if (!projects.has(item.jobName)) return false;
      }

      const itemMeta = meta[item.itemPath] ?? {};

      // Dropdown column filters
      for (const [colName, allowedValues] of Object.entries(dropFilters)) {
        if (allowedValues.size === 0) continue;
        const cellVal = String(itemMeta[colName] ?? "");
        if (!allowedValues.has(cellVal)) return false;
      }

      // Due date filter
      if (dueDatePreset !== "all") {
        if (!matchesDueDatePreset(itemMeta, dueDatePreset)) return false;
      }

      return true;
    });
  };

  const sortedItems = () => {
    const list = [...filteredItems()];
    const field = sortField();
    const dir = sortDir() === "asc" ? 1 : -1;
    const meta = metadataMap();
    const tMap = typeMap();

    return list.sort((a, b) => {
      if (field === "type") {
        return dir * (tMap[a.itemPath] ?? "other").localeCompare(tMap[b.itemPath] ?? "other");
      }
      if (field === "project") {
        return dir * a.jobName.localeCompare(b.jobName);
      }
      if (field === "name") {
        const aName = a.itemPath.split(/[/\\]/).pop() ?? "";
        const bName = b.itemPath.split(/[/\\]/).pop() ?? "";
        return dir * aName.toLowerCase().localeCompare(bName.toLowerCase());
      }
      if (field === "modified") {
        return dir * ((a.modifiedTime ?? 0) - (b.modifiedTime ?? 0));
      }
      // Sort by metadata column
      const aVal = meta[a.itemPath]?.[field] ?? "";
      const bVal = meta[b.itemPath]?.[field] ?? "";
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

  // ── Data loading ──

  async function loadData() {
    setIsLoading(true);
    try {
      const result = props.mode === "job" && props.jobPath
        ? await getTrackedItems(props.jobPath)
        : await getAllTrackedItems();
      setItems(result);

      // Parse metadata
      const meta: Record<string, Record<string, unknown>> = {};
      const tMap: Record<string, ItemType> = {};
      for (const item of result) {
        try {
          meta[item.itemPath] = JSON.parse(item.metadataJson || "{}");
        } catch {
          meta[item.itemPath] = {};
        }
        tMap[item.itemPath] = inferItemType(item.folderName);
      }
      setMetadataMap(meta);
      setTypeMap(tMap);

      // Build merged columns
      const cols = await buildMergedColumnDefs(result);
      setMergedColumns(cols);

      // Load visibility prefs
      const vis = loadVisibility(props.mode, props.jobPath);
      setColVisibility(vis);
    } catch (err) {
      console.error("Failed to load tracker data:", err);
    } finally {
      setIsLoading(false);
    }
  }

  onMount(() => { loadData(); });

  // Event listeners — mesh sync (peer changes)
  const unlistens: Promise<() => void>[] = [];

  unlistens.push(listen("mesh:metadata-changed", () => { loadData(); }));
  unlistens.push(listen("mesh:data-refreshed", () => { loadData(); }));
  unlistens.push(listen("mesh:table-changed", (event: any) => {
    const action = event?.payload?.action ?? "";
    if (action.startsWith("col_")) loadData();
  }));

  // Local metadata changes (from ItemListPanel or other TrackerView instances)
  let localChangeTimer: ReturnType<typeof setTimeout> | null = null;
  function onLocalMetadataChanged() {
    if (localChangeTimer) clearTimeout(localChangeTimer);
    localChangeTimer = setTimeout(() => loadData(), 300);
  }
  window.addEventListener("ufb:metadata-changed", onLocalMetadataChanged);

  onCleanup(() => {
    unlistens.forEach(p => p.then(fn => fn()));
    window.removeEventListener("ufb:metadata-changed", onLocalMetadataChanged);
  });

  // ── Metadata editing ──

  async function updateCellValue(itemPath: string, colName: string, value: unknown) {
    const item = items().find(i => i.itemPath === itemPath);
    if (!item) return;
    const existing = metadataMap()[itemPath] ?? {};
    const newJson = { ...existing, [colName]: value };
    const jsonStr = JSON.stringify(newJson);
    try {
      await upsertItemMetadata(item.jobPath, itemPath, item.folderName, jsonStr, true);
      setMetadataMap(prev => ({ ...prev, [itemPath]: newJson }));
      window.dispatchEvent(new CustomEvent("ufb:metadata-changed", {
        detail: { jobPath: item.jobPath, itemPath, isTracked: true },
      }));
    } catch (err) {
      console.error("Failed to save metadata:", err);
    }
  }

  async function untrackItem(item: TrackedItemRecord) {
    const existing = metadataMap()[item.itemPath] ?? {};
    const jsonStr = JSON.stringify(existing);
    try {
      await upsertItemMetadata(item.jobPath, item.itemPath, item.folderName, jsonStr, false);
      setItems(prev => prev.filter(i => i.itemPath !== item.itemPath));
      const newMeta = { ...metadataMap() };
      delete newMeta[item.itemPath];
      setMetadataMap(newMeta);
      window.dispatchEvent(new CustomEvent("ufb:metadata-changed", {
        detail: { jobPath: item.jobPath, itemPath: item.itemPath, isTracked: false },
      }));
    } catch (err) {
      console.error("Failed to untrack item:", err);
    }
  }

  // ── Column visibility ──

  function toggleColVisibility(colName: string) {
    const vis = { ...colVisibility() };
    vis[colName] = vis[colName] === false ? true : false;
    setColVisibility(vis);
    saveVisibility(props.mode, vis, props.jobPath);
  }

  // ── Filter helpers ──

  function toggleTypeFilter(type: ItemType) {
    setFilterTypes(prev => {
      const next = new Set(prev);
      if (next.has(type)) next.delete(type);
      else next.add(type);
      return next;
    });
  }

  function toggleProjectFilter(project: string) {
    setFilterProjectsAll(false);
    setFilterProjects(prev => {
      const next = new Set(prev);
      if (next.has(project)) next.delete(project);
      else next.add(project);
      if (next.size === 0) setFilterProjectsAll(true);
      return next;
    });
  }

  function resetProjectFilter() {
    setFilterProjectsAll(true);
    setFilterProjects(new Set<string>());
  }

  function toggleDropdownFilter(colName: string, value: string) {
    setFilterDropdowns(prev => {
      const existing = new Set(prev[colName] ?? []);
      if (existing.has(value)) existing.delete(value);
      else existing.add(value);
      return { ...prev, [colName]: existing };
    });
  }

  // ── Context menu ──

  function onRowContextMenu(e: MouseEvent, item: TrackedItemRecord) {
    e.preventDefault();
    e.stopPropagation();
    setCtxMenu({ x: e.clientX, y: e.clientY, item });
  }

  function closeCtxMenu() { setCtxMenu(null); }

  async function ctxCopyPath() {
    const m = ctxMenu(); if (m) await navigator.clipboard.writeText(m.item.itemPath);
    closeCtxMenu();
  }
  async function ctxCopyUfbLink() {
    const m = ctxMenu(); if (m) { const uri = await buildUfbUri(m.item.itemPath); await navigator.clipboard.writeText(uri); }
    closeCtxMenu();
  }
  async function ctxCopyUnionLink() {
    const m = ctxMenu(); if (m) { const uri = await buildUnionUri(m.item.itemPath); await navigator.clipboard.writeText(uri); }
    closeCtxMenu();
  }
  async function ctxReveal() {
    const m = ctxMenu(); if (m) await revealInFileManager(m.item.itemPath);
    closeCtxMenu();
  }
  function ctxOpen() {
    const m = ctxMenu(); if (m) workspaceStore.openBrowserTab(m.item.itemPath);
    closeCtxMenu();
  }
  function ctxUntrack() {
    const m = ctxMenu(); if (m) untrackItem(m.item);
    closeCtxMenu();
  }

  function onColHeaderContextMenu(e: MouseEvent) {
    e.preventDefault();
    setColHeaderMenu({ x: e.clientX, y: e.clientY });
  }

  function itemName(item: TrackedItemRecord): string {
    return item.itemPath.split(/[/\\]/).pop() ?? item.itemPath;
  }

  return (
    <div class="tracker-view" onClick={() => { closeCtxMenu(); setColHeaderMenu(null); }}>
      {/* Header */}
      <div class="tracker-view-header">
        <span class="tracker-view-title">
          {props.mode === "job" ? `${props.jobName} Tracker` : "Tracker"}
        </span>
        <span class="tracker-view-count">{filteredItems().length} / {items().length} items</span>
        <button
          class={`tracker-view-filter-btn ${showFilterBar() ? "active" : ""}`}
          onClick={() => setShowFilterBar(prev => !prev)}
          title="Toggle filters"
        >
          <span class="icon">filter_list</span>
        </button>
        <button class="tracker-view-refresh-btn" onClick={() => loadData()} title="Refresh">
          <span class="icon">refresh</span>
        </button>
      </div>

      {/* Filter bar */}
      <Show when={showFilterBar()}>
        <div class="tracker-filter-bar">
          {/* Type filter */}
          <div class="tracker-filter-group">
            <span class="tracker-filter-label">Type</span>
            <For each={(["shot", "asset", "posting", "other"] as ItemType[])}>
              {(type) => (
                <label class="tracker-filter-check">
                  <input
                    type="checkbox"
                    checked={filterTypes().has(type)}
                    onChange={() => toggleTypeFilter(type)}
                  />
                  <span class="tracker-type-badge" style={{ background: TYPE_COLORS[type] }}>
                    {TYPE_LABELS[type]}
                  </span>
                </label>
              )}
            </For>
          </div>

          {/* Project filter (aggregated only) */}
          <Show when={props.mode === "aggregated"}>
            <div class="tracker-filter-group">
              <span class="tracker-filter-label">Project</span>
              <label class="tracker-filter-check">
                <input
                  type="checkbox"
                  checked={filterProjectsAll()}
                  onChange={() => resetProjectFilter()}
                />
                All
              </label>
              <For each={availableProjects()}>
                {(proj) => (
                  <label class="tracker-filter-check">
                    <input
                      type="checkbox"
                      checked={filterProjectsAll() || filterProjects().has(proj)}
                      onChange={() => toggleProjectFilter(proj)}
                    />
                    {proj}
                  </label>
                )}
              </For>
            </div>
          </Show>

          {/* Dropdown column filters */}
          <For each={dropdownColumns()}>
            {(col) => {
              const meta = metadataMap();
              const available = () => {
                const vals = new Set<string>();
                for (const item of items()) {
                  const v = meta[item.itemPath]?.[col.columnName];
                  if (v != null && v !== "") vals.add(String(v));
                }
                return Array.from(vals).sort();
              };
              return (
                <Show when={available().length > 0}>
                  <div class="tracker-filter-group">
                    <span class="tracker-filter-label">{col.columnName}</span>
                    <For each={available()}>
                      {(val) => (
                        <label class="tracker-filter-check">
                          <input
                            type="checkbox"
                            checked={!(filterDropdowns()[col.columnName]?.size) || filterDropdowns()[col.columnName]?.has(val)}
                            onChange={() => toggleDropdownFilter(col.columnName, val)}
                          />
                          {col.columnType === "priority" ? (["", "Low", "Med", "High"][parseInt(val)] ?? val) : val}
                        </label>
                      )}
                    </For>
                  </div>
                </Show>
              );
            }}
          </For>

          {/* Due date filter */}
          <div class="tracker-filter-group">
            <span class="tracker-filter-label">Due Date</span>
            <select
              class="tracker-filter-select"
              value={filterDueDate()}
              onChange={(e) => setFilterDueDate(e.currentTarget.value as DueDatePreset)}
            >
              <option value="all">All</option>
              <option value="overdue">Overdue</option>
              <option value="today">Today</option>
              <option value="this_week">This Week</option>
              <option value="this_month">This Month</option>
            </select>
          </div>
        </div>
      </Show>

      <Show when={!isLoading()} fallback={<div class="tracker-view-loading">Loading...</div>}>
        {/* Column headers */}
        <div class="tracker-columns" onContextMenu={onColHeaderContextMenu}>
          <div class="tracker-col-header col-type" onClick={() => toggleSort("type")}>
            Type
            {sortField() === "type" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
          <Show when={props.mode === "aggregated"}>
            <div class="tracker-col-header col-project" onClick={() => toggleSort("project")}>
              Project
              {sortField() === "project" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
            </div>
          </Show>
          <div class="tracker-col-header col-name" onClick={() => toggleSort("name")}>
            Name
            {sortField() === "name" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
          <For each={visibleColumns()}>
            {(col) => {
              const resizer = makeMetaResizer(col);
              return (
                <>
                  <div
                    class="tracker-col-header col-meta"
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
          <div class="tracker-col-header col-modified" onClick={() => toggleSort("modified")}>
            Modified
            {sortField() === "modified" && <span class="sort-arrow">{sortDir() === "asc" ? "\u25B2" : "\u25BC"}</span>}
          </div>
        </div>

        {/* Rows */}
        <div class="tracker-scroll">
          <Show when={sortedItems().length > 0} fallback={<div class="tracker-view-empty">No tracked items</div>}>
            <For each={sortedItems()}>
              {(item) => {
                const meta = () => metadataMap()[item.itemPath] ?? {};
                const type = () => typeMap()[item.itemPath] ?? "other";
                return (
                  <div
                    class="tracker-row"
                    onContextMenu={(e) => onRowContextMenu(e, item)}
                    onDblClick={() => workspaceStore.openBrowserTab(item.itemPath)}
                  >
                    <span class="tracker-row-type">
                      <span class="tracker-type-badge" style={{ background: TYPE_COLORS[type()] }}>
                        {TYPE_LABELS[type()]}
                      </span>
                    </span>
                    <Show when={props.mode === "aggregated"}>
                      <span class="tracker-row-project truncate">{item.jobName}</span>
                    </Show>
                    <span class="tracker-row-name truncate" title={item.itemPath}>
                      {itemName(item)}
                    </span>
                    <For each={visibleColumns()}>
                      {(col) => {
                        const val = () => meta()[col.columnName];
                        const editing = () => editingCell();
                        const isEditing = () => editing()?.itemPath === item.itemPath && editing()?.colName === col.columnName;
                        return (
                          <span
                            class="tracker-row-meta"
                            style={{ width: `${colWidth(col)}px`, "min-width": `${Math.min(colWidth(col), 60)}px` }}
                          >
                            {renderCellValue({
                              itemPath: item.itemPath,
                              value: val(),
                              col,
                              isEditing: isEditing(),
                              onUpdate: (v) => updateCellValue(item.itemPath, col.columnName, v),
                              onStartEdit: () => setEditingCell({ itemPath: item.itemPath, colName: col.columnName }),
                              onStopEdit: () => setEditingCell(null),
                            })}
                          </span>
                        );
                      }}
                    </For>
                    <span class="tracker-row-modified">{formatDate(item.modifiedTime)}</span>
                  </div>
                );
              }}
            </For>
          </Show>
        </div>
      </Show>

      {/* Context menu */}
      <Show when={ctxMenu()}>
        {(menu) => (
          <div class="ctx-menu" style={{ left: `${menu().x}px`, top: `${menu().y}px` }} ref={adjustMenuPosition}>
            <div class="ctx-menu-header truncate">{itemName(menu().item)}</div>
            <div class="ctx-menu-item" onClick={ctxOpen}>
              <span class="icon">open_in_new</span> Open
            </div>
            <div class="ctx-menu-item" onClick={ctxReveal}>
              <span class="icon">folder_open</span> Reveal in Explorer
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item" onClick={ctxCopyPath}>
              <span class="icon">content_copy</span> Copy Path
            </div>
            <div class="ctx-menu-item" onClick={ctxCopyUfbLink}>
              <span class="icon">link</span> Copy ufb:/// Link
            </div>
            <div class="ctx-menu-item" onClick={ctxCopyUnionLink}>
              <span class="icon">link</span> Copy union:/// Link
            </div>
            <div class="ctx-menu-separator" />
            <div class="ctx-menu-item ctx-menu-danger" onClick={ctxUntrack}>
              <span class="icon">star_border</span> Un-track
            </div>
          </div>
        )}
      </Show>

      {/* Column visibility menu */}
      <Show when={colHeaderMenu()}>
        {(menu) => (
          <div class="ctx-menu col-vis-menu" style={{ left: `${menu().x}px`, top: `${menu().y}px` }} ref={adjustMenuPosition}>
            <div class="ctx-menu-header">Show Columns</div>
            <For each={mergedColumns()}>
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
          </div>
        )}
      </Show>
    </div>
  );
}
