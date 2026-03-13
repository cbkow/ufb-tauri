import { createSignal, For, Show } from "solid-js";
import "./ConsolePanel.css";

interface LogEntry {
  timestamp: number;
  level: "info" | "warn" | "error";
  message: string;
  source: string;
}

export function ConsolePanel() {
  const [logs, setLogs] = createSignal<LogEntry[]>([]);
  const [filter, setFilter] = createSignal("");
  const [levelFilter, setLevelFilter] = createSignal<string>("all");
  const [autoScroll, setAutoScroll] = createSignal(true);

  const filteredLogs = () => {
    let entries = logs();
    const level = levelFilter();
    if (level !== "all") {
      entries = entries.filter((l) => l.level === level);
    }
    const query = filter().toLowerCase();
    if (query) {
      entries = entries.filter((l) => l.message.toLowerCase().includes(query));
    }
    return entries;
  };

  function formatTime(ts: number): string {
    const d = new Date(ts);
    return d.toLocaleTimeString(undefined, { hour12: false });
  }

  return (
    <div class="console-panel">
      <div class="console-header">
        <span>Console</span>
        <div class="console-controls">
          <select
            value={levelFilter()}
            onChange={(e) => setLevelFilter(e.currentTarget.value)}
          >
            <option value="all">All</option>
            <option value="info">Info</option>
            <option value="warn">Warn</option>
            <option value="error">Error</option>
          </select>
          <input
            type="text"
            placeholder="Filter..."
            value={filter()}
            onInput={(e) => setFilter(e.currentTarget.value)}
            class="console-filter"
          />
          <button
            class={`console-btn ${autoScroll() ? "active" : ""}`}
            onClick={() => setAutoScroll(!autoScroll())}
            title="Auto-scroll"
          >
            <span class="icon">vertical_align_bottom</span>
          </button>
          <button
            class="console-btn"
            onClick={() => setLogs([])}
            title="Clear"
          >
            <span class="icon">delete</span>
          </button>
        </div>
      </div>
      <div class="console-content">
        <For each={filteredLogs()}>
          {(entry) => (
            <div class={`console-line level-${entry.level}`}>
              <span class="console-time">{formatTime(entry.timestamp)}</span>
              <span class={`console-level`}>{entry.level.toUpperCase()}</span>
              <span class="console-source">[{entry.source}]</span>
              <span class="console-message">{entry.message}</span>
            </div>
          )}
        </For>
        <Show when={filteredLogs().length === 0}>
          <div class="console-empty">No log entries</div>
        </Show>
      </div>
    </div>
  );
}
