import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import type { AppSettings } from "../lib/types";
import { loadSettings, saveSettings } from "../lib/tauri";

// Accent color palette (matches C++ UFB)
export const ACCENT_COLORS = [
  "#E06C75", // Red
  "#E5C07B", // Yellow
  "#98C379", // Green
  "#56B6C2", // Cyan
  "#61AFEF", // Blue
  "#C678DD", // Purple
  "#BE5046", // Dark Red
  "#D19A66", // Orange
  "#7EC8E3", // Light Blue
  "#A9DC76", // Lime
  "#FC9867", // Coral
  "#AB9DF2", // Lavender
  "#78DCE8", // Aqua
  "#FF6188", // Hot Pink
  "#FFD866", // Gold
  "#A6E22E", // Neon Green
  "#F8F8F2", // White
  "#808080", // Gray
];

const defaultSettings: AppSettings = {
  window: { x: -1, y: -1, width: 1914, height: 1060, maximized: false },
  panels: {
    showSubscriptions: true,
    showBrowser1: true,
    showBrowser2: true,
    showTranscodeQueue: false,
    useWindowsAccent: true,
  },
  appearance: {
    useWindowsAccentColor: true,
    customAccentColorIndex: -1,
    customPickerColorR: 0.5,
    customPickerColorG: 0.5,
    customPickerColorB: 0.5,
  },
  ui: {
    fontScale: 1.0,
    browserPanelRatios: [0.2, 0.4, 0.4],
  },
  sync: { enabled: false },
  meshSync: {
    nodeId: "",
    farmPath: "",
    httpPort: 49200,
    tags: "",
    apiSecret: "",
  },
  googleDrive: {
    scriptUrl: "",
    parentFolderId: "",
  },
  pathMappings: [],
  jobViews: [],
  aggregatedTrackerOpen: false,
};

const [settings, setSettings] = createStore<AppSettings>({ ...defaultSettings });
const [isLoaded, setIsLoaded] = createSignal(false);

async function load() {
  try {
    const s = await loadSettings();
    setSettings(s);
    setIsLoaded(true);
  } catch (err) {
    console.error("Failed to load settings:", err);
    setIsLoaded(true);
  }
}

async function save() {
  try {
    await saveSettings(settings);
  } catch (err) {
    console.error("Failed to save settings:", err);
  }
}

function getAccentColor(): string {
  const { customAccentColorIndex, customPickerColorR, customPickerColorG, customPickerColorB } =
    settings.appearance;
  if (customAccentColorIndex >= 0 && customAccentColorIndex < ACCENT_COLORS.length) {
    return ACCENT_COLORS[customAccentColorIndex];
  }
  // Custom picker color (RGB float 0-1 → hex)
  const r = Math.round(customPickerColorR * 255);
  const g = Math.round(customPickerColorG * 255);
  const b = Math.round(customPickerColorB * 255);
  return `#${r.toString(16).padStart(2, "0")}${g.toString(16).padStart(2, "0")}${b.toString(16).padStart(2, "0")}`;
}

export const settingsStore = {
  settings,
  setSettings,
  isLoaded,
  load,
  save,
  getAccentColor,
  ACCENT_COLORS,
};
