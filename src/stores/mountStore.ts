import { createStore, reconcile } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import { platformStore } from "./platformStore";
import {
  mountGetStates,
  mountIsConnected,
  mountRestart,
  mountStart as tauriMountStart,
  mountStop as tauriMountStop,
  mountSaveConfig,
  mountGetConfig,
  mountLaunchAgent,
  mountCreateSymlinks,
} from "../lib/tauri";

export interface MountStateUpdate {
  mountId: string;
  state: string;
  stateDetail: string;
  syncState?: string;
  syncStateDetail?: string;
  needsElevation?: boolean;
}

export interface MountConfig {
  id: string;
  enabled: boolean;
  displayName: string;
  nasSharePath: string;
  credentialKey: string;
  mountDriveLetter: string;
  smbMountPath?: string;
  mountPathLinux?: string;
  mountPathMacos?: string;
  isJobsFolder: boolean;
  syncEnabled?: boolean;
  syncRootPath?: string;
  syncCacheLimitBytes?: number;
}

export interface MountsConfig {
  version: number;
  mounts: MountConfig[];
  syncCacheRoot?: string;
}

interface MountStoreState {
  states: Record<string, MountStateUpdate>;
  connected: boolean;
  configs: MountConfig[];
}

const [state, setState] = createStore<MountStoreState>({
  states: {},
  connected: false,
  configs: [],
});

let listenersSetUp = false;
let listenerSetupPromise: Promise<void> | null = null;

function setupListeners(): Promise<void> {
  if (listenersSetUp) return Promise.resolve();
  if (listenerSetupPromise) return listenerSetupPromise;

  // `listen()` returns a Promise — the subscription isn't live until that
  // resolves. On a cold first launch the agent can emit early state-update
  // events (especially the "mounted" transition) before registration completes,
  // silently dropping them and leaving the UI stuck at "mounting". Await both
  // registrations before anyone asks for a snapshot.
  listenerSetupPromise = (async () => {
    await listen<MountStateUpdate>("mount:state-update", (e) => {
      const update = e.payload;
      console.log("[mountStore] state-update:", update.mountId, update.state, update);
      setState("states", update.mountId, reconcile(update));
    });

    await listen<boolean>("mount:connection", (e) => {
      console.log("[mountStore] connection:", e.payload);
      setState("connected", e.payload);
    });

    listenersSetUp = true;
  })();

  return listenerSetupPromise;
}

