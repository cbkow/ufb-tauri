import { Show, For, createSignal } from "solid-js";
import { fileOpsStore } from "../../stores/fileOpsStore";
import "./FileOpsProgress.css";

export function FileOpsProgress() {
  const ops = () => fileOpsStore.operations();

  return (
    <Show when={ops().length > 0}>
      <div class="fileops-bar">
        <For each={ops()}>
          {(op) => {
            const [showErrors, setShowErrors] = createSignal(false);

            const percent = () => {
              if (op.totalBytes > 0)
                return Math.round((op.copiedBytes / op.totalBytes) * 100);
              if (op.itemsTotal > 0)
                return Math.round((op.itemsDone / op.itemsTotal) * 100);
              return 0;
            };

            const label = () => {
              if (op.status === "completed_with_errors") {
                const verb = op.operation === "move" ? "Moved" : "Copied";
                return `${verb} ${op.succeeded ?? 0}/${op.itemsTotal} items (${op.failed ?? 0} failed)`;
              }
              if (op.status === "completed") {
                const verb = op.operation === "move" ? "Moved" :
                  op.operation === "delete" ? "Deleted" : "Copied";
                return `${verb} ${op.itemsTotal} item${op.itemsTotal !== 1 ? "s" : ""}`;
              }
              if (op.status === "error") return `Failed: ${op.error}`;
              const verb =
                op.operation === "copy" ? "Copying" :
                op.operation === "move" ? "Moving" : "Deleting";
              return `${verb} ${op.itemsTotal} item${op.itemsTotal !== 1 ? "s" : ""}`;
            };

            const shortFile = () => {
              const f = op.currentFile;
              if (!f) return "";
              const parts = f.split(/[\\/]/);
              return parts[parts.length - 1] || f;
            };

            const itemsPercent = () =>
              op.itemsTotal > 1 ? Math.round((op.itemsDone / op.itemsTotal) * 100) : 0;

            return (
              <div class={`fileops-item fileops-${op.status === "completed_with_errors" ? "warning" : op.status}`}>
                <Show when={op.status === "active"}>
                  <span class="icon fileops-spinner">sync</span>
                </Show>
                <Show when={op.status === "completed"}>
                  <span class="icon fileops-check">check_circle</span>
                </Show>
                <Show when={op.status === "completed_with_errors"}>
                  <span class="icon fileops-warning-icon">warning</span>
                </Show>
                <Show when={op.status === "error"}>
                  <span class="icon fileops-error-icon">error</span>
                </Show>

                <div class="fileops-details">
                  <div class="fileops-row">
                    <span class="fileops-label">{label()}</span>
                    <Show when={shortFile() && op.status === "active"}>
                      <span class="fileops-filename">{shortFile()}</span>
                    </Show>
                    <Show when={op.status === "completed_with_errors" && op.errors && op.errors.length > 0}>
                      <button
                        class="fileops-toggle-errors"
                        onClick={() => setShowErrors(!showErrors())}
                      >
                        {showErrors() ? "Hide errors" : "Show errors"}
                      </button>
                    </Show>
                  </div>

                  <Show when={op.status === "active"}>
                    {/* Overall items progress (shown when multiple items) */}
                    <Show when={op.itemsTotal > 1}>
                      <div class="fileops-row">
                        <span class="fileops-items-count">{op.itemsDone} / {op.itemsTotal}</span>
                        <div class="fileops-progress-track">
                          <div
                            class="fileops-progress-fill fileops-items-fill"
                            style={{ width: `${itemsPercent()}%` }}
                          />
                        </div>
                      </div>
                    </Show>

                    {/* Per-file byte progress */}
                    <div class="fileops-row">
                      <div class="fileops-progress-track">
                        <div
                          class="fileops-progress-fill"
                          style={{ width: `${percent()}%` }}
                        />
                      </div>
                      <span class="fileops-percent">{percent()}%</span>
                    </div>
                  </Show>

                  <Show when={showErrors() && op.errors}>
                    <div class="fileops-error-list">
                      <For each={op.errors}>
                        {(err) => <div class="fileops-error-line">{err}</div>}
                      </For>
                    </div>
                  </Show>
                </div>

                <button
                  class="fileops-dismiss"
                  onClick={() => fileOpsStore.dismiss(op.id)}
                  title="Dismiss"
                >
                  <span class="icon">close</span>
                </button>
              </div>
            );
          }}
        </For>
      </div>
    </Show>
  );
}
