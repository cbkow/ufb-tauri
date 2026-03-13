import { Show } from "solid-js";
import { createBrowserStore } from "../../stores/fileStore";
import { settingsStore } from "../../stores/settingsStore";
import { workspaceStore } from "../../stores/workspaceStore";
import { FileBrowser } from "../FileBrowser/FileBrowser";
import type { FileBrowserCallbacks } from "../FileBrowser/FileBrowser";
import { Splitter } from "../shared/Splitter";
import { getSpecialPaths } from "../../lib/tauri";
import { useBrowserDragDrop } from "../../lib/useBrowserDragDrop";
import "./DualBrowserView.css";

export interface DualBrowserViewProps {
  /** Called once with navigate functions so the parent can drive navigation */
  onRegisterNavigate?: (
    navigateLeft: (path: string) => void,
    navigateRight: (path: string) => void,
    selectInLeft?: (path: string) => void,
  ) => void;
}

export function DualBrowserView(props: DualBrowserViewProps) {
  const fallback = navigator.platform.startsWith("Win") ? "C:\\" : "/";

  const browser1 = createBrowserStore(fallback);
  const browser2 = createBrowserStore(fallback);

  // Navigate to home (left) and desktop (right) on mount
  getSpecialPaths()
    .then((paths) => {
      if (paths.desktop) browser1.navigateTo(paths.desktop);
      if (paths.downloads) browser2.navigateTo(paths.downloads);
    })
    .catch((e) => console.error("Failed to get special paths:", e));

  // External drop handlers — registered by each FileBrowser
  let externalDrop1: ((paths: string[]) => void) | undefined;
  let externalDrop2: ((paths: string[]) => void) | undefined;

  // Expose navigation to parent (for sidebar)
  props.onRegisterNavigate?.(
    (path: string) => browser1.navigateTo(path),
    (path: string) => browser2.navigateTo(path),
    (path: string) => browser1.selectItem(path),
  );

  const callbacks1: FileBrowserCallbacks = {
    onOpenInOtherBrowser: (path) => browser2.navigateTo(path),
    onOpenInNewTab: (path) => workspaceStore.openBrowserTab(path),
  };

  const callbacks2: FileBrowserCallbacks = {
    onOpenInOtherBrowser: (path) => browser1.navigateTo(path),
    onOpenInNewTab: (path) => workspaceStore.openBrowserTab(path),
  };

  // ── Shared drag/drop hook ──
  const activeDrag = useBrowserDragDrop({
    getBrowserStore: (id) => {
      if (id === browser1.id) return browser1;
      if (id === browser2.id) return browser2;
      return null;
    },
    getExternalDropHandler: (id) => {
      if (id === browser1.id) return externalDrop1;
      if (id === browser2.id) return externalDrop2;
      // Fallback for empty id
      return externalDrop1;
    },
    enableCrossBrowserDrag: true,
  });

  return (
    <div class="dual-browser-view">
      <BrowserPair
        browser1={browser1}
        browser2={browser2}
        callbacks1={callbacks1}
        callbacks2={callbacks2}
        showBrowser2={settingsStore.settings.panels.showBrowser2}
        initialRatio={0.5}
        onExternalDrop1={(h) => { externalDrop1 = h; }}
        onExternalDrop2={(h) => { externalDrop2 = h; }}
      />

      {/* Drag overlay indicator */}
      <Show when={activeDrag()}>
        {(drag) => (
          <div
            class="drag-overlay"
            style={{
              left: `${drag().x + 12}px`,
              top: `${drag().y + 12}px`,
            }}
          >
            {drag().paths.length === 1
              ? drag().paths[0].split(/[\\/]/).pop()
              : `${drag().paths.length} items`}
          </div>
        )}
      </Show>
    </div>
  );
}

function BrowserPair(props: {
  browser1: ReturnType<typeof createBrowserStore>;
  browser2: ReturnType<typeof createBrowserStore>;
  callbacks1: FileBrowserCallbacks;
  callbacks2: FileBrowserCallbacks;
  showBrowser2: boolean;
  initialRatio: number;
  onExternalDrop1: (handler: (paths: string[]) => void) => void;
  onExternalDrop2: (handler: (paths: string[]) => void) => void;
}) {
  return (
    <Show
      when={props.showBrowser2}
      fallback={
        <div class="browser-pair-single">
          <FileBrowser
            store={props.browser1}
            callbacks={props.callbacks1}
            onExternalDrop={props.onExternalDrop1}
          />
        </div>
      }
    >
      <Splitter
        direction="horizontal"
        initialSize={Math.round(props.initialRatio * (window.innerWidth * 0.8))}
        minSize={300}
        minSecondSize={300}
        first={
          <FileBrowser
            store={props.browser1}
            callbacks={props.callbacks1}
            onExternalDrop={props.onExternalDrop1}
          />
        }
        second={
          <FileBrowser
            store={props.browser2}
            callbacks={props.callbacks2}
            onExternalDrop={props.onExternalDrop2}
          />
        }
      />
    </Show>
  );
}
