import { createSignal, onMount, For, Index, Show } from "solid-js";
import { addColumn, updateColumn, deleteColumn, getColumnPresets, saveColumnPreset, deleteColumnPreset, addPresetColumn } from "../../lib/tauri";
import type { ColumnDefinition, ColumnOption, ColumnPreset } from "../../lib/types";
import "./ColumnManagerDialog.css";

const COLUMN_TYPES = [
  { value: "text", label: "Text" },
  { value: "dropdown", label: "Dropdown" },
  { value: "date", label: "Date" },
  { value: "number", label: "Number" },
  { value: "priority", label: "Priority" },
  { value: "checkbox", label: "Checkbox" },
  { value: "links", label: "Links" },
  { value: "note", label: "Note" },
] as const;

interface ColumnManagerDialogProps {
  jobPath: string;
  folderName: string;
  columns: ColumnDefinition[];
  onClose: () => void;
  onColumnsChanged: () => void;
}

export function ColumnManagerDialog(props: ColumnManagerDialogProps) {
  const [editingColumn, setEditingColumn] = createSignal<ColumnDefinition | null>(null);
  const [isNew, setIsNew] = createSignal(false);
  const [presets, setPresets] = createSignal<ColumnPreset[]>([]);
  const [selectedPresetId, setSelectedPresetId] = createSignal<number | null>(null);

  onMount(() => { loadPresets(); });

  async function loadPresets() {
    try { setPresets(await getColumnPresets()); } catch (e) { console.error("Failed to load presets:", e); }
  }

  async function handleSaveColumnAsPreset(col: ColumnDefinition) {
    const name = window.prompt(`Save "${col.columnName}" as preset:`, col.columnName);
    if (!name?.trim()) return;
    try {
      await saveColumnPreset(name.trim(), col);
      await loadPresets();
    } catch (e) { console.error("Failed to save preset:", e); }
  }

  async function handleAddPresetColumn() {
    const id = selectedPresetId();
    if (!id) return;
    try {
      await addPresetColumn(id, props.jobPath, props.folderName);
      props.onColumnsChanged();
    } catch (e) { console.error("Failed to add preset column:", e); }
  }

  async function handleDeletePreset() {
    const id = selectedPresetId();
    if (!id) return;
    const preset = presets().find(p => p.id === id);
    if (!preset || !window.confirm(`Delete preset "${preset.presetName}"?`)) return;
    try {
      await deleteColumnPreset(id);
      setSelectedPresetId(null);
      await loadPresets();
    } catch (e) { console.error("Failed to delete preset:", e); }
  }

  function startAdd() {
    setEditingColumn({
      jobPath: props.jobPath,
      folderName: props.folderName,
      columnName: "",
      columnType: "text",
      columnOrder: props.columns.length,
      columnWidth: 100,
      isVisible: true,
      options: [],
    });
    setIsNew(true);
  }

  function startEdit(col: ColumnDefinition) {
    setEditingColumn({ ...col, options: col.options.map(o => ({ ...o })) });
    setIsNew(false);
  }

  async function handleDelete(col: ColumnDefinition) {
    if (!col.id) return;
    if (!window.confirm(`Delete column "${col.columnName}"?`)) return;
    try {
      await deleteColumn(col.id);
      props.onColumnsChanged();
    } catch (err) {
      console.error("Failed to delete column:", err);
    }
  }

  function moveColumn(col: ColumnDefinition, dir: -1 | 1) {
    const idx = props.columns.findIndex(c => c.id === col.id);
    const swapIdx = idx + dir;
    if (swapIdx < 0 || swapIdx >= props.columns.length) return;

    const a = props.columns[idx];
    const b = props.columns[swapIdx];

    // Swap orders
    const tmpOrder = a.columnOrder;
    Promise.all([
      updateColumn({ ...a, columnOrder: b.columnOrder }),
      updateColumn({ ...b, columnOrder: tmpOrder }),
    ]).then(() => props.onColumnsChanged());
  }

  return (
    <div class="col-mgr-overlay">
      <div class="col-mgr-dialog">
        <div class="col-mgr-header">
          <span>Manage Columns</span>
          <button class="col-mgr-close" onClick={props.onClose}>&times;</button>
        </div>

        <Show when={!editingColumn()} fallback={
          <ColumnEditor
            column={editingColumn()!}
            isNew={isNew()}
            onSave={async (col) => {
              try {
                if (isNew()) {
                  await addColumn(col);
                } else {
                  await updateColumn(col);
                }
                setEditingColumn(null);
                props.onColumnsChanged();
              } catch (err) {
                console.error("Failed to save column:", err);
              }
            }}
            onCancel={() => setEditingColumn(null)}
          />
        }>
          <div class="col-mgr-list">
            <Show when={props.columns.length === 0}>
              <div class="col-mgr-empty">No columns defined. Click + to add one.</div>
            </Show>
            <For each={props.columns}>
              {(col, i) => (
                <div class="col-mgr-row">
                  <div class="col-mgr-row-info">
                    <span class="col-mgr-row-name">{col.columnName}</span>
                    <span class="col-mgr-row-type">{col.columnType}</span>
                  </div>
                  <div class="col-mgr-row-actions">
                    <button
                      class="col-mgr-btn"
                      onClick={() => moveColumn(col, -1)}
                      disabled={i() === 0}
                      title="Move up"
                    >
                      <span class="icon">arrow_upward</span>
                    </button>
                    <button
                      class="col-mgr-btn"
                      onClick={() => moveColumn(col, 1)}
                      disabled={i() === props.columns.length - 1}
                      title="Move down"
                    >
                      <span class="icon">arrow_downward</span>
                    </button>
                    <button class="col-mgr-btn" onClick={() => handleSaveColumnAsPreset(col)} title="Save as preset">
                      <span class="icon">bookmark_add</span>
                    </button>
                    <button class="col-mgr-btn" onClick={() => startEdit(col)} title="Edit">
                      <span class="icon">edit</span>
                    </button>
                    <button class="col-mgr-btn col-mgr-btn-danger" onClick={() => handleDelete(col)} title="Delete">
                      <span class="icon">delete</span>
                    </button>
                  </div>
                </div>
              )}
            </For>
          </div>
          <div class="col-mgr-presets">
            <select
              class="col-mgr-preset-select"
              value={selectedPresetId()?.toString() ?? ""}
              onChange={(e) => setSelectedPresetId(e.currentTarget.value ? Number(e.currentTarget.value) : null)}
            >
              <option value="">Presets...</option>
              <For each={presets()}>
                {(p) => <option value={p.id}>{p.presetName}</option>}
              </For>
            </select>
            <button class="col-mgr-btn" onClick={handleAddPresetColumn} disabled={!selectedPresetId()} title="Add preset column">
              <span class="icon">add</span>
            </button>
            <button class="col-mgr-btn col-mgr-btn-danger" onClick={handleDeletePreset} disabled={!selectedPresetId()} title="Delete preset">
              <span class="icon">delete</span>
            </button>
          </div>
          <div class="col-mgr-footer">
            <button class="col-mgr-add-btn" onClick={startAdd}>
              <span class="icon">add</span> Add Column
            </button>
          </div>
        </Show>
      </div>
    </div>
  );
}

