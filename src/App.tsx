import { onMount, onCleanup, createSignal, createEffect, on, Show, For, Switch, Match, createMemo } from "solid-js";
import { settingsStore } from "./stores/settingsStore";
import { subscriptionStore } from "./stores/subscriptionStore";
import { workspaceStore } from "./stores/workspaceStore";
import { mountStore } from "./stores/mountStore";
import { platformStore } from "./stores/platformStore";
import { DualBrowserView } from "./components/DualBrowserView/DualBrowserView";
import { SubscriptionPanel } from "./components/SubscriptionPanel/SubscriptionPanel";
import { Splitter } from "./components/shared/Splitter";
import { JobView } from "./components/JobView/JobView";
import { TrackerView } from "./components/TrackerView/TrackerView";
import { TranscodeQueue } from "./components/TranscodeQueue/TranscodeQueue";
import { createBrowserStore } from "./stores/fileStore";
import { FileBrowser } from "./components/FileBrowser/FileBrowser";
import { FileOpsProgress } from "./components/FileOpsProgress/FileOpsProgress";
import { SettingsDialog } from "./components/Settings/SettingsDialog";
import { initDeepLinkListener, setDeepLinkNavigate } from "./lib/deepLink";
import { listDirectory } from "./lib/tauri";
import "./styles/theme.css";
import "./App.css";

export default function App() {
  const [ready, setReady] = createSignal(false);
  const [showSettings, setShowSettings] = createSignal(false);

  // Navigation refs for sidebar → DualBrowserView communication
  let navigateLeft: ((path: string) => void) | undefined;
  let navigateRight: ((path: string) => void) | undefined;
  let selectInLeft: ((path: string) => void) | undefined;

  onMount(async () => {
    await platformStore.init();
    await settingsStore.load();
    await subscriptionStore.loadAll();

    const accent = settingsStore.getAccentColor();
    document.documentElement.style.setProperty("--accent-color", accent);

    // Initialize mount store — launch agent + tray on macOS
    mountStore.launchAgent();
    mountStore.loadStates();

    setReady(true);

    // Deep-link handling: navigate left browser when a ufb:// URI is received
    setDeepLinkNavigate(async (path) => {
      workspaceStore.setActiveTabId("main");
      // If path points to a file, navigate to its parent and select the file
      try {
        await listDirectory(path);
        // It's a directory — navigate directly
        navigateLeft?.(path);
      } catch {
        // Not a directory — navigate to parent, then select the file
        const sep = path.includes("/") ? "/" : "\\";
        const parent = path.substring(0, path.lastIndexOf(sep));
        if (parent) {
          await navigateLeft?.(parent);
          // Select after navigation completes
          setTimeout(() => selectInLeft?.(path), 200);
        }
      }
    });
    initDeepLinkListener();

    // Global Ctrl+/- zoom handling
    const handleZoom = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        const current = parseFloat(document.documentElement.style.getPropertyValue("--zoom") || "1");
        document.documentElement.style.setProperty("--zoom", String(Math.min(current + 0.1, 2.0)));
        document.documentElement.style.fontSize = `${Math.min(current + 0.1, 2.0) * 100}%`;
      } else if (e.key === "-") {
        e.preventDefault();
        const current = parseFloat(document.documentElement.style.getPropertyValue("--zoom") || "1");
        document.documentElement.style.setProperty("--zoom", String(Math.max(current - 0.1, 0.5)));
        document.documentElement.style.fontSize = `${Math.max(current - 0.1, 0.5) * 100}%`;
      } else if (e.key === "0") {
        e.preventDefault();
        document.documentElement.style.setProperty("--zoom", "1");
        document.documentElement.style.fontSize = "100%";
      }
    };
    window.addEventListener("keydown", handleZoom);
    onCleanup(() => window.removeEventListener("keydown", handleZoom));

    // ── F5 / Ctrl+R → refresh browsers (prevent webview reload) ──
    const handleRefreshKey = (e: KeyboardEvent) => {
      if (e.key === "F5" || ((e.ctrlKey || e.metaKey) && e.key === "r")) {
        e.preventDefault();
        window.dispatchEvent(new CustomEvent("ufb:refresh"));
      }
    };
    window.addEventListener("keydown", handleRefreshKey);
    onCleanup(() => window.removeEventListener("keydown", handleRefreshKey));

    // ── Window focus → refresh browsers ──
    let focusDebounce: ReturnType<typeof setTimeout> | null = null;
    const handleWindowFocus = () => {
      if (focusDebounce) clearTimeout(focusDebounce);
      focusDebounce = setTimeout(() => {
        focusDebounce = null;
        window.dispatchEvent(new CustomEvent("ufb:refresh"));
      }, 500);
    };
    window.addEventListener("focus", handleWindowFocus);
    onCleanup(() => window.removeEventListener("focus", handleWindowFocus));
  });

  // ── Tab switch → refresh browsers ──
  createEffect(on(workspaceStore.activeTabId, () => {
    window.dispatchEvent(new CustomEvent("ufb:refresh"));
  }, { defer: true }));

  return (
    <Show when={ready()} fallback={<div class="loading">Loading...</div>}>
      <div class="app-root">
      <Splitter
        direction="horizontal"
        initialSize={250}
        minSize={160}
        minSecondSize={400}
        class="app-layout"
        first={
          <SubscriptionPanel
            onNavigate={(path) => navigateLeft?.(path)}
            onNavigateRight={(path) => navigateRight?.(path)}
          />
        }
        second={
          <div class="main-content">
            {/* Tab bar */}
            <div class="tab-bar">
              <div class="tab-bar-tabs">
                <For each={workspaceStore.tabs}>
                  {(tab) => (
                    <div
                      class={`tab-item ${workspaceStore.activeTabId() === tab.id ? "active" : ""}`}
                      onClick={() => workspaceStore.setActiveTabId(tab.id)}
                    >
                      <span class="tab-icon icon">
                        {tab.kind === "main" ? "desktop_windows" :
                         tab.kind === "job" ? "movie" :
                         tab.kind === "browser" ? "folder" :
                         tab.kind === "tracker" ? "assignment" :
                         tab.kind === "transcode" ? "swap_horiz" :
                         tab.kind === "notes" ? "description" : "tab"}
                      </span>
                      <span class="tab-label">{tab.label}</span>
                      <Show when={tab.id !== "main" && tab.id !== "tracker"}>
                        <span
                          class="tab-close"
                          onClick={(e) => {
                            e.stopPropagation();
                            workspaceStore.closeTab(tab.id);
                          }}
                        >
                          <span class="icon">close</span>
                        </span>
                      </Show>
                    </div>
                  )}
                </For>
              </div>
              <button class="tab-settings-btn" onClick={() => setShowSettings(true)} title="Settings">
                <span class="icon">settings</span>
              </button>
            </div>

            {/* Tab content */}
            <div class="tab-content">
              <For each={workspaceStore.tabs}>
                {(tab) => (
                  <div
                    class="tab-panel"
                    style={{ display: workspaceStore.activeTabId() === tab.id ? "flex" : "none" }}
                  >
                    <Switch>
                      <Match when={tab.kind === "main"}>
                        <DualBrowserView
                          onRegisterNavigate={(left, right, selLeft) => {
                            navigateLeft = left;
                            navigateRight = right;
                            selectInLeft = selLeft;
                          }}
                        />
                      </Match>
                      <Match when={tab.kind === "job"}>
                        <JobView jobPath={tab.jobPath!} jobName={tab.label} />
                      </Match>
                      <Match when={tab.kind === "browser"}>
                        <StandaloneBrowser initialPath={tab.initialPath} />
                      </Match>
                      <Match when={tab.kind === "tracker"}>
                        <TrackerView mode="aggregated" />
                      </Match>
                      <Match when={tab.kind === "transcode"}>
                        <TranscodeQueue />
                      </Match>
                    </Switch>
                  </div>
                )}
              </For>
            </div>
          </div>
        }
      />
      <FileOpsProgress />
      <MountStatusBar />
      </div>

      <Show when={showSettings()}>
        <SettingsDialog onClose={() => setShowSettings(false)} />
      </Show>
    </Show>
  );
}

