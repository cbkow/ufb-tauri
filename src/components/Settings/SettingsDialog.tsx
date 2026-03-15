import { createSignal, createResource, For, Show, onCleanup, onMount } from "solid-js";
import { settingsStore, ACCENT_COLORS } from "../../stores/settingsStore";
import { getMeshStatus, setMeshEnabled, triggerFlushEdits, triggerSnapshot, pickFolder, relaunchApp, mountStoreCredentials, mountHasCredentials, mountDeleteCredentials, mountHideDrives, mountUnhideDrives } from "../../lib/tauri";
import { mountStore, type MountStateUpdate, type MountConfig, type MountsConfig } from "../../stores/mountStore";
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

/** Credential editor for a mount — lets users store/check/clear NAS credentials. */
function MountCredentialEditor(props: { credentialKey: string }) {
  const [hasStored, setHasStored] = createSignal<boolean | null>(null);
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [status, setStatus] = createSignal("");

  const checkStored = async () => {
    if (!props.credentialKey) {
      setHasStored(null);
      return;
    }
    try {
      const result = await mountHasCredentials(props.credentialKey);
      setHasStored(result);
    } catch {
      setHasStored(false);
    }
  };

  onMount(() => checkStored());

  // Re-check when key changes
  const prevKey = { v: props.credentialKey };
  // Use a simple check approach
  const checkKeyChange = () => {
    if (props.credentialKey !== prevKey.v) {
      prevKey.v = props.credentialKey;
      checkStored();
    }
  };

  return (
    <div class="mount-credentials-section">
      <div class="mount-credentials-status">
        {checkKeyChange()}
        <Show when={!props.credentialKey}>
          <span class="settings-help-text">Set a credential key above to manage credentials</span>
        </Show>
        <Show when={props.credentialKey}>
          <Show when={hasStored() === true}>
            <span class="mount-cred-badge mount-cred-stored">Credentials stored</span>
            <button
              class="settings-btn settings-btn-small settings-btn-danger"
              onClick={async () => {
                try {
                  await mountDeleteCredentials(props.credentialKey);
                  setHasStored(false);
                  setStatus("Credentials removed");
                } catch (e) {
                  setStatus("Failed to remove: " + e);
                }
              }}
            >Remove</button>
          </Show>
          <Show when={hasStored() === false}>
            <span class="mount-cred-badge mount-cred-missing">No credentials stored</span>
          </Show>
        </Show>
      </div>
      <Show when={props.credentialKey && hasStored() !== true}>
        <div class="mount-editor-row">
          <label class="settings-field" style={{ flex: 1 }}>
            <span>Username</span>
            <input
              type="text"
              value={username()}
              placeholder="admin"
              onInput={(e) => setUsername(e.currentTarget.value)}
            />
          </label>
          <label class="settings-field" style={{ flex: 1 }}>
            <span>Password</span>
            <input
              type="password"
              value={password()}
              placeholder="password"
              onInput={(e) => setPassword(e.currentTarget.value)}
            />
          </label>
        </div>
        <button
          class="settings-btn"
          disabled={!username() || !password()}
          onClick={async () => {
            try {
              await mountStoreCredentials(props.credentialKey, username(), password());
              setHasStored(true);
              setUsername("");
              setPassword("");
              setStatus("Credentials saved");
            } catch (e) {
              setStatus("Failed to save: " + e);
            }
          }}
        >Save Credentials</button>
      </Show>
      <Show when={status()}>
        <span class="settings-help-text">{status()}</span>
      </Show>
    </div>
  );
}

