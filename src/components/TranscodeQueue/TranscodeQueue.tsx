import { For, Show, onMount } from "solid-js";
import { transcodeStore } from "../../stores/transcodeStore";
import "./TranscodeQueue.css";

export function TranscodeQueue() {
  onMount(() => {
    transcodeStore.loadQueue();
  });

  const jobs = () => transcodeStore.jobs;

  const hasCompleted = () =>
    jobs().some(
      (j) =>
        j.status === "completed" ||
        j.status === "failed" ||
        j.status === "cancelled"
    );

  return (
    <div class="transcode-queue">
      <div class="transcode-header">
        <span>Transcode Queue</span>
        <div class="transcode-header-actions">
          <span class="transcode-count">{jobs().length} jobs</span>
          <Show when={hasCompleted()}>
            <button
              class="transcode-clear-btn"
              onClick={() => transcodeStore.clearCompleted()}
            >
              Clear Completed
            </button>
          </Show>
        </div>
      </div>
      <div class="transcode-content">
        <For each={jobs()}>
          {(job) => {
            const filename = () =>
              job.inputPath.split(/[/\\]/).pop() ?? job.inputPath;
            return (
              <div class="transcode-job">
                <div class="transcode-job-info">
                  <span class="transcode-filename truncate" title={job.inputPath}>
                    {filename()}
                  </span>
                  <div class="transcode-job-actions">
                    <span class={`transcode-status status-${job.status}`}>
                      {job.status === "copyingMetadata" ? "copying metadata" : job.status}
                    </span>
                    <Show
                      when={
                        job.status === "queued" || job.status === "processing" || job.status === "copyingMetadata"
                      }
                    >
                      <button
                        class="transcode-cancel-btn"
                        onClick={() => transcodeStore.cancelJob(job.id)}
                        title="Cancel"
                      >
                        <span class="icon">cancel</span>
                      </button>
                    </Show>
                    <Show
                      when={
                        job.status === "completed" ||
                        job.status === "failed" ||
                        job.status === "cancelled"
                      }
                    >
                      <button
                        class="transcode-remove-btn"
                        onClick={() => transcodeStore.removeJob(job.id)}
                        title="Remove"
                      >
                        <span class="icon">close</span>
                      </button>
                    </Show>
                  </div>
                </div>
                <Show when={job.status === "processing"}>
                  <div class="transcode-progress-bar">
                    <div
                      class="transcode-progress-fill"
                      style={{ width: `${job.progress}%` }}
                    />
                  </div>
                  <span class="transcode-progress-text">
                    {job.progress.toFixed(1)}%
                    {job.totalFrames > 0 && ` \u2014 ${job.currentFrame}/${job.totalFrames} frames`}
                    {job.fps > 0 && ` \u2014 ${job.fps.toFixed(1)} fps`}
                  </span>
                </Show>
                <Show when={job.status === "failed" && job.error}>
                  <div class="transcode-error">{job.error}</div>
                </Show>
                <Show when={job.status === "completed"}>
                  <div class="transcode-output truncate" title={job.outputPath}>
                    {job.outputPath}
                  </div>
                </Show>
              </div>
            );
          }}
        </For>
        <Show when={jobs().length === 0}>
          <div class="transcode-empty">
            No transcode jobs. Right-click a video file and select "Transcode to MP4" to add jobs.
          </div>
        </Show>
      </div>
    </div>
  );
}
