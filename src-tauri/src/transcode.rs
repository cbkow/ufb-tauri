use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::sync::Mutex;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    Queued,
    Processing,
    CopyingMetadata,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscodeJob {
    pub id: String,
    pub input_path: String,
    pub output_path: String,
    pub status: JobStatus,
    pub progress: f64,
    pub current_frame: u64,
    pub total_frames: u64,
    pub fps: f64,
    pub error: Option<String>,
}

struct TranscodeState {
    queue: Vec<TranscodeJob>,
    active_child: Option<tokio::process::Child>,
    app_handle: Option<tauri::AppHandle>,
}

impl TranscodeState {
    /// Update a job by id and emit a job-updated event. Returns the updated job clone.
    fn update_job(&mut self, job_id: &str, f: impl FnOnce(&mut TranscodeJob)) -> Option<TranscodeJob> {
        if let Some(job) = self.queue.iter_mut().find(|j| j.id == job_id) {
            f(job);
            let snapshot = job.clone();
            self.emit_job_updated(&snapshot);
            Some(snapshot)
        } else {
            None
        }
    }

    fn emit_job_updated(&self, job: &TranscodeJob) {
        if let Some(ref handle) = self.app_handle {
            use tauri::Emitter;
            let _ = handle.emit("transcode:job-updated", job);
        }
    }

    fn emit_progress(&self, job: &TranscodeJob) {
        if let Some(ref handle) = self.app_handle {
            use tauri::Emitter;
            let _ = handle.emit("transcode:progress", job);
        }
    }
}

pub struct TranscodeManager {
    state: Arc<Mutex<TranscodeState>>,
    ffmpeg_path: PathBuf,
    ffprobe_path: PathBuf,
    exiftool_path: PathBuf,
}

