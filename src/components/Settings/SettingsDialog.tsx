import { createSignal, createResource, For, Show, onCleanup } from "solid-js";
import { settingsStore, ACCENT_COLORS } from "../../stores/settingsStore";
import { getMeshStatus, setMeshEnabled, triggerFlushEdits, triggerSnapshot, pickFolder, relaunchApp } from "../../lib/tauri";
import type { MeshSyncStatus, PathMapping } from "../../lib/types";
import "./SettingsDialog.css";

interface SettingsDialogProps {
  onClose: () => void;
}

/** Helper: create an input that only commits to the store on blur, avoiding SolidJS re-render on every keystroke. */
function SettingsInput(props: {
  type?: string;
  value: string | number;
  placeholder?: string;
  class?: string;
  min?: number;
  max?: number;
  onCommit: (value: string) => void;
}) {
  let ref!: HTMLInputElement;
  return (
    <input
      ref={ref}
      type={props.type ?? "text"}
      value={props.value}
      placeholder={props.placeholder}
      class={props.class}
      min={props.min}
      max={props.max}
      onBlur={() => {
        if (ref.value !== String(props.value)) {
          props.onCommit(ref.value);
        }
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter") ref.blur();
      }}
    />
  );
}

export function SettingsDialog(props: SettingsDialogProps) {
  const [activeSection, setActiveSection] = createSignal("appearance");

  const sections = [
    { id: "appearance", label: "Appearance" },
    { id: "paths", label: "Path Mappings" },
    { id: "sync", label: "Sync" },
    { id: "integrations", label: "Integrations" },
  ];

  // Poll mesh sync status every 3s while on the sync tab
  const [meshStatusTick, setMeshStatusTick] = createSignal(0);
  const [meshStatus] = createResource(meshStatusTick, async () => {
    try {
      return await getMeshStatus();
    } catch {
      return null;
    }
  });

  const statusInterval = setInterval(() => {
    if (activeSection() === "sync") {
      setMeshStatusTick((n) => n + 1);
    }
  }, 3000);
  onCleanup(() => clearInterval(statusInterval));

  function setAccentColor(index: number) {
    settingsStore.setSettings("appearance", "customAccentColorIndex", index);
    document.documentElement.style.setProperty("--accent-color", ACCENT_COLORS[index]);
    settingsStore.save();
  }

  const isMeshConfigured = () => {
    const ms = settingsStore.settings.meshSync;
    return ms.farmPath.trim() !== "";
  };

  async function toggleSyncEnabled() {
    const newVal = !settingsStore.settings.sync.enabled;
    settingsStore.setSettings("sync", "enabled", newVal);
    await settingsStore.save();
    try {
      await setMeshEnabled(newVal);
    } catch (e) {
      console.error("Failed to toggle mesh sync:", e);
    }
    setMeshStatusTick((n) => n + 1);
  }

  async function handleSyncNow() {
    try {
      await triggerFlushEdits();
      await triggerSnapshot();
      setMeshStatusTick((n) => n + 1);
    } catch (e) {
      console.error("Sync now failed:", e);
    }
  }

  async function saveAndRestart() {
    await settingsStore.save();
    await relaunchApp();
  }

  async function browseFarmPath() {
    try {
      const selected = await pickFolder("Select Farm Path");
      if (selected) {
        settingsStore.setSettings("meshSync", "farmPath", selected);
        settingsStore.save();
      }
    } catch (e) {
      console.error("Browse failed:", e);
    }
  }

  function statusLabel(status: MeshSyncStatus | null | undefined): { text: string; color: string } {
    if (!status) return { text: "Disabled", color: "var(--text-disabled)" };
    if (!status.isConfigured) return { text: "Not configured", color: "var(--warning)" };
    if (!status.isEnabled) return { text: "Disabled", color: "var(--text-disabled)" };
    if (status.isLeader) return { text: "Leader", color: "var(--success)" };
    return { text: "Follower", color: "var(--info)" };
  }

  function formatTimestamp(ts: number | null | undefined): string {
    if (!ts) return "Never";
    return new Date(ts * 1000).toLocaleString();
  }

  return (
    <div class="settings-overlay">
      <div class="settings-dialog">
        <div class="settings-header">
          <span>Settings</span>
          <button class="settings-close" onClick={props.onClose}>
            &times;
          </button>
        </div>
        <div class="settings-body">
          <div class="settings-nav">
            <For each={sections}>
              {(section) => (
                <button
                  class={`settings-nav-item ${activeSection() === section.id ? "active" : ""}`}
                  onClick={() => setActiveSection(section.id)}
                >
                  {section.label}
                </button>
              )}
            </For>
          </div>
          <div class="settings-content">
            {/* ── Appearance ── */}
            <Show when={activeSection() === "appearance"}>
              <div class="settings-section">
                <h3>Zoom</h3>
                <p class="settings-hint">Use Ctrl + / Ctrl - to zoom in and out. Ctrl+0 resets to default.</p>

                <h3>Accent Color</h3>
                <div class="accent-palette">
                  <For each={ACCENT_COLORS}>
                    {(color, i) => (
                      <button
                        class={`accent-swatch ${settingsStore.settings.appearance.customAccentColorIndex === i() ? "active" : ""}`}
                        style={{ background: color }}
                        onClick={() => setAccentColor(i())}
                      />
                    )}
                  </For>
                </div>
              </div>
            </Show>

            {/* ── Path Mappings ── */}
            <Show when={activeSection() === "paths"}>
              <div class="settings-section">
                <h3>Cross-OS Path Mappings</h3>
                <p class="settings-hint">
                  Map equivalent paths across operating systems so ufb:/// links work between Windows, macOS, and Linux.
                  Each row maps the same location on different OSes.
                </p>

                <div class="path-mappings-table">
                  <div class="path-mappings-header">
                    <span class="pm-cell pm-header">Windows</span>
                    <span class="pm-cell pm-header">macOS</span>
                    <span class="pm-cell pm-header">Linux</span>
                    <span class="pm-actions-cell" />
                  </div>
                  <For each={settingsStore.settings.pathMappings}>
                    {(mapping, i) => (
                      <div class="path-mappings-row">
                        <SettingsInput
                          class="pm-cell pm-input"
                          value={mapping.win}
                          placeholder="C:/Volumes/jobs"
                          onCommit={(v) => {
                            settingsStore.setSettings("pathMappings", i(), "win", v);
                            settingsStore.save();
                          }}
                        />
                        <SettingsInput
                          class="pm-cell pm-input"
                          value={mapping.mac}
                          placeholder="/Volumes/jobs"
                          onCommit={(v) => {
                            settingsStore.setSettings("pathMappings", i(), "mac", v);
                            settingsStore.save();
                          }}
                        />
                        <SettingsInput
                          class="pm-cell pm-input"
                          value={mapping.lin}
                          placeholder="/mnt/jobs"
                          onCommit={(v) => {
                            settingsStore.setSettings("pathMappings", i(), "lin", v);
                            settingsStore.save();
                          }}
                        />
                        <button
                          class="pm-delete-btn"
                          onClick={() => {
                            const updated = settingsStore.settings.pathMappings.filter((_, idx) => idx !== i());
                            settingsStore.setSettings("pathMappings", updated);
                            settingsStore.save();
                          }}
                          title="Remove mapping"
                        >
                          <span class="icon">close</span>
                        </button>
                      </div>
                    )}
                  </For>
                </div>

                <button
                  class="settings-btn"
                  style={{ "margin-top": "var(--spacing-md)" }}
                  onClick={() => {
                    const newMapping: PathMapping = { win: "", mac: "", lin: "" };
                    settingsStore.setSettings("pathMappings", [
                      ...settingsStore.settings.pathMappings,
                      newMapping,
                    ]);
                  }}
                >
                  + Add Mapping
                </button>

                <div class="path-mappings-example">
                  <p class="settings-hint" style={{ "margin-top": "var(--spacing-lg)" }}>
                    <strong>Example:</strong> If your job files are at <code>Z:\union-jobs</code> on Windows
                    and <code>/Volumes/union-jobs</code> on macOS, add a row with those two values.
                    When someone on macOS clicks a ufb:/// link created on Windows, the path prefix is automatically swapped.
                  </p>
                </div>
              </div>
            </Show>

            {/* ── Sync ── */}
            <Show when={activeSection() === "sync"}>
              <div class="settings-section">
                <h3>Mesh Sync</h3>

                {/* Enable toggle + status */}
                <div class="settings-row">
                  <label class="settings-toggle">
                    <input
                      type="checkbox"
                      checked={settingsStore.settings.sync.enabled}
                      onChange={toggleSyncEnabled}
                    />
                    <span>Enable Sync</span>
                  </label>
                  <Show when={!isMeshConfigured()}>
                    <span class="settings-warning">(Not configured)</span>
                  </Show>
                </div>

                {/* Status panel */}
                <div class="mesh-status-panel">
                  <div class="mesh-status-row">
                    <span class="mesh-status-label">Status:</span>
                    <span style={{ color: statusLabel(meshStatus())?.color }}>
                      {statusLabel(meshStatus())?.text}
                    </span>
                  </div>
                  <Show when={meshStatus()?.statusMessage}>
                    <div class="mesh-status-row">
                      <span class="mesh-status-label" />
                      <span class="mesh-status-msg">{meshStatus()!.statusMessage}</span>
                    </div>
                  </Show>
                  <Show when={meshStatus()?.isEnabled && meshStatus()?.isConfigured}>
                    <div class="mesh-status-row">
                      <span class="mesh-status-label">Leader:</span>
                      <span>{meshStatus()!.leaderId || "Unknown"}</span>
                    </div>
                    <div class="mesh-status-row">
                      <span class="mesh-status-label">Peers online:</span>
                      <span>{meshStatus()!.peerCount}</span>
                    </div>
                    <div class="mesh-status-row">
                      <span class="mesh-status-label">Last snapshot:</span>
                      <span>{formatTimestamp(meshStatus()!.lastSnapshotTime)}</span>
                    </div>
                    <div class="mesh-status-row">
                      <span class="mesh-status-label">Pending edits:</span>
                      <span class={meshStatus()!.pendingEditsCount > 0 ? "mesh-pending" : ""}>
                        {meshStatus()!.pendingEditsCount}
                      </span>
                    </div>
                  </Show>
                  <div class="mesh-status-actions">
                    <button
                      class="settings-btn"
                      onClick={handleSyncNow}
                      disabled={!settingsStore.settings.sync.enabled || !isMeshConfigured()}
                    >
                      Sync Now
                    </button>
                    <span class="settings-hint-inline">Flush pending edits and force a snapshot (if leader).</span>
                  </div>
                </div>

                {/* Configuration */}
                <h3>Configuration</h3>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>Node ID</span>
                    <span class="settings-help" title="Unique identifier for this machine on the mesh network. Defaults to the computer name.">?</span>
                  </div>
                  <SettingsInput
                    value={settingsStore.settings.meshSync.nodeId}
                    placeholder="(auto: computer name)"
                    onCommit={(v) => {
                      settingsStore.setSettings("meshSync", "nodeId", v);
                      settingsStore.save();
                    }}
                  />
                </label>

                <div class="settings-field">
                  <div class="settings-field-header">
                    <span>Farm Path</span>
                    <span class="settings-help" title="Shared folder accessible by all nodes (e.g., UNC path to network share). Used for peer discovery and snapshot storage.">?</span>
                  </div>
                  <div class="settings-field-row">
                    <SettingsInput
                      value={settingsStore.settings.meshSync.farmPath}
                      placeholder="\\\\server\\share\\ufb-sync"
                      onCommit={(v) => {
                        settingsStore.setSettings("meshSync", "farmPath", v);
                        settingsStore.save();
                      }}
                    />
                    <button class="settings-btn" onClick={browseFarmPath}>Browse...</button>
                  </div>
                </div>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>HTTP Port</span>
                    <span class="settings-help" title="TCP port for the mesh sync HTTP server. Must be the same on all nodes. Default: 49200.">?</span>
                  </div>
                  <SettingsInput
                    type="number"
                    value={settingsStore.settings.meshSync.httpPort}
                    min={1024}
                    max={65535}
                    onCommit={(v) => {
                      const val = parseInt(v);
                      if (val >= 1024 && val <= 65535) {
                        settingsStore.setSettings("meshSync", "httpPort", val);
                        settingsStore.save();
                      }
                    }}
                  />
                </label>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>Tags</span>
                    <span class="settings-help" title="Comma-separated tags for leader election. 'leader' — prefer this node as leader, 'noleader' — never elect this node. Leave empty for default.">?</span>
                  </div>
                  <SettingsInput
                    value={settingsStore.settings.meshSync.tags}
                    placeholder="e.g. leader, noleader"
                    onCommit={(v) => {
                      settingsStore.setSettings("meshSync", "tags", v);
                      settingsStore.save();
                    }}
                  />
                </label>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>API Secret</span>
                    <span class="settings-help" title="Shared bearer token for HTTP authentication. Must be the same on all nodes.">?</span>
                  </div>
                  <SettingsInput
                    type="password"
                    value={settingsStore.settings.meshSync.apiSecret}
                    placeholder="Shared secret"
                    onCommit={(v) => {
                      settingsStore.setSettings("meshSync", "apiSecret", v);
                      settingsStore.save();
                    }}
                  />
                </label>

                <Show when={!isMeshConfigured()}>
                  <div class="mesh-setup-hint">
                    <p class="mesh-hint-warn">Fill in the configuration above to enable mesh sync.</p>
                    <p class="mesh-hint-info">All nodes on the network must use the same Farm Path and API Secret.</p>
                  </div>
                </Show>

                <div class="settings-restart-row">
                  <button class="settings-btn settings-btn-primary" onClick={saveAndRestart}>
                    Save &amp; Restart
                  </button>
                  <span class="settings-hint-inline">Sync configuration changes require a restart to take effect.</span>
                </div>
              </div>
            </Show>

            {/* ── Integrations ── */}
            <Show when={activeSection() === "integrations"}>
              <div class="settings-section">
                <h3>Project Notes (Google Drive)</h3>
                <p class="settings-hint">Configure Google Apps Script integration for project notes.</p>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>Script URL</span>
                    <span class="settings-help" title="The deployed Google Apps Script web app URL. This is the URL that creates/opens project note documents.">?</span>
                  </div>
                  <SettingsInput
                    value={settingsStore.settings.googleDrive.scriptUrl}
                    placeholder="https://script.google.com/macros/s/..."
                    onCommit={(v) => {
                      settingsStore.setSettings("googleDrive", "scriptUrl", v);
                      settingsStore.save();
                    }}
                  />
                </label>

                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>Parent Folder ID</span>
                    <span class="settings-help" title="The Google Drive folder ID where project note folders are created. Find this in the folder's URL: drive.google.com/drive/folders/<ID>">?</span>
                  </div>
                  <SettingsInput
                    value={settingsStore.settings.googleDrive.parentFolderId}
                    placeholder="Google Drive folder ID"
                    onCommit={(v) => {
                      settingsStore.setSettings("googleDrive", "parentFolderId", v);
                      settingsStore.save();
                    }}
                  />
                </label>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </div>
  );
}
