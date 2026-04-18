import { createSignal, createResource, createMemo, createEffect, For, Show, onCleanup, onMount } from "solid-js";
import { settingsStore, ACCENT_COLORS } from "../../stores/settingsStore";
import { getMeshStatus, setMeshEnabled, triggerFlushEdits, triggerSnapshot, reinitMeshSync, pickFolder, relaunchApp, mountStoreCredentials, mountHasCredentials, mountDeleteCredentials, mountListCredentialKeys, mountHideDrives, mountUnhideDrives, getPlatform, mountSmbShare, revealInFileManager } from "../../lib/tauri";
import type { CredentialInfo } from "../../lib/tauri";
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

/** Inline credential form for adding or editing a credential. */
function InlineCredentialForm(props: {
  initialName?: string;
  initialUsername?: string;
  nameReadOnly?: boolean;
  onSave: (name: string, username: string, password: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = createSignal(props.initialName ?? "");
  const [username, setUsername] = createSignal(props.initialUsername ?? "");
  const [password, setPassword] = createSignal("");

  return (
    <div class="mount-editor" style={{ "margin-top": "var(--spacing-sm)" }}>
      <label class="settings-field">
        <span>Credential Name</span>
        <input
          type="text"
          value={name()}
          placeholder="primary-nas"
          disabled={props.nameReadOnly}
          onInput={(e) => setName(e.currentTarget.value.replace(/\s+/g, "-").toLowerCase())}
        />
      </label>
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
      <div class="mount-editor-actions">
        <button class="settings-btn" onClick={props.onCancel}>Cancel</button>
        <button
          class="settings-btn settings-btn-primary"
          disabled={!name() || !username() || !password()}
          onClick={() => props.onSave(name(), username(), password())}
        >Save</button>
      </div>
    </div>
  );
}

export function SettingsDialog(props: SettingsDialogProps) {
  const [activeSection, setActiveSection] = createSignal("appearance");
  const [platform, setPlatform] = createSignal("win");

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
    try { setPlatform(await getPlatform()); } catch {}
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

  async function saveAndApplyMesh() {
    await settingsStore.save();
    try {
      await reinitMeshSync();
      setMeshStatusTick((n) => n + 1);
    } catch (e) {
      console.error("Failed to reinit mesh sync:", e);
    }
  }

  const [showMountDialog, setShowMountDialog] = createSignal(false);
  const [mountHost, setMountHost] = createSignal("");
  const [mountShare, setMountShare] = createSignal("");
  const [mountUser, setMountUser] = createSignal("");
  const [mountPass, setMountPass] = createSignal("");
  const [mountStatus, setMountStatus] = createSignal("");
  const [mounting, setMounting] = createSignal(false);

  async function doSmbMount() {
    if (!mountHost() || !mountShare()) return;
    setMounting(true);
    setMountStatus("Mounting... (you may see a password prompt for sudo)");
    try {
      const localPath = await mountSmbShare(mountHost(), mountShare(), mountUser(), mountPass());
      settingsStore.setSettings("meshSync", "farmPath", localPath);
      settingsStore.save();
      setShowMountDialog(false);
      setMountStatus("");
      setMountPass("");
    } catch (e) {
      setMountStatus("Failed: " + e);
    } finally {
      setMounting(false);
    }
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
                    <span class="pm-toggle-cell pm-header" />
                    <span class="pm-cell pm-label-cell pm-header">Label</span>
                    <span class="pm-cell pm-header">Windows</span>
                    <span class="pm-cell pm-header">macOS</span>
                    <span class="pm-cell pm-header">Linux</span>
                    <span class="pm-actions-cell" />
                  </div>
                  <For each={settingsStore.settings.pathMappings}>
                    {(mapping, i) => (
                      <div class={`path-mappings-row ${!mapping.enabled ? "pm-row-disabled" : ""}`}>
                        <input
                          type="checkbox"
                          class="pm-toggle"
                          checked={mapping.enabled}
                          onChange={(e) => {
                            settingsStore.setSettings("pathMappings", i(), "enabled", e.currentTarget.checked);
                            settingsStore.save();
                          }}
                        />
                        <SettingsInput
                          class="pm-cell pm-label-input"
                          value={mapping.label}
                          placeholder="e.g., Office"
                          onCommit={(v) => {
                            settingsStore.setSettings("pathMappings", i(), "label", v);
                            settingsStore.save();
                          }}
                        />
                        <SettingsInput
                          class="pm-cell pm-input"
                          value={mapping.win}
                          placeholder="Z:\jobs"
                          onCommit={(v) => {
                            settingsStore.setSettings("pathMappings", i(), "win", v);
                            settingsStore.save();
                          }}
                        />
                        <SettingsInput
                          class="pm-cell pm-input"
                          value={mapping.mac}
                          placeholder="~/ufb/mounts/nas"
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
                    const newMapping: PathMapping = { win: "", mac: "", lin: "", enabled: true, label: "" };
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
                    <span class="settings-help" title="Shared folder accessible by all nodes. On Windows use a UNC path (e.g. \\\\server\\share\\ufb-sync). On Linux use a local mount path.">?</span>
                  </div>
                  <div class="settings-field-row">
                    <SettingsInput
                      value={settingsStore.settings.meshSync.farmPath}
                      placeholder={platform() === "lin" ? "/mnt/nas/ufb-sync" : "\\\\server\\share\\ufb-sync"}
                      onCommit={(v) => {
                        settingsStore.setSettings("meshSync", "farmPath", v);
                        settingsStore.save();
                      }}
                    />
                    <button class="settings-btn" onClick={browseFarmPath}>Browse...</button>
                    <Show when={platform() === "lin"}>
                      <button class="settings-btn" onClick={() => setShowMountDialog(true)}>Mount SMB...</button>
                    </Show>
                  </div>
                  <Show when={platform() === "lin" && settingsStore.settings.meshSync.farmPath.startsWith("smb://")}>
                    <span class="settings-hint" style={{ color: "var(--warning)" }}>
                      Farm path should be a local mount path, not an smb:// URL. Use "Mount SMB..." to mount the share first.
                    </span>
                  </Show>
                </div>

                {/* SMB Mount dialog */}
                <Show when={showMountDialog()}>
                  <div class="mount-editor" style={{ "margin-top": "var(--spacing-md)" }}>
                    <div class="mount-editor-title">Mount SMB Share</div>
                    <p class="settings-hint">Mount a network share to a local path. You'll be prompted for your system password.</p>

                    <div class="mount-editor-row">
                      <label class="settings-field" style={{ flex: 2 }}>
                        <span>Host / IP</span>
                        <input
                          type="text"
                          value={mountHost()}
                          placeholder="192.168.40.100"
                          onInput={(e) => setMountHost(e.currentTarget.value)}
                        />
                      </label>
                      <label class="settings-field" style={{ flex: 1 }}>
                        <span>Share Name</span>
                        <input
                          type="text"
                          value={mountShare()}
                          placeholder="MinRender"
                          onInput={(e) => setMountShare(e.currentTarget.value)}
                        />
                      </label>
                    </div>

                    <div class="mount-editor-row">
                      <label class="settings-field" style={{ flex: 1 }}>
                        <span>Username</span>
                        <input
                          type="text"
                          value={mountUser()}
                          placeholder="(optional)"
                          onInput={(e) => setMountUser(e.currentTarget.value)}
                        />
                      </label>
                      <label class="settings-field" style={{ flex: 1 }}>
                        <span>Password</span>
                        <input
                          type="password"
                          value={mountPass()}
                          placeholder="(optional)"
                          onInput={(e) => setMountPass(e.currentTarget.value)}
                        />
                      </label>
                    </div>

                    <Show when={mountStatus()}>
                      <p class="settings-hint" style={{ color: mountStatus().startsWith("Failed") ? "var(--warning)" : undefined }}>
                        {mountStatus()}
                      </p>
                    </Show>

                    <div class="mount-editor-actions">
                      <button class="settings-btn" onClick={() => { setShowMountDialog(false); setMountStatus(""); setMountPass(""); }}>Cancel</button>
                      <button
                        class="settings-btn settings-btn-primary"
                        onClick={doSmbMount}
                        disabled={!mountHost() || !mountShare() || mounting()}
                      >
                        Mount
                      </button>
                    </div>

                    <p class="settings-hint" style={{ "margin-top": "var(--spacing-sm)" }}>
                      Mounts to <code>/media/$USER/ufb/{mountShare() || "<share>"}/</code>. Requires <code>cifs-utils</code> package.
                    </p>
                  </div>
                </Show>

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
                  <button class="settings-btn settings-btn-primary" onClick={saveAndApplyMesh}>
                    Save &amp; Apply
                  </button>
                  <span class="settings-hint-inline">Sync configuration changes require a restart to take effect.</span>
                </div>
              </div>
            </Show>

            {/* ── Mounts ── */}
            <Show when={activeSection() === "mounts"}>
              <MountsSection mountConfig={mountConfig} setMountConfig={setMountConfig} platform={platform} />
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
    mountDriveLetter: "",
    smbMountPath: "",
    mountPathLinux: "",
    mountPathMacos: "",
    isJobsFolder: true,
    syncEnabled: false,
  };
}

function MountsSection(props: {
  mountConfig: () => MountsConfig;
  setMountConfig: (cfg: MountsConfig) => void;
  platform: () => string;
}) {
  const [editingMount, setEditingMount] = createSignal<MountConfig | null>(null);
  const [isNew, setIsNew] = createSignal(false);
  const [confirmRemove, setConfirmRemove] = createSignal<string | null>(null);
  const [confirmDrain, setConfirmDrain] = createSignal<{ id: string; displayName: string } | null>(null);

  // Refresh cache stats whenever we open/switch to a sync-enabled mount.
  // Agent replies via the `mount:cache-stats` event and mountStore receives it.
  createEffect(() => {
    const mount = editingMount();
    if (mount && mount.syncEnabled) {
      mountStore.refreshCacheStats(mount.id);
    }
  });

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const units = ["KB", "MB", "GB", "TB"];
    let v = n / 1024;
    for (const u of units) {
      if (v < 1024) return `${v.toFixed(v < 10 ? 1 : 0)} ${u}`;
      v /= 1024;
    }
    return `${v.toFixed(1)} PB`;
  }

  // ── Saved credentials ──
  const [savedCreds, setSavedCreds] = createSignal<CredentialInfo[]>([]);
  const [credFormMode, setCredFormMode] = createSignal<"add" | "edit" | null>(null);
  const [editingCredKey, setEditingCredKey] = createSignal<string>("");
  const [editingCredUsername, setEditingCredUsername] = createSignal<string>("");
  const [confirmDeleteCred, setConfirmDeleteCred] = createSignal<string | null>(null);
  const [credSectionOpen, setCredSectionOpen] = createSignal(false);

  // ── Inline credential form from mount editor (+ New credential...) ──
  const [showInlineCred, setShowInlineCred] = createSignal(false);

  async function refreshCreds() {
    try {
      const creds = await mountListCredentialKeys();
      setSavedCreds(creds);
    } catch (e) {
      console.error("Failed to list credentials:", e);
    }
  }

  onMount(() => refreshCreds());

  function startAdd() {
    setEditingMount(defaultMountConfig());
    setIsNew(true);
    setShowInlineCred(false);
  }

  function startEdit(cfg: MountConfig) {
    setEditingMount({ ...cfg });
    setIsNew(false);
    setShowInlineCred(false);
  }

  function cancelEdit() {
    setEditingMount(null);
    setShowInlineCred(false);
  }

  // ── Validation ──
  const saveDisabled = createMemo(() => {
    const m = editingMount();
    if (!m) return true;
    if (!m.id.trim()) return true;
    if (!m.nasSharePath.trim()) return true;
    // Duplicate ID check (only when adding new)
    if (isNew() && props.mountConfig().mounts.some((e) => e.id === m.id)) return true;
    return false;
  });

  const saveHint = createMemo(() => {
    const m = editingMount();
    if (!m) return "";
    if (!m.id.trim()) return "Mount ID is required";
    if (!m.nasSharePath.trim()) return "NAS share path is required";
    if (isNew() && props.mountConfig().mounts.some((e) => e.id === m.id))
      return "A mount with this ID already exists";
    return "";
  });

  async function saveMount() {
    if (saveDisabled()) return;
    const m = editingMount()!;

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
    setShowInlineCred(false);
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
    if (!m) return;
    const updated = { ...m, [key]: value };
    // Auto-select credential when mount ID changes and no credential is selected
    if (key === "id" && !m.credentialKey) {
      const matchingCred = savedCreds().find((c) => c.key === value);
      if (matchingCred) {
        updated.credentialKey = matchingCred.key;
      }
    }
    setEditingMount(updated);
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

      {/* ── Saved Credentials ── (hidden on macOS — Keychain handles it) */}
      <Show when={props.platform() === "mac"}>
        <h3>Credentials</h3>
        <p class="settings-hint" style={{ "margin-bottom": "var(--spacing-sm)" }}>
          UFB uses the macOS login Keychain for SMB credentials. On first connection
          to each share, macOS shows a dialog — check "Remember this password in my
          keychain" and subsequent mounts are silent. Manage saved SMB credentials in
          the built-in <strong>Keychain Access</strong> app (search for <em>smb:</em>).
        </p>
      </Show>

      <Show when={props.platform() !== "mac"}>
        <details open={credSectionOpen()} onToggle={(e) => setCredSectionOpen((e.target as HTMLDetailsElement).open)}>
          <summary><h3 style={{ display: "inline", cursor: "pointer" }}>Saved Credentials</h3></summary>
          <div style={{ "margin-top": "var(--spacing-sm)" }}>
            <Show when={savedCreds().length > 0}>
              <For each={savedCreds()}>
                {(cred) => (
                  <div class="mount-config-item">
                    <div class="mount-config-header">
                      <span class="mount-config-name">{cred.key}</span>
                      <span class="mount-config-detail">{cred.username}</span>
                    </div>
                    <div class="mount-config-actions">
                      <button class="settings-btn" onClick={() => {
                        setCredFormMode("edit");
                        setEditingCredKey(cred.key);
                        setEditingCredUsername(cred.username);
                      }}>Edit</button>
                      <button class="settings-btn" onClick={() => setConfirmDeleteCred(cred.key)}>Delete</button>
                    </div>
                  </div>
                )}
              </For>
            </Show>
            <Show when={savedCreds().length === 0 && credFormMode() === null}>
              <p class="settings-hint">No saved credentials. Credentials are stored securely and referenced by name in mount configs.</p>
            </Show>

            <Show when={credFormMode() === null}>
              <button
                class="settings-btn"
                style={{ "margin-top": "var(--spacing-sm)" }}
                onClick={() => { setCredFormMode("add"); setEditingCredKey(""); setEditingCredUsername(""); }}
              >+ Add Credential</button>
            </Show>

            <Show when={credFormMode() !== null}>
              <InlineCredentialForm
                initialName={credFormMode() === "edit" ? editingCredKey() : ""}
                initialUsername={credFormMode() === "edit" ? editingCredUsername() : ""}
                nameReadOnly={credFormMode() === "edit"}
                onSave={async (name, username, password) => {
                  try {
                    await mountStoreCredentials(name, username, password);
                    await refreshCreds();
                    setCredFormMode(null);
                  } catch (e) {
                    console.error("Failed to save credential:", e);
                  }
                }}
                onCancel={() => setCredFormMode(null)}
              />
            </Show>
          </div>
        </details>
      </Show>

      {/* Confirm delete credential */}
      <Show when={confirmDeleteCred()}>
        <div class="modal-overlay">
          <div class="modal">
            <div class="modal-title">Delete Credential</div>
            <div class="modal-body">
              <p>Delete credential <strong>{confirmDeleteCred()}</strong>? Any mounts using it will need a new credential.</p>
            </div>
            <div class="modal-actions">
              <button class="modal-btn" onClick={() => setConfirmDeleteCred(null)}>Cancel</button>
              <button class="modal-btn modal-btn-danger" onClick={async () => {
                try {
                  await mountDeleteCredentials(confirmDeleteCred()!);
                  await refreshCreds();
                } catch (e) { console.error("Failed to delete credential:", e); }
                setConfirmDeleteCred(null);
              }}>Delete</button>
            </div>
          </div>
        </div>
      </Show>

      {/* Confirm drain cache */}
      <Show when={confirmDrain()}>
        <div class="modal-overlay">
          <div class="modal">
            <div class="modal-title">Drain Cache</div>
            <div class="modal-body">
              <p>
                Drain all cached files for <strong>{confirmDrain()!.displayName}</strong>?
              </p>
              <p class="settings-hint">
                Non-destructive — files re-download on next access. Any active reads are skipped and drained on the next tick.
              </p>
            </div>
            <div class="modal-actions">
              <button class="modal-btn" onClick={() => setConfirmDrain(null)}>Cancel</button>
              <button class="modal-btn modal-btn-danger" onClick={async () => {
                const entry = confirmDrain();
                setConfirmDrain(null);
                if (entry) {
                  await mountStore.drainShareCache(entry.id);
                  // Agent emits a CacheStats message after drain; the
                  // mount:cache-stats listener picks it up. Request an
                  // explicit refresh too in case the agent's ack-emit
                  // path skipped this mount (non-NFS backend).
                  await mountStore.refreshCacheStats(entry.id);
                }
              }}>Drain</button>
            </div>
          </div>
        </div>
      </Show>

      {/* Sync cache location — global setting for all sync mounts.
          Shown whenever at least one mount has sync enabled; applies to
          both the macOS NFS loopback cache and the Windows WinFsp cache. */}
      <Show when={props.mountConfig().mounts.some((m) => m.syncEnabled)}>
        <h3>Sync Cache</h3>
        <label class="settings-field">
          <div class="settings-field-header">
            <span>Cache Location</span>
            <span class="settings-help" title="Local folder where sync mount metadata and block-cached content is stored. Changing this will tear down all sync mounts, drop cached data, and rebuild at the new location.">?</span>
          </div>
          <div style={{ display: "flex", gap: "var(--spacing-xs)", "align-items": "center" }}>
            <div class="settings-value-display" style={{ flex: 1 }}>
              {props.mountConfig().syncCacheRoot || mountStore.defaultCacheRoot}
            </div>
            <button
              class="settings-btn"
              title="Reveal cache folder in file manager"
              onClick={async () => {
                const path = props.mountConfig().syncCacheRoot || mountStore.defaultCacheRoot;
                if (path) {
                  try {
                    await revealInFileManager(path);
                  } catch (err) {
                    console.error("Failed to open cache folder:", err);
                  }
                }
              }}
            >Open</button>
            <button
              class="settings-btn"
              onClick={async () => {
                const folder = await pickFolder();
                if (folder) {
                  const newCfg = { ...props.mountConfig(), syncCacheRoot: folder };
                  props.setMountConfig(newCfg);
                  await mountStore.saveConfig(newCfg);
                }
              }}
            >Browse</button>
            <Show when={props.mountConfig().syncCacheRoot}>
              <button
                class="settings-btn"
                onClick={async () => {
                  const newCfg = { ...props.mountConfig(), syncCacheRoot: undefined };
                  props.setMountConfig(newCfg);
                  await mountStore.saveConfig(newCfg);
                }}
              >Reset</button>
            </Show>
          </div>
        </label>
      </Show>

      {/* Mount configurations */}
      <h3>Configuration</h3>
      <Show when={!editingMount()}>
        <For each={props.mountConfig().mounts}>
          {(cfg) => (
            <div class={`mount-config-item ${!cfg.enabled ? "mount-config-disabled" : ""}`}>
              <div class="mount-config-header">
                <input
                  type="checkbox"
                  class="mount-enable-toggle"
                  checked={cfg.enabled}
                  onChange={(e) => {
                    const newConfig = {
                      ...props.mountConfig(),
                      mounts: props.mountConfig().mounts.map((m) =>
                        m.id === cfg.id ? { ...m, enabled: e.currentTarget.checked } : m
                      ),
                    };
                    props.setMountConfig(newConfig);
                    mountStore.saveConfig(newConfig);
                  }}
                  title={cfg.enabled ? "Auto-connects on agent start" : "Does not auto-connect"}
                />
                <span class="mount-config-name">{cfg.displayName || cfg.id}</span>
                <span class="mount-config-detail">
                  {cfg.nasSharePath} → {cfg.syncEnabled ? "Sync" : mountStore.getMountPath(cfg) || "(not set)"}
                </span>
              </div>
              <div class="mount-config-actions">
                <button class="settings-btn" onClick={() => startEdit(cfg)}>Edit</button>
                <button class="settings-btn" onClick={() => setConfirmRemove(cfg.id)}>Remove</button>
              </div>
            </div>
          )}
        </For>
        <Show when={props.mountConfig().mounts.length === 0}>
          <p class="settings-hint">No mounts configured. Add a mount to manage SMB NAS access.</p>
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
                <span>Auto-connect on startup</span>
              </label>
            </div>

            <div class="settings-row">
              <label class="settings-toggle">
                <input
                  type="checkbox"
                  checked={m().isJobsFolder}
                  onChange={(e) => updateField("isJobsFolder", e.currentTarget.checked)}
                />
                <span>Jobs Folder</span>
              </label>
              <span class="settings-hint-inline">Subfolders appear as subscribable jobs</span>
            </div>

            {/* On-demand sync toggle — Windows and macOS */}
            <Show when={props.platform() === "win" || props.platform() === "mac"}>
              <div class="settings-row">
                <label class="settings-toggle">
                  <input
                    type="checkbox"
                    checked={m().syncEnabled ?? false}
                    onChange={(e) => {
                      const sync = e.currentTarget.checked;
                      updateField("syncEnabled", sync);
                      if (sync) updateField("mountDriveLetter", "");
                    }}
                  />
                  <span>On-Demand Sync</span>
                </label>
                <span class="settings-hint-inline">Files appear locally as cloud placeholders, downloaded on access</span>
              </div>
              <Show when={m().syncEnabled}>
                <p class="settings-hint" style={{ "margin-top": "0", "margin-bottom": "var(--spacing-sm)" }}>
                  Sync root: {mountStore.getMountPath(m()!) || "(auto)"}
                </p>
                <Show when={props.platform() === "mac"}>
                  <p class="settings-hint" style={{ "margin-top": "0", "margin-bottom": "var(--spacing-sm)", opacity: 0.7 }}>
                    Mounted at ~/ufb/mounts/&lt;share&gt; via NFS loopback. Cache managed by the agent.
                  </p>
                </Show>

                <div class="settings-field">
                  <div class="settings-field-header">
                    <span>Cache Limit</span>
                    <span class="settings-help" title="Maximum local disk space for cached (hydrated) files. Oldest files are evicted when over budget. Set to 0 for unlimited.">?</span>
                  </div>
                  {(() => {
                    const presets = [0, 10e9, 25e9, 50e9, 100e9, 200e9, 500e9];
                    const isCustom = () => {
                      const v = m().syncCacheLimitBytes ?? 0;
                      return !presets.includes(v);
                    };
                    const [showCustom, setShowCustom] = createSignal(isCustom());
                    return (
                      <div style={{ display: "flex", gap: "var(--spacing-sm)", "align-items": "center" }}>
                        <select
                          class="settings-select"
                          value={(() => {
                            const v = m().syncCacheLimitBytes ?? 0;
                            return presets.includes(v) && !showCustom() ? String(v) : "custom";
                          })()}
                          onChange={(e) => {
                            const val = e.currentTarget.value;
                            if (val === "custom") {
                              setShowCustom(true);
                            } else {
                              setShowCustom(false);
                              updateField("syncCacheLimitBytes", Number(val));
                            }
                          }}
                        >
                          <option value="0">Unlimited</option>
                          <option value="10000000000">10 GB</option>
                          <option value="25000000000">25 GB</option>
                          <option value="50000000000">50 GB</option>
                          <option value="100000000000">100 GB</option>
                          <option value="200000000000">200 GB</option>
                          <option value="500000000000">500 GB</option>
                          <option value="custom">Custom...</option>
                        </select>
                        <Show when={showCustom()}>
                          <SettingsInput
                            type="number"
                            value={String(Math.round((m().syncCacheLimitBytes ?? 0) / 1e9))}
                            placeholder="GB"
                            onCommit={(v) => {
                              const gb = parseFloat(v);
                              if (!isNaN(gb) && gb >= 0) {
                                updateField("syncCacheLimitBytes", Math.round(gb * 1e9));
                              }
                            }}
                          />
                          <span class="settings-hint-inline">GB</span>
                        </Show>
                      </div>
                    );
                  })()}
                </div>

                <p class="settings-hint" style={{ "margin-top": "var(--spacing-sm)", "margin-bottom": "0" }}>
                  {(() => {
                    const s = mountStore.cacheStats[m().id];
                    if (!s) return "Cache: (fetching…)";
                    return `Cache: ${fmtBytes(s.hydratedBytes)} across ${s.hydratedCount} file${s.hydratedCount === 1 ? "" : "s"}`;
                  })()}
                </p>
                <button
                  class="settings-btn"
                  style={{ "margin-top": "var(--spacing-sm)" }}
                  onClick={() => {
                    setConfirmDrain({ id: m().id, displayName: m().displayName || m().id });
                  }}
                >Drain Cache</button>
              </Show>
            </Show>

            <h3>Network</h3>

            <div class="settings-field">
              <div class="settings-field-header">
                <span>NAS Share Path</span>
                <span class="settings-help" title="UNC path to the network share, e.g. \\\\nas\\media or \\\\nas\\media\\projects">?</span>
              </div>
              <SettingsInput
                value={m().nasSharePath}
                placeholder="\\\\nas\\media"
                onCommit={(v) => updateField("nasSharePath", v)}
              />
            </div>

            {/* Credential dropdown — hidden on macOS (Keychain handles auth) */}
            <Show when={props.platform() !== "mac"}>
            <div class="settings-field">
              <div class="settings-field-header">
                <span>Credential</span>
                <span class="settings-help" title="Select a saved credential for NAS authentication. Credentials are managed in the Saved Credentials section above.">?</span>
              </div>
              <Show when={!showInlineCred()}>
                <select
                  class="settings-select"
                  value={m().credentialKey}
                  onChange={(e) => {
                    const val = e.currentTarget.value;
                    if (val === "__new__") {
                      setShowInlineCred(true);
                    } else {
                      updateField("credentialKey", val);
                    }
                  }}
                >
                  <option value="">(none)</option>
                  <For each={savedCreds()}>
                    {(cred) => <option value={cred.key}>{cred.key} ({cred.username})</option>}
                  </For>
                  <option value="__new__">+ New credential...</option>
                </select>
              </Show>
              <Show when={showInlineCred()}>
                <InlineCredentialForm
                  initialName={m().id || ""}
                  onSave={async (name, username, password) => {
                    try {
                      await mountStoreCredentials(name, username, password);
                      await refreshCreds();
                      updateField("credentialKey", name);
                      setShowInlineCred(false);
                    } catch (e) {
                      console.error("Failed to save credential:", e);
                    }
                  }}
                  onCancel={() => setShowInlineCred(false)}
                />
              </Show>
            </div>
            </Show>

            {/* Drive letter — Windows only, hidden when sync enabled */}
            <Show when={props.platform() === "win" && !m().syncEnabled}>
              <h3>Volume Path</h3>

              <label class="settings-field">
                <div class="settings-field-header">
                  <span>Mount Path</span>
                  <span class="settings-help" title="Volume symlink path — derived from share name. Consistent across all machines.">?</span>
                </div>
                <div class="settings-value-display">
                  {`C:\\Volumes\\ufb\\${mountStore.getShareName(m())}`}
                </div>
              </label>
            </Show>

            {/* macOS mount path — display for regular mounts, info for sync */}
            <Show when={props.platform() === "mac"}>
              <Show when={!m().syncEnabled}>
                <h3>Mount Path</h3>
                <label class="settings-field">
                  <div class="settings-field-header">
                    <span>Symlink Location</span>
                    <span class="settings-help" title="Stable symlink path that points to the mounted SMB share. Drag to Finder Favorites for quick access.">?</span>
                  </div>
                  <div class="settings-value-display">
                    {mountStore.getMountPath(m()!) || mountStore.getMountPath(m())}
                  </div>
                </label>
                <details style={{ "margin-top": "var(--spacing-sm)" }}>
                  <summary class="settings-hint" style={{ cursor: "pointer" }}>
                    Advanced: override mount path
                  </summary>
                  <label class="settings-field">
                    <span>Custom Mount Path</span>
                    <SettingsInput
                      value={m().mountPathMacos ?? ""}
                      placeholder="(auto)"
                      onCommit={(v) => updateField("mountPathMacos", v)}
                    />
                  </label>
                </details>
              </Show>
            </Show>

            {/* Linux mount path overrides — collapsed by default */}
            <Show when={props.platform() === "lin"}>
              <details style={{ "margin-top": "var(--spacing-sm)" }}>
                <summary class="settings-hint" style={{ cursor: "pointer" }}>
                  Advanced: override auto-derived mount paths
                </summary>
                <p class="settings-hint">
                  Paths are auto-derived from the mount ID (/media/$USER/ufb/). Only set these if you need custom locations.
                </p>

                <label class="settings-field">
                  <span>SMB Mount Path</span>
                  <SettingsInput
                    value={m().smbMountPath ?? ""}
                    placeholder="(auto)"
                    onCommit={(v) => updateField("smbMountPath", v)}
                  />
                </label>

                <label class="settings-field">
                  <span>User-Facing Mount Path</span>
                  <SettingsInput
                    value={m().mountPathLinux ?? ""}
                    placeholder="(auto)"
                    onCommit={(v) => updateField("mountPathLinux", v)}
                  />
                </label>
              </details>
            </Show>

            <Show when={saveHint()}>
              <p class="settings-hint" style={{ color: "var(--warning)" }}>{saveHint()}</p>
            </Show>

            <div class="mount-editor-actions">
              <button class="settings-btn" onClick={cancelEdit}>Cancel</button>
              <button
                class="settings-btn settings-btn-primary"
                onClick={saveMount}
                disabled={saveDisabled()}
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