impl TranscodeManager {
    pub fn new(ffmpeg_path: PathBuf, ffprobe_path: PathBuf, exiftool_path: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(TranscodeState {
                queue: Vec::new(),
                active_child: None,
                app_handle: None,
            })),
            ffmpeg_path,
            ffprobe_path,
            exiftool_path,
        }
    }

    pub async fn set_app_handle(&self, handle: tauri::AppHandle) {
        let mut state = self.state.lock().await;
        state.app_handle = Some(handle);
    }

    pub async fn add_jobs(&self, paths: Vec<String>) -> Vec<TranscodeJob> {
        let mut state = self.state.lock().await;
        let mut added = Vec::new();
        for input_path in paths {
            let input = std::path::Path::new(&input_path);
            let parent = input.parent().unwrap_or(input);
            let stem = input.file_stem().unwrap_or_default().to_string_lossy();
            let output_dir = parent.join("MP4");
            let output_path = output_dir.join(format!("{}.mp4", stem));

            let job = TranscodeJob {
                id: uuid::Uuid::new_v4().to_string(),
                input_path: input_path.clone(),
                output_path: output_path.to_string_lossy().to_string(),
                status: JobStatus::Queued,
                progress: 0.0,
                current_frame: 0,
                total_frames: 0,
                fps: 0.0,
                error: None,
            };
            added.push(job.clone());
            state.queue.push(job);
        }
        added
    }

    pub async fn get_queue(&self) -> Vec<TranscodeJob> {
        let state = self.state.lock().await;
        state.queue.clone()
    }

    pub async fn cancel_job(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let job = state.queue.iter().find(|j| j.id == id);
        let should_kill = match job.map(|j| &j.status) {
            Some(JobStatus::Processing | JobStatus::CopyingMetadata) => true,
            _ => false,
        };

        state.update_job(id, |job| {
            match job.status {
                JobStatus::Queued | JobStatus::Processing | JobStatus::CopyingMetadata => {
                    job.status = JobStatus::Cancelled;
                }
                _ => {}
            }
        });

        if should_kill {
            if let Some(ref mut child) = state.active_child {
                let _ = child.kill().await;
            }
        }
        Ok(())
    }

    pub async fn remove_job(&self, id: &str) {
        let mut state = self.state.lock().await;
        state.queue.retain(|j| j.id != id);
    }

    pub async fn clear_completed(&self) {
        let mut state = self.state.lock().await;
        state.queue.retain(|j| {
            !matches!(
                j.status,
                JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
            )
        });
    }

    /// Start the background worker that processes jobs sequentially.
    pub fn start_worker(self: &Arc<Self>) {
        let mgr = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Find the next queued job
                let job_id = {
                    let state = mgr.state.lock().await;
                    state
                        .queue
                        .iter()
                        .find(|j| j.status == JobStatus::Queued)
                        .map(|j| j.id.clone())
                };

                if let Some(id) = job_id {
                    mgr.process_job(&id).await;
                } else {
                    // No work — sleep briefly then check again
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        });
    }

    async fn process_job(&self, job_id: &str) {
        // Mark as processing
        {
            let mut state = self.state.lock().await;
            state.update_job(job_id, |job| {
                job.status = JobStatus::Processing;
            });
        }

        // 1. Create MP4 output directory
        let (input_path, output_path) = {
            let state = self.state.lock().await;
            let job = state.queue.iter().find(|j| j.id == job_id).unwrap();
            (job.input_path.clone(), job.output_path.clone())
        };

        let output_dir = std::path::Path::new(&output_path)
            .parent()
            .unwrap_or(std::path::Path::new("."));
        if let Err(e) = std::fs::create_dir_all(output_dir) {
            self.fail_job(job_id, &format!("Failed to create MP4 directory: {}", e))
                .await;
            return;
        }

        // 2. Get total frame count via ffprobe
        let total_frames = match self.get_frame_count(&input_path).await {
            Ok(n) => n,
            Err(e) => {
                log::warn!("ffprobe frame count failed ({}), using 0: {}", input_path, e);
                0
            }
        };

        // Update total_frames
        {
            let mut state = self.state.lock().await;
            if let Some(job) = state.queue.iter_mut().find(|j| j.id == job_id) {
                job.total_frames = total_frames;
            }
        }

        // 3. Run ffmpeg transcode
        if let Err(e) = self.run_ffmpeg(job_id, &input_path, &output_path, total_frames).await {
            // Check if it was cancelled
            let is_cancelled = {
                let state = self.state.lock().await;
                state.queue.iter().find(|j| j.id == job_id)
                    .map(|j| j.status == JobStatus::Cancelled)
                    .unwrap_or(false)
            };
            if is_cancelled {
                let _ = std::fs::remove_file(&output_path);
                return;
            }
            self.fail_job(job_id, &format!("FFmpeg failed: {}", e)).await;
            let _ = std::fs::remove_file(&output_path);
            return;
        }

        // Check if cancelled during transcode
        {
            let state = self.state.lock().await;
            if state.queue.iter().any(|j| j.id == job_id && j.status == JobStatus::Cancelled) {
                let _ = std::fs::remove_file(&output_path);
                return;
            }
        }

        // 4. Copy metadata with exiftool
        {
            let mut state = self.state.lock().await;
            state.update_job(job_id, |job| {
                job.status = JobStatus::CopyingMetadata;
            });
        }

        if let Err(e) = self.run_exiftool(&input_path, &output_path).await {
            log::warn!("Exiftool metadata copy failed (non-fatal): {}", e);
        }

        // 5. Mark completed
        {
            let mut state = self.state.lock().await;
            state.update_job(job_id, |job| {
                if job.status != JobStatus::Cancelled {
                    job.status = JobStatus::Completed;
                    job.progress = 100.0;
                }
            });
        }
    }

    async fn get_frame_count(&self, input_path: &str) -> Result<u64, String> {
        let mut cmd = tokio::process::Command::new(&self.ffprobe_path);
        cmd.args([
                "-v", "error",
                "-select_streams", "v:0",
                "-count_packets",
                "-show_entries", "stream=nb_read_packets",
                "-of", "csv=p=0",
                input_path,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = cmd.output()
            .await
            .map_err(|e| format!("Failed to run ffprobe: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let count = stdout
            .trim()
            .lines()
            .next()
            .unwrap_or("0")
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
        Ok(count)
    }

    async fn run_ffmpeg(
        &self,
        job_id: &str,
        input_path: &str,
        output_path: &str,
        total_frames: u64,
    ) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&self.ffmpeg_path);
        cmd.args([
                "-v", "quiet",
                "-progress", "pipe:1",
                "-i", input_path,
                "-c:v", "libx264",
                "-pix_fmt", "yuv420p",
                "-crf", "25",
                "-preset", "fast",
                "-c:a", "aac",
                "-b:a", "192k",
                "-y",
                output_path,
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn ffmpeg: {}", e))?;

        // Store child handle for cancellation
        let stdout = child.stdout.take();
        {
            let mut state = self.state.lock().await;
            state.active_child = Some(child);
        }

        // Parse progress from stdout
        if let Some(stdout) = stdout {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                if line.starts_with("frame=") {
                    if let Ok(frame) = line[6..].trim().parse::<u64>() {
                        let progress = if total_frames > 0 {
                            (frame as f64 / total_frames as f64 * 100.0).min(100.0)
                        } else {
                            0.0
                        };

                        let mut state = self.state.lock().await;
                        if let Some(job) = state.queue.iter_mut().find(|j| j.id == job_id) {
                            if job.status == JobStatus::Cancelled {
                                break;
                            }
                            job.current_frame = frame;
                            job.progress = progress;
                            let snapshot = job.clone();
                            state.emit_progress(&snapshot);
                        }
                    }
                } else if line.starts_with("fps=") {
                    if let Ok(fps) = line[4..].trim().parse::<f64>() {
                        let mut state = self.state.lock().await;
                        if let Some(job) = state.queue.iter_mut().find(|j| j.id == job_id) {
                            job.fps = fps;
                        }
                    }
                }
            }
        }

        // Wait for child to finish
        let status = {
            let mut state = self.state.lock().await;
            match state.active_child.as_mut() {
                Some(c) => c.wait().await,
                None => return Err("No active child process".to_string()),
            }
        };

        // Clear active child
        {
            let mut state = self.state.lock().await;
            state.active_child = None;
        }

        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => {
                // Exit code != 0 but could be from kill (cancellation)
                let is_cancelled = {
                    let state = self.state.lock().await;
                    state.queue.iter().any(|j| j.id == job_id && j.status == JobStatus::Cancelled)
                };
                if is_cancelled {
                    return Err("Cancelled".to_string());
                }
                Err(format!("FFmpeg exited with code: {:?}", s.code()))
            }
            Err(e) => Err(format!("Failed to wait for ffmpeg: {}", e)),
        }
    }

    async fn run_exiftool(&self, input_path: &str, output_path: &str) -> Result<(), String> {
        let mut cmd = tokio::process::Command::new(&self.exiftool_path);
        cmd.args([
                "-TagsFromFile",
                input_path,
                "-overwrite_original",
                output_path,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        #[cfg(target_os = "windows")]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = cmd.output()
            .await
            .map_err(|e| format!("Failed to run exiftool: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Exiftool failed: {}", stderr));
        }
        Ok(())
    }

    async fn fail_job(&self, job_id: &str, error: &str) {
        let mut state = self.state.lock().await;
        let error = error.to_string();
        state.update_job(job_id, |job| {
            job.status = JobStatus::Failed;
            job.error = Some(error.clone());
        });
    }
}