/** Mount status indicator — hidden when all healthy or no mounts configured */
function MountStatusBar() {
  const hasMounts = createMemo(() => Object.keys(mountStore.states).length > 0);
  const hasIssues = createMemo(() => {
    const states = Object.values(mountStore.states);
    return states.some(
      (s: any) =>
        s.state === "error" ||
        s.state === "stopped"
    );
  });

  return (
    <Show when={hasMounts() && hasIssues()}>
      <div class="mount-status-bar">
        <span class="icon" style={{ "font-size": "13px", color: "#ff9800" }}>
          warning
        </span>
        <span style={{ "font-size": "11px" }}>
          {Object.values(mountStore.states)
            .filter(
              (s: any) =>
                s.state !== "mounted" && s.state !== "initializing" && s.state !== "mounting"
            )
            .map((s: any) => `${s.mountId}: ${s.stateDetail}`)
            .join(", ")}
        </span>
      </div>
    </Show>
  );
}

/** A standalone single-browser tab */
function StandaloneBrowser(props: { initialPath?: string }) {
  const defaultPath = navigator.platform.startsWith("Win") ? "C:\\" : "/";
  const store = createBrowserStore(props.initialPath ?? defaultPath);
  return (
    <FileBrowser
      store={store}
      callbacks={{
        onOpenInNewTab: (path) => workspaceStore.openBrowserTab(path),
      }}
    />
  );
}
