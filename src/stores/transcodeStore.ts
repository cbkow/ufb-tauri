import { createStore, reconcile } from "solid-js/store";
import { listen } from "@tauri-apps/api/event";
import {
  transcodeAddJobs,
  transcodeGetQueue,
  transcodeCancelJob,
  transcodeRemoveJob,
  transcodeClearCompleted,
} from "../lib/tauri";

export type JobStatus =
  | "queued"
  | "processing"
  | "copyingMetadata"
  | "completed"
  | "failed"
  | "cancelled";

export interface TranscodeJob {
  id: string;
  inputPath: string;
  outputPath: string;
  status: JobStatus;
  progress: number;
  currentFrame: number;
  totalFrames: number;
  fps: number;
  error: string | null;
}

const [jobs, setJobs] = createStore<TranscodeJob[]>([]);

let listenersSetUp = false;

function setupListeners() {
  if (listenersSetUp) return;
  listenersSetUp = true;

  listen<TranscodeJob>("transcode:progress", (e) => {
    const update = e.payload;
    setJobs(
      (j) => j.id === update.id,
      {
        progress: update.progress,
        currentFrame: update.currentFrame,
        fps: update.fps,
        status: update.status,
      }
    );
  });

  listen<TranscodeJob>("transcode:job-updated", (e) => {
    const update = e.payload;
    setJobs(
      (j) => j.id === update.id,
      {
        status: update.status,
        progress: update.progress,
        error: update.error,
      }
    );
  });
}

async function loadQueue() {
  setupListeners();
  try {
    const queue = await transcodeGetQueue();
    setJobs(reconcile(queue));
  } catch (e) {
    console.error("Failed to load transcode queue:", e);
  }
}

async function addJobs(paths: string[]) {
  setupListeners();
  try {
    const newJobs = await transcodeAddJobs(paths);
    setJobs((prev) => [...prev, ...newJobs]);
  } catch (e) {
    console.error("Failed to add transcode jobs:", e);
  }
}

async function cancelJob(id: string) {
  try {
    await transcodeCancelJob(id);
  } catch (e) {
    console.error("Failed to cancel transcode job:", e);
  }
}

async function removeJob(id: string) {
  try {
    await transcodeRemoveJob(id);
    setJobs((prev) => prev.filter((j) => j.id !== id));
  } catch (e) {
    console.error("Failed to remove transcode job:", e);
  }
}

async function clearCompleted() {
  try {
    await transcodeClearCompleted();
    setJobs((prev) =>
      prev.filter(
        (j) =>
          j.status !== "completed" &&
          j.status !== "failed" &&
          j.status !== "cancelled"
      )
    );
  } catch (e) {
    console.error("Failed to clear completed jobs:", e);
  }
}

export const transcodeStore = {
  get jobs() {
    return jobs;
  },
  loadQueue,
  addJobs,
  cancelJob,
  removeJob,
  clearCompleted,
};