/** Get the mount state for a given path via prefix matching against mount paths. */
function getMountForPath(path: string): MountStateUpdate | undefined {
  if (state.configs.length === 0) return undefined;
  const normalized = path.replace(/\//g, "\\").toLowerCase();
  for (const cfg of state.configs) {
    const mountPath = getMountPath(cfg);
    if (!mountPath) continue;
    const mountNormalized = mountPath.replace(/\//g, "\\").toLowerCase();
    const sep = mountNormalized.includes("\\") ? "\\" : "/";
    if (normalized === mountNormalized || normalized.startsWith(mountNormalized + sep)) {
      return state.states[cfg.id];
    }
  }
  return undefined;
}

async function loadStates() {
  console.log("[mountStore] loadStates called");
  await setupListeners();
  try {
    const states = await mountGetStates();
    console.log("[mountStore] initial states:", states);
    setState("states", reconcile(states));
    const connected = await mountIsConnected();
    console.log("[mountStore] connected:", connected);
    setState("connected", connected);
    // Load configs for path matching
    const cfg = await mountGetConfig();
    console.log("[mountStore] configs:", cfg.mounts?.length, cfg.mounts);
    setState("configs", (cfg.mounts ?? []).filter((m) => m.enabled));
  } catch (e) {
    console.error("Failed to load mount states:", e);
  }
}

async function restart(mountId: string) {
  try {
    await mountRestart(mountId);
  } catch (e) {
    console.error("Failed to restart mount:", e);
  }
}

async function start(mountId: string) {
  try {
    await tauriMountStart(mountId);
  } catch (e) {
    console.error("Failed to start mount:", e);
  }
}

async function stop(mountId: string) {
  try {
    await tauriMountStop(mountId);
  } catch (e) {
    console.error("Failed to stop mount:", e);
  }
}

/** Toggle a mount between connected and disconnected. */
function toggleMount(mountId: string) {
  const ms = state.states[mountId];
  if (ms?.state === "mounted" || ms?.state === "mounting" || ms?.state === "initializing") {
    stop(mountId);
  } else {
    start(mountId);
  }
}

async function saveConfig(config: MountsConfig) {
  try {
    await mountSaveConfig(config);
    // Update local configs so bookmarks panel reflects changes immediately
    setState("configs", (config.mounts ?? []).filter((m) => m.enabled));
  } catch (e) {
    console.error("Failed to save mount config:", e);
  }
}

async function launchAgent() {
  try {
    await mountLaunchAgent();
  } catch (e) {
    console.error("Failed to launch agent:", e);
  }
}

async function loadConfig(): Promise<MountsConfig> {
  try {
    return await mountGetConfig();
  } catch (e) {
    console.error("Failed to load mount config:", e);
    return { version: 1, mounts: [] };
  }
}

/** Get the user-facing mount path for a config (platform-aware). */
function getMountPath(cfg: MountConfig): string {
  if (platformStore.platform === "win") {
    // Windows: all mounts use C:\Volumes\ufb\{shareName}
    // Sync mounts may have an explicit override
    if (cfg.syncEnabled && cfg.syncRootPath) return cfg.syncRootPath;
    const shareName = getShareName(cfg);
    return `C:\\Volumes\\ufb\\${shareName}`;
  }
  if (platformStore.platform === "mac") {
    if (cfg.mountPathMacos) return cfg.mountPathMacos;
    const shareName = getShareName(cfg);
    if (cfg.syncEnabled) {
      // Sync mounts use FileProvider — path is ~/Library/CloudStorage/UFB-{displayName}
      const displayName = (cfg.displayName || shareName).replace(/\s+/g, "");
      return `${platformStore.home}/Library/CloudStorage/UFB-${displayName}`;
    }
    return `${platformStore.home}/ufb/mounts/${shareName}`;
  }
  // Linux: explicit path or auto-derived from mount ID
  if (cfg.mountPathLinux) return cfg.mountPathLinux;
  if (cfg.smbMountPath) return cfg.smbMountPath;
  const home = platformStore.home;
  if (home && cfg.id) return `${home}/.local/share/ufb/mnt/${cfg.id}`;
  return "";
}

/** Extract the last component of the NAS path (matches agent's share_name()). */
function getShareName(cfg: MountConfig): string {
  const parts = cfg.nasSharePath.replace(/\\+$/, "").split("\\").filter(Boolean);
  return parts[parts.length - 1] || cfg.id;
}

/** Check if any mount needs elevation for symlink creation. */
function needsElevation(): boolean {
  return Object.values(state.states).some((ms) => ms.needsElevation);
}

async function createSymlinks() {
  try {
    await mountCreateSymlinks();
  } catch (e) {
    console.error("Failed to create symlinks:", e);
  }
}

/** Default cache root display — matches agent's default_cache_root(). */
const defaultCacheRoot = platformStore.platform === "win"
  ? "%LOCALAPPDATA%\\ufb\\sync"
  : `${platformStore.home || "~"}/.local/share/ufb/sync`;

export const mountStore = {
  get states() {
    return state.states;
  },
  get connected() {
    return state.connected;
  },
  get configs() {
    return state.configs;
  },
  get needsElevation() {
    return needsElevation();
  },
  defaultCacheRoot,
  getMountForPath,
  getMountPath,
  getShareName,
  loadStates,
  launchAgent,
  restart,
  start,
  stop,
  toggleMount,
  createSymlinks,
  saveConfig,
  loadConfig,
};
