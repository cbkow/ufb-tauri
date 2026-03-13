import { Show, For } from "solid-js";
import type { ColumnDefinition } from "./types";

export interface CellRenderParams {
  itemPath: string;
  value: unknown;
  col: ColumnDefinition;
  isEditing: boolean;
  onUpdate: (value: unknown) => void;
  onStartEdit: () => void;
  onStopEdit: () => void;
}

export function formatDate(timestamp: number | null): string {
  if (!timestamp) return "";
  const d = new Date(timestamp);
  const now = new Date();
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  if (d.getFullYear() === now.getFullYear()) {
    return `${months[d.getMonth()]} ${String(d.getDate()).padStart(2, "0")}`;
  }
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

export function renderCellValue(params: CellRenderParams) {
  const { value: val, col, isEditing, onUpdate, onStartEdit, onStopEdit } = params;

  switch (col.columnType) {
    case "dropdown": {
      const selectedOpt = () => col.options.find(o => o.name === val);
      return (
        <span class="meta-cell-dropdown">
          <Show when={selectedOpt()?.color}>
            <span class="meta-cell-dot" style={{ background: selectedOpt()!.color! }} />
          </Show>
          <select
            class="meta-cell-select"
            value={String(val ?? "")}
            onChange={(e) => onUpdate(e.currentTarget.value)}
            onClick={(e) => e.stopPropagation()}
          >
            <option value=""></option>
            <For each={col.options}>
              {(opt) => <option value={opt.name}>{opt.name}</option>}
            </For>
          </select>
        </span>
      );
    }
    case "priority": {
      const numVal = typeof val === "number" ? val : 0;
      return (
        <select
          class="meta-cell-select"
          value={String(numVal)}
          onChange={(e) => onUpdate(parseInt(e.currentTarget.value) || 0)}
          onClick={(e) => e.stopPropagation()}
        >
          <option value="0"></option>
          <option value="1">Low</option>
          <option value="2">Med</option>
          <option value="3">High</option>
        </select>
      );
    }
    case "checkbox": {
      return (
        <input
          type="checkbox"
          class="meta-cell-check"
          checked={!!val}
          onChange={(e) => onUpdate(e.currentTarget.checked)}
          onClick={(e) => e.stopPropagation()}
        />
      );
    }
    case "date": {
      const dateStr = val ? new Date(val as number).toISOString().split("T")[0] : "";
      return (
        <input
          type="date"
          class="meta-cell-date"
          value={dateStr}
          onChange={(e) => {
            const ms = e.currentTarget.value ? new Date(e.currentTarget.value).getTime() : null;
            onUpdate(ms);
          }}
          onClick={(e) => e.stopPropagation()}
        />
      );
    }
    case "number": {
      if (isEditing) {
        return (
          <input
            type="number"
            class="meta-cell-input"
            value={val != null ? String(val) : ""}
            onBlur={(e) => {
              const n = parseFloat(e.currentTarget.value);
              onUpdate(isNaN(n) ? null : n);
              onStopEdit();
            }}
            onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
            onClick={(e) => e.stopPropagation()}
            ref={(el) => setTimeout(() => el.focus(), 0)}
          />
        );
      }
      return (
        <span
          class="meta-cell-text"
          onDblClick={(e) => { e.stopPropagation(); onStartEdit(); }}
        >
          {val != null ? String(val) : ""}
        </span>
      );
    }
    case "links": {
      const linksVal = Array.isArray(val) ? (val as string[]).join("\n") : String(val ?? "");
      const urls = linksVal.split("\n").map(s => s.trim()).filter(Boolean);

      if (isEditing) {
        return (
          <textarea
            class="meta-cell-links-edit"
            value={linksVal}
            rows={3}
            onBlur={(e) => {
              const lines = e.currentTarget.value.split("\n").map(s => s.trim()).filter(Boolean);
              onUpdate(lines.length > 0 ? lines.join("\n") : null);
              onStopEdit();
            }}
            onKeyDown={(e) => { if (e.key === "Escape") onStopEdit(); }}
            onClick={(e) => e.stopPropagation()}
            ref={(el) => setTimeout(() => el.focus(), 0)}
          />
        );
      }

      if (urls.length === 0) {
        return (
          <span
            class="meta-cell-text"
            onDblClick={(e) => { e.stopPropagation(); onStartEdit(); }}
          />
        );
      }

      return (
        <span class="meta-cell-links" onClick={(e) => e.stopPropagation()}>
          <For each={urls}>
            {(url) => {
              const label = (() => {
                try { return new URL(url).hostname; } catch { return url.slice(0, 20); }
              })();
              return (
                <a
                  class="meta-cell-link"
                  href={url}
                  target="_blank"
                  rel="noopener"
                  title={url}
                  onClick={(e) => { e.preventDefault(); e.stopPropagation(); window.open(url, "_blank"); }}
                >
                  {label}
                </a>
              );
            }}
          </For>
          <span
            class="meta-cell-link-edit-btn"
            onClick={(e) => { e.stopPropagation(); onStartEdit(); }}
            title="Edit links"
          >
            <span class="icon" style={{ "font-size": "12px" }}>edit</span>
          </span>
        </span>
      );
    }
    case "text":
    case "note":
    default: {
      if (isEditing) {
        return (
          <input
            type="text"
            class="meta-cell-input"
            value={String(val ?? "")}
            onBlur={(e) => {
              onUpdate(e.currentTarget.value || null);
              onStopEdit();
            }}
            onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); if (e.key === "Escape") onStopEdit(); }}
            onClick={(e) => e.stopPropagation()}
            ref={(el) => setTimeout(() => el.focus(), 0)}
          />
        );
      }
      const display = col.columnType === "note" ? String(val ?? "").slice(0, 40) : String(val ?? "");
      return (
        <span
          class="meta-cell-text"
          onDblClick={(e) => { e.stopPropagation(); onStartEdit(); }}
          title={col.columnType === "note" ? String(val ?? "") : undefined}
        >
          {display}
        </span>
      );
    }
  }
}
