import { createStore, reconcile } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import {
  mountGetStates,
  mountIsConnected,
  mountRestart,
  mountFlushAndRestart,
  mountSwitchToSmb,
  mountForceRclone,
  mountSaveConfig,
  mountGetConfig,
  mountLaunchAgent,
} from "../lib/tauri";

export interface MountStateUpdate {
  mountId: string;
  state: string;
  stateDetail: string;
  cacheUsedBytes: number;
  cacheMaxBytes: number;
  dirtyFiles: number;
  lastFallbackTime: number | null;
  isRcloneActive: boolean;
  isSmbActive: boolean;
}

export interface MountConfig {
  id: string;
  enabled: boolean;
  displayName: string;
  nasSharePath: string;
  credentialKey: string;
  rcloneDriveLetter: string;
  smbDriveLetter?: string;
  junctionPath: string;
  cacheDirPath: string;
  cacheMaxSize: string;
  cacheMaxAge: string;
  vfsWriteBack: string;
  vfsReadChunkSize: string;
  vfsReadChunkStreams: number;
  vfsReadAhead: string;
  bufferSize: string;
  probeIntervalSecs: number;
  probeTimeoutMs: number;
  fallbackThreshold: number;
  recoveryThreshold: number;
  maxRcloneStartAttempts: number;
  healthcheckFileName: string;
  extraRcloneFlags: string[];
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

/** Get the mount state for a given path via prefix matching against junction paths. */
function getMountForPath(path: string): MountStateUpdate | undefined {
  if (state.configs.length === 0) return undefined;
  const normalized = path.replace(/\//g, "\\").toLowerCase();
  for (const cfg of state.configs) {
    const junctionNorm = cfg.junctionPath.replace(/\//g, "\\").toLowerCase();
    if (normalized.startsWith(junctionNorm)) {
      return state.states[cfg.id];
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
    setState("configs", cfg.mounts ?? []);
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

async function flushAndRestart(mountId: string) {
  try {
    await mountFlushAndRestart(mountId);
  } catch (e) {
    console.error("Failed to flush and restart mount:", e);
  }
}

async function switchToSmb(mountId: string) {
  try {
    await mountSwitchToSmb(mountId);
  } catch (e) {
    console.error("Failed to switch to SMB:", e);
  }
}

async function forceRclone(mountId: string) {
  try {
    await mountForceRclone(mountId);
  } catch (e) {
    console.error("Failed to force rclone:", e);
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
  loadStates,
  launchAgent,
  restart,
  flushAndRestart,
  switchToSmb,
  forceRclone,
  saveConfig,
  loadConfig,
};
