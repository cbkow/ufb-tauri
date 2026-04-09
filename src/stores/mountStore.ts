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
} from "../lib/tauri";

export interface MountStateUpdate {
  mountId: string;
  state: string;
  stateDetail: string;
  syncState?: string;
  syncStateDetail?: string;
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
}

export interface MountsConfig {
  version: number;
  mounts: MountConfig[];
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

function setupListeners() {
  if (listenersSetUp) return;
  listenersSetUp = true;

  listen<MountStateUpdate>("mount:state-update", (e) => {
    const update = e.payload;
    console.log("[mountStore] state-update:", update.mountId, update.state, update);
    setState("states", update.mountId, reconcile(update));
  });

  listen<boolean>("mount:connection", (e) => {
    console.log("[mountStore] connection:", e.payload);
    setState("connected", e.payload);
  });
}

/** Get the mount state for a given path via prefix matching against mount paths. */
function getMountForPath(path: string): MountStateUpdate | undefined {
  if (state.configs.length === 0) return undefined;
  const isWindows = path.length >= 2 && path[1] === ":";
  if (isWindows) {
    const normalized = path.replace(/\//g, "\\").toLowerCase();
    for (const cfg of state.configs) {
      if (!cfg.mountDriveLetter) continue;
      const mountPrefix = (cfg.mountDriveLetter + ":\\").toLowerCase();
      if (normalized.startsWith(mountPrefix)) {
        return state.states[cfg.id];
      }
    }
  } else {
    // Linux/macOS: match against mount path fields
    for (const cfg of state.configs) {
      const mountPath = getMountPath(cfg);
      if (mountPath && (path === mountPath || path.startsWith(mountPath + "/"))) {
        return state.states[cfg.id];
      }
      // Also check explicit path fields
      const extraPaths = [cfg.mountPathLinux, cfg.mountPathMacos, cfg.smbMountPath].filter(Boolean) as string[];
      for (const mp of extraPaths) {
        if (path === mp || path.startsWith(mp + "/")) {
          return state.states[cfg.id];
        }
      }
    }
  }
  return undefined;
}

async function loadStates() {
  console.log("[mountStore] loadStates called");
  setupListeners();
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
  // Sync mounts use the sync root path
  if (cfg.syncEnabled) {
    if (cfg.syncRootPath) return cfg.syncRootPath;
    // Default: C:\Volumes\ufb\{shareName}
    const shareName = cfg.nasSharePath.replace(/^\\\\/, "").split("\\")[1] || cfg.id;
    if (platformStore.platform === "win") {
      return `C:\\Volumes\\ufb\\${shareName}`;
    }
    return `${platformStore.home || "~"}/.local/share/ufb/sync/${shareName}`;
  }
  if (platformStore.platform === "win") {
    return cfg.mountDriveLetter ? cfg.mountDriveLetter + ":\\" : "";
  }
  if (platformStore.platform === "mac") {
    // macOS: explicit path or auto-derived /opt/ufb/mounts/{id}
    if (cfg.mountPathMacos) return cfg.mountPathMacos;
    if (cfg.id) return `/opt/ufb/mounts/${cfg.id}`;
    return "";
  }
  // Linux: explicit path or auto-derived from mount ID
  if (cfg.mountPathLinux) return cfg.mountPathLinux;
  if (cfg.smbMountPath) return cfg.smbMountPath;
  const home = platformStore.home;
  if (home && cfg.id) return `${home}/.local/share/ufb/mnt/${cfg.id}`;
  return "";
}

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
  getMountForPath,
  getMountPath,
  loadStates,
  launchAgent,
  restart,
  start,
  stop,
  toggleMount,
  saveConfig,
  loadConfig,
};
