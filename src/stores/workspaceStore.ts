import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";

export type TabKind = "main" | "job" | "browser" | "tracker" | "notes" | "transcode";

export interface WorkspaceTab {
  id: string;
  kind: TabKind;
  label: string;
  /** For job tabs: the job path */
  jobPath?: string;
  /** For standalone browser tabs: initial path */
  initialPath?: string;
  /** For notes tabs: the URL to load */
  url?: string;
}

interface WorkspaceState {
  tabs: WorkspaceTab[];
}

const [state, setState] = createStore<WorkspaceState>({
  tabs: [
    { id: "main", kind: "main", label: "Browser" },
    { id: "tracker", kind: "tracker", label: "Tracker" },
  ],
});

const [activeTabId, setActiveTabId] = createSignal("main");

let nextId = 1;

function openJobTab(jobPath: string, jobName: string) {
  // Reuse existing tab for same job
  const existing = state.tabs.find((t) => t.kind === "job" && t.jobPath === jobPath);
  if (existing) {
    setActiveTabId(existing.id);
    return;
  }
  const id = `job-${nextId++}`;
  setState("tabs", (tabs) => [...tabs, { id, kind: "job", label: jobName, jobPath }]);
  setActiveTabId(id);
}

function openBrowserTab(initialPath?: string) {
  const id = `browser-${nextId++}`;
  const label = initialPath ? initialPath.split(/[/\\]/).pop() || "Browser" : "Browser";
  setState("tabs", (tabs) => [...tabs, { id, kind: "browser", label, initialPath }]);
  setActiveTabId(id);
}

function openTrackerTab() {
  setActiveTabId("tracker");
}

function openNotesTab(url: string, label: string) {
  // Reuse existing tab for same URL
  const existing = state.tabs.find((t) => t.kind === "notes" && t.url === url);
  if (existing) {
    setActiveTabId(existing.id);
    return;
  }
  const id = `notes-${nextId++}`;
  setState("tabs", (tabs) => [...tabs, { id, kind: "notes", label, url }]);
  setActiveTabId(id);
}

function openTranscodeQueue() {
  const existing = state.tabs.find((t) => t.kind === "transcode");
  if (existing) {
    setActiveTabId(existing.id);
    return;
  }
  const id = `transcode-${nextId++}`;
  setState("tabs", (tabs) => [...tabs, { id, kind: "transcode" as TabKind, label: "Transcode Queue" }]);
  setActiveTabId(id);
}

function closeTab(id: string) {
  // Can't close permanent tabs
  if (id === "main" || id === "tracker") return;
  const idx = state.tabs.findIndex((t) => t.id === id);
  if (idx === -1) return;
  setState("tabs", (tabs) => tabs.filter((t) => t.id !== id));
  // If closing active tab, switch to previous or first
  if (activeTabId() === id) {
    const newIdx = Math.min(idx, state.tabs.length - 1);
    setActiveTabId(state.tabs[newIdx >= 0 ? newIdx : 0]?.id ?? "main");
  }
}

function getActiveTab(): WorkspaceTab | undefined {
  return state.tabs.find((t) => t.id === activeTabId());
}

// ── Main browser navigation (registered by DualBrowserView) ──
let _navigateLeft: ((path: string) => void) | undefined;
let _navigateRight: ((path: string) => void) | undefined;

function registerMainBrowserNav(
  left: (path: string) => void,
  right: (path: string) => void,
) {
  _navigateLeft = left;
  _navigateRight = right;
}

/** Switch to main tab and navigate the left browser */
function navigateMainLeft(path: string) {
  setActiveTabId("main");
  _navigateLeft?.(path);
}

/** Switch to main tab and navigate the right browser */
function navigateMainRight(path: string) {
  setActiveTabId("main");
  _navigateRight?.(path);
}

export const workspaceStore = {
  get tabs() { return state.tabs; },
  activeTabId,
  setActiveTabId,
  getActiveTab,
  openJobTab,
  openBrowserTab,
  openTrackerTab,
  openNotesTab,
  openTranscodeQueue,
  closeTab,
  registerMainBrowserNav,
  navigateMainLeft,
  navigateMainRight,
};