// ── Column Editor (add/edit single column) ──

interface ColumnEditorProps {
  column: ColumnDefinition;
  isNew: boolean;
  onSave: (col: ColumnDefinition) => void;
  onCancel: () => void;
}

function ColumnEditor(props: ColumnEditorProps) {
  const [name, setName] = createSignal(props.column.columnName);
  const [type, setType] = createSignal(props.column.columnType);
  const [width, setWidth] = createSignal(props.column.columnWidth);
  const [defaultValue, setDefaultValue] = createSignal(props.column.defaultValue ?? "");
  const [options, setOptions] = createSignal<ColumnOption[]>(props.column.options.map(o => ({ ...o })));

  function addOption() {
    setOptions([...options(), { name: "", color: "#6b7280" }]);
  }

  function updateOption(idx: number, field: "name" | "color", value: string) {
    setOptions(opts => opts.map((o, i) => i === idx ? { ...o, [field]: value } : o));
  }

  function removeOption(idx: number) {
    setOptions(opts => opts.filter((_, i) => i !== idx));
  }

  function handleSave() {
    const trimmed = name().trim();
    if (!trimmed) return;
    props.onSave({
      ...props.column,
      columnName: trimmed,
      columnType: type(),
      columnWidth: width(),
      defaultValue: defaultValue() || undefined,
      options: type() === "dropdown" ? options().filter(o => o.name.trim()) : [],
    });
  }

  return (
    <div class="col-editor">
      <div class="col-editor-title">{props.isNew ? "Add Column" : "Edit Column"}</div>

      <label class="col-editor-label">
        Name
        <input
          type="text"
          class="col-editor-input"
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          placeholder="Column name"
          ref={(el) => setTimeout(() => el.focus(), 50)}
        />
      </label>

      <label class="col-editor-label">
        Type
        <select class="col-editor-input" value={type()} onChange={(e) => setType(e.currentTarget.value as ColumnDefinition["columnType"])}>
          <For each={COLUMN_TYPES}>
            {(t) => <option value={t.value}>{t.label}</option>}
          </For>
        </select>
      </label>

      <label class="col-editor-label">
        Width (px)
        <input
          type="number"
          class="col-editor-input"
          value={width()}
          onInput={(e) => setWidth(parseInt(e.currentTarget.value) || 100)}
          min={40}
          max={400}
        />
      </label>

      <label class="col-editor-label">
        Default Value
        <input
          type="text"
          class="col-editor-input"
          value={defaultValue()}
          onInput={(e) => setDefaultValue(e.currentTarget.value)}
          placeholder="Optional"
        />
      </label>

      {/* Dropdown option builder */}
      <Show when={type() === "dropdown"}>
        <div class="col-editor-options">
          <div class="col-editor-options-title">Dropdown Options</div>
          <Index each={options()}>
            {(opt, i) => (
              <div class="col-editor-option-row">
                <input
                  type="color"
                  class="col-editor-color"
                  value={opt().color ?? "#6b7280"}
                  onInput={(e) => updateOption(i, "color", e.currentTarget.value)}
                  title="Option color"
                />
                <input
                  type="text"
                  class="col-editor-input col-editor-option-name"
                  value={opt().name}
                  onInput={(e) => updateOption(i, "name", e.currentTarget.value)}
                  placeholder="Option name"
                />
                <button class="col-mgr-btn col-mgr-btn-danger" onClick={() => removeOption(i)} title="Remove">
                  <span class="icon">close</span>
                </button>
              </div>
            )}
          </Index>
          <button class="col-editor-add-option" onClick={addOption}>
            <span class="icon">add</span> Add Option
          </button>
        </div>
      </Show>

      <div class="col-editor-actions">
        <button class="col-editor-cancel" onClick={props.onCancel}>Cancel</button>
        <button class="col-editor-save" onClick={handleSave} disabled={!name().trim()}>
          {props.isNew ? "Add" : "Save"}
        </button>
      </div>
    </div>
  );
}