export function SettingsDialog(props: SettingsDialogProps) {
  const [activeSection, setActiveSection] = createSignal("appearance");

  const sections = [
    { id: "appearance", label: "Appearance" },
    { id: "paths", label: "Path Mappings" },
    { id: "sync", label: "Sync" },
    { id: "mounts", label: "Mounts" },
    { id: "integrations", label: "Integrations" },
  ];

  // Mount state
  const [mountConfig, setMountConfig] = createSignal<MountsConfig>({ version: 1, mounts: [] });

  onMount(async () => {
    mountStore.loadStates();
    const cfg = await mountStore.loadConfig();
    setMountConfig(cfg);
  });

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

            {/* ── Mounts ── */}
            <Show when={activeSection() === "mounts"}>
              <MountsSection mountConfig={mountConfig} setMountConfig={setMountConfig} />
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

/** Default values for a new mount config */
function defaultMountConfig(): MountConfig {
  return {
    id: "",
    enabled: true,
    displayName: "",
    nasSharePath: "",
    credentialKey: "",
    rcloneDriveLetter: "R",
    smbDriveLetter: "",
    mountDriveLetter: "",
    cacheDirPath: "",
    cacheMaxSize: "1T",
    cacheMaxAge: "72h",
    vfsWriteBack: "10s",
    vfsReadChunkSize: "64M",
    vfsReadChunkStreams: 8,
    vfsReadAhead: "2G",
    bufferSize: "512M",
    probeIntervalSecs: 15,
    probeTimeoutMs: 3000,
    fallbackThreshold: 3,
    recoveryThreshold: 5,
    maxRcloneStartAttempts: 3,
    healthcheckFileName: ".healthcheck",
    extraRcloneFlags: [],
  };
}

function MountsSection(props: {
  mountConfig: () => MountsConfig;
  setMountConfig: (cfg: MountsConfig) => void;
}) {
  const [editingMount, setEditingMount] = createSignal<MountConfig | null>(null);
  const [isNew, setIsNew] = createSignal(false);
  const [confirmRemove, setConfirmRemove] = createSignal<string | null>(null);

  function startAdd() {
    setEditingMount(defaultMountConfig());
    setIsNew(true);
  }

  function startEdit(cfg: MountConfig) {
    setEditingMount({ ...cfg });
    setIsNew(false);
  }

  function cancelEdit() {
    setEditingMount(null);
  }

  async function saveMount() {
    const m = editingMount();
    if (!m || !m.id.trim() || !m.nasSharePath.trim()) return;

    const cfg = props.mountConfig();
    let mounts: MountConfig[];
    if (isNew()) {
      mounts = [...cfg.mounts, m];
    } else {
      mounts = cfg.mounts.map((existing) => (existing.id === m.id ? m : existing));
    }
    const newCfg = { ...cfg, mounts };
    props.setMountConfig(newCfg);
    await mountStore.saveConfig(newCfg);
    setEditingMount(null);
  }

  async function removeMount(id: string) {
    const cfg = props.mountConfig();
    const newCfg = { ...cfg, mounts: cfg.mounts.filter((m) => m.id !== id) };
    props.setMountConfig(newCfg);
    await mountStore.saveConfig(newCfg);
    setConfirmRemove(null);
  }

  function updateField<K extends keyof MountConfig>(key: K, value: MountConfig[K]) {
    const m = editingMount();
    if (m) setEditingMount({ ...m, [key]: value });
  }

  return (
    <div class="settings-section">
      <h3>MediaMount Agent</h3>
      <div class="settings-field">
        <div class={`mount-connection-status ${mountStore.connected ? "connected" : "disconnected"}`}>
          <span class="icon" style={{ "font-size": "14px" }}>
            {mountStore.connected ? "check_circle" : "cancel"}
          </span>
          <span>{mountStore.connected ? "Connected to agent" : "Agent not connected"}</span>
        </div>
      </div>

      {/* Explorer drive visibility */}
      <Show when={props.mountConfig().mounts.length > 0}>
        <div class="settings-field">
          <div class="settings-field-header">
            <span>Explorer Drive Visibility</span>
            <span class="settings-help" title="Hide mount-related drive letters from Explorer's 'This PC' view. Drives remain accessible by path. Requires admin (UAC prompt). Restart Explorer after changing.">?</span>
          </div>
          <div class="mount-controls" style={{ gap: "var(--spacing-sm)" }}>
            <button
              class="settings-btn"
              onClick={() => {
                const letters: string[] = [];
                for (const m of props.mountConfig().mounts) {
                  if (m.rcloneDriveLetter) letters.push(m.rcloneDriveLetter);
                  if (m.mountDriveLetter) letters.push(m.mountDriveLetter);
                }
                if (letters.length > 0) mountHideDrives(letters);
              }}
            >Hide Drives</button>
            <button
              class="settings-btn"
              onClick={() => {
                const letters: string[] = [];
                for (const m of props.mountConfig().mounts) {
                  if (m.rcloneDriveLetter) letters.push(m.rcloneDriveLetter);
                  if (m.mountDriveLetter) letters.push(m.mountDriveLetter);
                }
                if (letters.length > 0) mountUnhideDrives(letters);
              }}
            >Show Drives</button>
            <span class="settings-hint-inline">Restart Explorer after changing.</span>
          </div>
        </div>
      </Show>

      {/* Live mount status */}
      <Show when={Object.keys(mountStore.states).length > 0}>
        <h3>Live Status</h3>
        <For each={Object.values(mountStore.states) as MountStateUpdate[]}>
          {(ms) => {
            const cachePercent = () =>
              ms.cacheMaxBytes > 0
                ? Math.round((ms.cacheUsedBytes / ms.cacheMaxBytes) * 100)
                : 0;
            const cacheUsedGB = () => (ms.cacheUsedBytes / (1024 * 1024 * 1024)).toFixed(1);
            const cacheMaxGB = () => (ms.cacheMaxBytes / (1024 * 1024 * 1024)).toFixed(0);

            return (
              <div class="mount-item">
                <div class="mount-item-header">
                  <span class={`mount-state-dot ${ms.isRcloneActive ? "healthy" : ms.isSmbActive ? "fallback" : ms.state === "error" ? "error" : "neutral"}`} />
                  <span class="mount-item-name">{ms.mountId}</span>
                  <span class="mount-item-state">{ms.stateDetail}</span>
                </div>
                <div class="mount-cache-bar">
                  <div class="mount-cache-fill" style={{ width: `${cachePercent()}%` }} />
                </div>
                <div class="mount-cache-label">
                  Cache: {cacheUsedGB()} GB / {cacheMaxGB()} GB
                  <Show when={ms.dirtyFiles > 0}>
                    <span class="mount-dirty"> | {ms.dirtyFiles} dirty</span>
                  </Show>
                </div>
                <div class="mount-controls">
                  <button onClick={() => mountStore.restart(ms.mountId)}>Restart</button>
                  <button onClick={() => mountStore.flushAndRestart(ms.mountId)}>Flush & Restart</button>
                  <Show when={ms.isRcloneActive}>
                    <button onClick={() => mountStore.switchToSmb(ms.mountId)}>Switch to SMB</button>
                  </Show>
                  <Show when={ms.isSmbActive}>
                    <button onClick={() => mountStore.forceRclone(ms.mountId)}>Force rclone</button>
                  </Show>
                </div>
              </div>
            );
          }}
        </For>
      </Show>

      {/* Mount configurations */}
      <h3>Configuration</h3>
      <Show when={!editingMount()}>
        <For each={props.mountConfig().mounts}>
          {(cfg) => (
            <div class="mount-config-item">
              <div class="mount-config-header">
                <span class={`mount-state-dot ${cfg.enabled ? "healthy" : "neutral"}`} />
                <span class="mount-config-name">{cfg.displayName || cfg.id}</span>
                <span class="mount-config-detail">{cfg.nasSharePath} → {cfg.mountDriveLetter ? cfg.mountDriveLetter + ":\\" : "(not set)"}</span>
              </div>
              <div class="mount-config-actions">
                <button class="settings-btn" onClick={() => startEdit(cfg)}>Edit</button>
                <button class="settings-btn" onClick={() => setConfirmRemove(cfg.id)}>Remove</button>
              </div>
            </div>
          )}
        </For>
        <Show when={props.mountConfig().mounts.length === 0}>
          <p class="settings-hint">No mounts configured. Add a mount to manage NAS access with rclone VFS caching.</p>
        </Show>
        <button
          class="settings-btn"
          style={{ "margin-top": "var(--spacing-md)" }}
          onClick={startAdd}
        >
          + Add Mount
        </button>
      </Show>

      {/* Mount editor */}
      <Show when={editingMount()}>
        {(m) => (
          <div class="mount-editor">
            <div class="mount-editor-title">
              {isNew() ? "Add Mount" : `Edit: ${m().displayName || m().id}`}
            </div>

            <label class="settings-field">
              <span>Mount ID</span>
              <SettingsInput
                value={m().id}
                placeholder="primary-nas"
                onCommit={(v) => updateField("id", v.replace(/\s+/g, "-").toLowerCase())}
              />
            </label>

            <label class="settings-field">
              <span>Display Name</span>
              <SettingsInput
                value={m().displayName}
                placeholder="Studio NAS"
                onCommit={(v) => updateField("displayName", v)}
              />
            </label>

            <div class="settings-row">
              <label class="settings-toggle">
                <input
                  type="checkbox"
                  checked={m().enabled}
                  onChange={(e) => updateField("enabled", e.currentTarget.checked)}
                />
                <span>Enabled</span>
              </label>
            </div>

            <h3>Network</h3>

            <div class="settings-field">
              <div class="settings-field-header">
                <span>NAS Share Path</span>
                <span class="settings-help" title="UNC path to the network share, e.g. \\\\nas\\media">?</span>
              </div>
              <SettingsInput
                value={m().nasSharePath}
                placeholder="\\\\nas\\media"
                onCommit={(v) => updateField("nasSharePath", v)}
              />
            </div>

            <label class="settings-field">
              <div class="settings-field-header">
                <span>Credential Key</span>
                <span class="settings-help" title="Key used to store/retrieve NAS credentials in Windows Credential Manager">?</span>
              </div>
              <SettingsInput
                value={m().credentialKey}
                placeholder="mediamount_primary-nas"
                onCommit={(v) => updateField("credentialKey", v)}
              />
            </label>

            <MountCredentialEditor credentialKey={m().credentialKey} />

            <h3>Drive Letter</h3>

            <label class="settings-field">
              <div class="settings-field-header">
                <span>rclone Drive</span>
                <span class="settings-help" title="Drive letter for the rclone VFS mount">?</span>
              </div>
              <SettingsInput
                value={m().rcloneDriveLetter}
                placeholder="R"
                onCommit={(v) => updateField("rcloneDriveLetter", v.toUpperCase().charAt(0))}
              />
            </label>

            <label class="settings-field">
              <div class="settings-field-header">
                <span>Mount Drive Letter</span>
                <span class="settings-help" title="Drive letter that apps use to access media. Maps to rclone or SMB automatically via DefineDosDevice — no Developer Mode required.">?</span>
              </div>
              <SettingsInput
                value={m().mountDriveLetter}
                placeholder="M"
                onCommit={(v) => updateField("mountDriveLetter", v.toUpperCase().charAt(0))}
              />
            </label>

            <h3>Cache</h3>

            <div class="settings-field">
              <div class="settings-field-header">
                <span>Cache Directory</span>
                <span class="settings-help" title="Local directory for the rclone VFS cache. Use a fast SSD.">?</span>
              </div>
              <div class="settings-field-row">
                <SettingsInput
                  value={m().cacheDirPath}
                  placeholder="D:\\rclone-cache\\primary-nas"
                  onCommit={(v) => updateField("cacheDirPath", v)}
                />
                <button class="settings-btn" onClick={async () => {
                  try {
                    const selected = await pickFolder("Select Cache Directory");
                    if (selected) updateField("cacheDirPath", selected);
                  } catch (e) { console.error(e); }
                }}>Browse...</button>
              </div>
            </div>

            <div class="mount-editor-row">
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Max Size</span>
                <SettingsInput value={m().cacheMaxSize} placeholder="1T" onCommit={(v) => updateField("cacheMaxSize", v)} />
              </label>
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Max Age</span>
                <SettingsInput value={m().cacheMaxAge} placeholder="72h" onCommit={(v) => updateField("cacheMaxAge", v)} />
              </label>
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Write-Back</span>
                <SettingsInput value={m().vfsWriteBack} placeholder="10s" onCommit={(v) => updateField("vfsWriteBack", v)} />
              </label>
            </div>

            <h3>Performance</h3>

            <div class="mount-editor-row">
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Chunk Size</span>
                <SettingsInput value={m().vfsReadChunkSize} placeholder="64M" onCommit={(v) => updateField("vfsReadChunkSize", v)} />
              </label>
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Streams</span>
                <SettingsInput type="number" value={m().vfsReadChunkStreams} onCommit={(v) => updateField("vfsReadChunkStreams", parseInt(v) || 8)} />
              </label>
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Read Ahead</span>
                <SettingsInput value={m().vfsReadAhead} placeholder="2G" onCommit={(v) => updateField("vfsReadAhead", v)} />
              </label>
              <label class="settings-field" style={{ flex: 1 }}>
                <span>Buffer</span>
                <SettingsInput value={m().bufferSize} placeholder="512M" onCommit={(v) => updateField("bufferSize", v)} />
              </label>
            </div>

            <div class="mount-editor-actions">
              <button class="settings-btn" onClick={cancelEdit}>Cancel</button>
              <button
                class="settings-btn settings-btn-primary"
                onClick={saveMount}
                disabled={!m().id.trim() || !m().nasSharePath.trim()}
              >
                {isNew() ? "Add" : "Save"}
              </button>
            </div>
          </div>
        )}
      </Show>

      {/* Confirm remove dialog */}
      <Show when={confirmRemove()}>
        <div class="modal-overlay">
          <div class="modal">
            <div class="modal-title">Remove Mount</div>
            <div class="modal-body">
              <p>Remove mount <strong>{confirmRemove()}</strong>? The agent will stop managing this mount.</p>
            </div>
            <div class="modal-actions">
              <button class="modal-btn" onClick={() => setConfirmRemove(null)}>Cancel</button>
              <button class="modal-btn modal-btn-danger" onClick={() => removeMount(confirmRemove()!)}>Remove</button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
