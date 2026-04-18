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
  mountGetMode,
  mountLaunchAgent,
  mountCreateSymlinks,
  mountGetCacheStats,
  mountDrainShareCache,
  probePathReachable,
  type MountMode,
  type CacheStats,
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
  mode: MountMode;
  /// Per-mount cache footprint. Populated on demand by `refreshCacheStats`
  /// and refreshed asynchronously when the agent emits `mount:cache-stats`
  /// (e.g., after a drain).
  cacheStats: Record<string, CacheStats>;
  /// Per-mount reachability. `true` = last probe succeeded; `false` = last
  /// probe failed (VPN down, NAS offline, etc.); `undefined` = not yet
  /// probed. Drives the "Unavailable" state in the bookmarks panel.
  reachability: Record<string, boolean>;
}

const [state, setState] = createStore<MountStoreState>({
  states: {},
  connected: false,
  configs: [],
  mode: "fileprovider",
  cacheStats: {},
  reachability: {},
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
      const prev = state.states[update.mountId];
      console.log("[mountStore] state-update:", update.mountId, update.state, update);
      setState("states", update.mountId, reconcile(update));
      // When a mount SETTLES into `mounted` (transition from a non-mounted
      // prior state), re-probe reachability. Without this, a probe that
      // ran during the initial mounting phase (returning false because
      // the symlink or WinFsp mount didn't exist yet) stays stale until
      // the 30s tick — which surfaces as "Unavailable" + blocked clicks.
      if (update.state === "mounted" && prev?.state !== "mounted") {
        const cfg = state.configs.find((c) => c.id === update.mountId);
        if (cfg) {
          setTimeout(() => { void probeOne(cfg); }, 800);
        }
      }
    });

    await listen<boolean>("mount:connection", (e) => {
      console.log("[mountStore] connection:", e.payload);
      const wasConnected = state.connected;
      setState("connected", e.payload);
      // Race fix: at cold startup (esp. in the installed app, where the
      // agent is launched by the Run-key just as the UFB window opens),
      // `loadStates()` often runs BEFORE the agent's IPC pipe is ready.
      // `mountGetConfig` / `mountGetStates` return empty, and we'd be
      // stuck with an empty Bookmarks panel forever. When the connection
      // transitions false→true, re-fetch so the UI catches up, then kick
      // the reachability probe so we don't wait the full 30s interval
      // before Bookmarks get proper Connected/Unavailable state.
      if (e.payload && !wasConnected) {
        console.log("[mountStore] connection established — refetching state");
        void (async () => {
          try {
            const states = await mountGetStates();
            setState("states", reconcile(states));
            const cfg = await mountGetConfig();
            setState("configs", (cfg.mounts ?? []).filter((m) => m.enabled));
            const mode = await mountGetMode();
            setState("mode", mode);
            // Kick the probe now that we have configs. Small delay so
            // agent-side mounts have a moment to settle past their
            // initial Mounting → Mounted transition.
            setTimeout(() => { void probeReachabilityNow(); }, 1500);
          } catch (err) {
            console.error("post-connect refetch failed:", err);
          }
        })();
      }
    });

    await listen<CacheStats>("mount:cache-stats", (e) => {
      setState("cacheStats", e.payload.mountId, e.payload);
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
    // Mode tells macOS sync mounts apart: NFS loopback at ~/ufb/vfs/{share}
    // vs FileProvider at ~/Library/CloudStorage/UFB-{display}.
    const mode = await mountGetMode();
    console.log("[mountStore] mode:", mode);
    setState("mode", mode);
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
    // Unified path for sync + non-sync. Slice 5 retired the FileProvider
    // ~/Library/CloudStorage/UFB-* path; NFS loopback mounts (sync) and
    // plain-SMB symlinks (non-sync) both live under ~/ufb/mounts/.
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

/** Fire-and-forget: ask the agent for fresh cache stats. Response lands
 * on the `mount:cache-stats` event and updates `cacheStats[mountId]`. */
async function refreshCacheStats(mountId: string) {
  try {
    await mountGetCacheStats(mountId);
  } catch (e) {
    console.error("Failed to request cache stats:", e);
  }
}

async function drainShareCache(mountId: string) {
  try {
    await mountDrainShareCache(mountId);
  } catch (e) {
    console.error("Failed to drain cache:", e);
  }
}

// ── Reachability probe ──────────────────────────────────────────────────
//
// Background probe that keeps `state.reachability[mountId]` fresh so the
// Bookmarks panel can render an "Unavailable" state when a mount's
// destination is genuinely offline (VPN dropped, NAS down, etc.).
//
// **Debounce**: a single failed probe does NOT mark unreachable — cold
// starts regularly miss the first probe because the symlink or WinFsp
// mount hasn't settled yet. We require FAILURE_THRESHOLD consecutive
// failures before flipping to unreachable. Any single success clears
// the counter and sets reachable. This trades a bit of detection
// latency (~30-60s for a real outage) against false positives on
// startup — which were the real problem for users.
//
// Probes run every PROBE_INTERVAL_MS + on `probeReachabilityNow()`
// (called post-connection, on window focus, on state-settled events).

const PROBE_INTERVAL_MS = 30_000;
const FAILURE_THRESHOLD = 2;

let probeTimer: ReturnType<typeof setInterval> | null = null;
const probeInFlight = new Set<string>();
const probeFailureCount = new Map<string, number>();

async function probeOne(cfg: MountConfig) {
  if (probeInFlight.has(cfg.id)) return;
  probeInFlight.add(cfg.id);
  try {
    const path = getMountPath(cfg);
    if (!path) {
      // No mount path → nothing to probe. Treat as reachable.
      probeFailureCount.delete(cfg.id);
      setState("reachability", cfg.id, true);
      return;
    }
    const reachable = await probePathReachable(path);
    if (reachable) {
      // Any single success immediately clears. No lingering stale
      // "unavailable" after the VPN reconnects or the mount settles.
      probeFailureCount.delete(cfg.id);
      setState("reachability", cfg.id, true);
    } else {
      const n = (probeFailureCount.get(cfg.id) ?? 0) + 1;
      probeFailureCount.set(cfg.id, n);
      if (n >= FAILURE_THRESHOLD) {
        setState("reachability", cfg.id, false);
      }
      // If under the threshold, leave reachability undefined (the UI
      // treats that as "not yet known", which is correct on cold start).
    }
  } catch (e) {
    // Probe command itself failed (unlikely) — count it toward the
    // threshold but don't immediately flip to unreachable; a transient
    // IPC glitch shouldn't trip the UI into Unavailable.
    console.error("probePathReachable failed:", e);
    const n = (probeFailureCount.get(cfg.id) ?? 0) + 1;
    probeFailureCount.set(cfg.id, n);
    if (n >= FAILURE_THRESHOLD) {
      setState("reachability", cfg.id, false);
    }
  } finally {
    probeInFlight.delete(cfg.id);
  }
}

async function probeReachabilityNow() {
  const cfgs = state.configs;
  // Kick off all probes in parallel. `probeOne` dedups per-mount so
  // overlapping tick + focus refresh is safe.
  await Promise.all(cfgs.map(probeOne));
}

function startReachabilityProbe() {
  // Disabled. `fs::metadata` on Windows UNC symlinks and WinFsp reparse
  // points is not a reliable liveness signal — it routinely fails for
  // mounts that are perfectly accessible (cold SMB session cache, stat
  // under load, etc.), causing false "Unavailable" state. Since we
  // can't flag reachability reliably from the frontend, we don't flag
  // it at all. The agent already surfaces genuine offline state via
  // `syncState` on sync mounts; non-sync mounts stay "Connected" and
  // discover reachability at click time via the normal listDirectory
  // flow.
  //
  // Plumbing (command, store field, probeOne) is intentionally left
  // in place so we can re-enable with a smarter implementation later
  // (e.g., agent-side probes shared with NasHealth).
  if (probeTimer) return;
  void probeTimer; // silence unused var in this disabled path
}

/** Stale-aware reachability getter. Returns undefined before first probe. */
function isMountReachable(mountId: string): boolean | undefined {
  return state.reachability[mountId];
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
  get mode() {
    return state.mode;
  },
  get cacheStats() {
    return state.cacheStats;
  },
  get reachability() {
    return state.reachability;
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
  refreshCacheStats,
  drainShareCache,
  isMountReachable,
  probeReachabilityNow,
  startReachabilityProbe,
};
