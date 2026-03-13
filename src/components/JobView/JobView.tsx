import { createSignal, For, onMount, Show } from "solid-js";
import { openUrl } from "@tauri-apps/plugin-opener";
import { listDirectory } from "../../lib/tauri";
import { settingsStore } from "../../stores/settingsStore";
import { FolderTabView } from "../FolderTabView/FolderTabView";
import { TrackerView } from "../TrackerView/TrackerView";
import type { FileEntry } from "../../lib/types";
import "./JobView.css";

interface JobViewProps {
  jobPath: string;
  jobName: string;
}

export function JobView(props: JobViewProps) {
  const [tabs, setTabs] = createSignal<FileEntry[]>([]);
  const [activeTab, setActiveTab] = createSignal<string | null>(null);

  onMount(async () => {
    try {
      const entries = await listDirectory(props.jobPath);
      const folders = entries.filter((e) => e.isDir && !e.name.startsWith("."));
      setTabs(folders);
      if (folders.length > 0) {
        setActiveTab(folders[0].path);
      }
    } catch (err) {
      console.error("Failed to load job folders:", err);
    }
  });

  function buildNotesUrl(mode: "doc" | "folder"): string | null {
    const { scriptUrl, parentFolderId } = settingsStore.settings.googleDrive;
    if (!scriptUrl || !parentFolderId) return null;
    const params = new URLSearchParams({
      job: props.jobName,
      parent: parentFolderId,
      mode,
    });
    return `${scriptUrl}?${params.toString()}`;
  }

  function openNotes(mode: "doc" | "folder") {
    const url = buildNotesUrl(mode);
    if (!url) {
      alert("Configure Google Drive Script URL and Parent Folder ID in Settings > Integrations.");
      return;
    }
    openUrl(url);
  }

  return (
    <div class="job-view">
      <div class="job-header">
        <span class="job-title">{props.jobName}</span>
        <div class="job-header-actions">
          <button class="job-header-btn" onClick={() => openNotes("doc")} title="Project Notes">
            <span class="icon">description</span>
          </button>
          <button class="job-header-btn" onClick={() => openNotes("folder")} title="Notes Folder">
            <span class="icon">folder_shared</span>
          </button>
        </div>
      </div>
      <div class="job-tabs">
        <For each={tabs()}>
          {(tab) => (
            <button
              class={`job-tab ${activeTab() === tab.path ? "active" : ""}`}
              onClick={() => setActiveTab(tab.path)}
            >
              {tab.name}
            </button>
          )}
        </For>
        <button
          class={`job-tab ${activeTab() === "__tracker__" ? "active" : ""}`}
          onClick={() => setActiveTab("__tracker__")}
        >
          <span class="icon" style={{ "font-size": "14px", "margin-right": "4px" }}>assignment</span>
          Tracker
        </button>
      </div>
      <div class="job-content">
        <Show when={activeTab() === "__tracker__"}>
          <div class="job-tab-container" style={{ display: "flex" }}>
            <TrackerView mode="job" jobPath={props.jobPath} jobName={props.jobName} />
          </div>
        </Show>
        <For each={tabs()}>
          {(tab) => (
            <div
              class="job-tab-container"
              style={{
                display: activeTab() === tab.path ? "flex" : "none",
              }}
            >
              <FolderTabView
                jobPath={props.jobPath}
                folderPath={tab.path}
                folderName={tab.name}
              />
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
