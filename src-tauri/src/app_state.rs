use crate::backup::BackupManager;
use crate::bookmarks::BookmarkManager;
use crate::columns::ColumnConfigManager;
use crate::db::Database;
use crate::mesh_sync::MeshSyncManager;
use crate::metadata::MetadataManager;
use crate::mount_client::MountClient;
use crate::subscription::SubscriptionManager;
use crate::thumbnails::ThumbnailManager;
use crate::transcode::TranscodeManager;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::sync::Mutex as StdMutex;

/// Shared application state, accessible from all Tauri commands.
pub struct AppState {
    pub db: Arc<Database>,
    pub subscription_manager: SubscriptionManager,
    pub metadata_manager: MetadataManager,
    pub column_config_manager: Arc<ColumnConfigManager>,
    pub bookmark_manager: BookmarkManager,
    pub backup_manager: BackupManager,
    pub thumbnail_manager: ThumbnailManager,
    pub mesh_sync_manager: Mutex<Option<MeshSyncManager>>,
    pub transcode_manager: Arc<TranscodeManager>,
    pub mount_client: Arc<MountClient>,
    pub device_id: String,
    /// Stores a deep-link URI from cold start so the frontend can fetch it on mount.
    pub pending_deep_link: StdMutex<Option<String>>,
}

impl AppState {
    pub fn initialize() -> Result<Self, String> {
        let device_id = crate::utils::get_device_id();
        let db_path = crate::utils::get_database_path();

        log::info!("Opening database at: {}", db_path.display());
        let db = Arc::new(
            Database::open(&db_path).map_err(|e| format!("Failed to open database: {}", e))?,
        );

        // Run migrations
        db.run_migrations()
            .map_err(|e| format!("Failed to run migrations: {}", e))?;

        // Initialize managers (order matters — matches C++ init sequence)
        let subscription_manager = SubscriptionManager::new(Arc::clone(&db));
        let metadata_manager = MetadataManager::new(Arc::clone(&db));
        let column_config_manager = Arc::new(ColumnConfigManager::new(Arc::clone(&db)));
        let bookmark_manager = BookmarkManager::new(Arc::clone(&db));
        let backup_manager = BackupManager::new(device_id.clone());
        let thumbnail_manager = ThumbnailManager::new(Arc::clone(&db));

        // Ensure unique index for thumbnail cache upserts
        ThumbnailManager::ensure_unique_index(&db)?;

        // Mesh sync — initialized later from settings
        let mesh_sync_manager = Mutex::new(None);

        // Transcode manager — resolve binary paths
        let transcode_manager = Arc::new(Self::init_transcode_manager());

        // Mount client — connects to mediamount-agent
        let mount_client = Arc::new(MountClient::new());

        log::info!("App state initialized (device_id: {})", device_id);

        Ok(Self {
            db,
            subscription_manager,
            metadata_manager,
            column_config_manager,
            bookmark_manager,
            backup_manager,
            thumbnail_manager,
            mesh_sync_manager,
            transcode_manager,
            mount_client,
            device_id,
            pending_deep_link: StdMutex::new(None),
        })
    }

    /// Resolve paths to ffmpeg, ffprobe, and exiftool binaries.
    /// In dev mode, looks next to the executable. Falls back to system PATH on Linux/macOS.
    fn init_transcode_manager() -> TranscodeManager {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default();

        let ext = if cfg!(target_os = "windows") { ".exe" } else { "" };

        let ffmpeg = Self::resolve_binary(&exe_dir, "ffmpeg", ext);
        let ffprobe = Self::resolve_binary(&exe_dir, "ffprobe", ext);
        let exiftool = Self::resolve_binary(&exe_dir, "exiftool", ext);

        log::info!(
            "Transcode binaries: ffmpeg={} (exists={}), ffprobe={} (exists={}), exiftool={} (exists={})",
            ffmpeg.display(), ffmpeg.exists(),
            ffprobe.display(), ffprobe.exists(),
            exiftool.display(), exiftool.exists(),
        );

        TranscodeManager::new(ffmpeg, ffprobe, exiftool)
    }

    /// Resolve a binary: check bundled locations first, then system PATH.
    fn resolve_binary(exe_dir: &std::path::Path, name: &str, ext: &str) -> std::path::PathBuf {
        let bin_name = format!("{}{}", name, ext);

        // 1. Bundled next to exe
        let bundled = exe_dir.join(&bin_name);
        if bundled.exists() {
            return bundled;
        }

        // 2. macOS: check app bundle Resources/ directories
        #[cfg(target_os = "macos")]
        {
            if let Some(contents_dir) = exe_dir.parent() {
                // Production .app bundle: Resources/external/ffmpeg-macos/bin/
                let bundled_ffmpeg = contents_dir.join("Resources/external/ffmpeg-macos/bin").join(&bin_name);
                if bundled_ffmpeg.exists() {
                    return bundled_ffmpeg;
                }
                // Production: Resources/external/exiftool-macos/
                let bundled_exiftool = contents_dir.join("Resources/external/exiftool-macos").join(&bin_name);
                if bundled_exiftool.exists() {
                    return bundled_exiftool;
                }
                // Direct in Resources/
                let resources = contents_dir.join("Resources").join(&bin_name);
                if resources.exists() {
                    return resources;
                }
            }
            // Dev build: check external/ffmpeg-macos/bin/ relative to manifest
            let dev_path = exe_dir.join("../../external/ffmpeg-macos/bin").join(&bin_name);
            if let Ok(resolved) = std::fs::canonicalize(&dev_path) {
                if resolved.exists() {
                    return resolved;
                }
            }
        }

        // 3. System PATH (Linux/macOS)
        #[cfg(not(target_os = "windows"))]
        if let Ok(output) = std::process::Command::new("which").arg(name).output() {
            if output.status.success() {
                let p = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !p.is_empty() {
                    return std::path::PathBuf::from(p);
                }
            }
        }

        // 4. Bare name fallback
        std::path::PathBuf::from(bin_name)
    }

    /// Set the Tauri app handle on the transcode manager and start the worker.
    pub async fn set_transcode_app_handle(&self, handle: tauri::AppHandle) {
        self.transcode_manager.set_app_handle(handle).await;
        self.transcode_manager.start_worker();
    }

    /// Initialize mesh sync from loaded settings.
    /// Auto-populates nodeId from hostname if empty, and writes resolved
    /// values back to settings so the frontend displays them.
    pub fn init_mesh_sync(&self, settings: &mut crate::settings::AppSettings) {
        // Auto-populate nodeId from hostname if not set
        if settings.mesh_sync.node_id.trim().is_empty() {
            settings.mesh_sync.node_id = hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "UFB_NODE".to_string());
            log::info!("Auto-populated mesh node_id from hostname: {}", settings.mesh_sync.node_id);
        }

        // Ensure port has a value
        if settings.mesh_sync.http_port == 0 {
            settings.mesh_sync.http_port = crate::mesh_sync::DEFAULT_HTTP_PORT;
        }

        // Write back resolved settings so frontend sees them
        if let Err(e) = settings.save() {
            log::warn!("Failed to save resolved settings: {}", e);
        }

        if settings.mesh_sync.farm_path.is_empty() {
            log::info!("Mesh sync not configured (farmPath missing), skipping");
            return;
        }

        let ms = &settings.mesh_sync;
        let node_id = ms.node_id.clone();

        let tags: Vec<String> = ms
            .tags
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let manager = MeshSyncManager::new(
            ms.farm_path.clone(),
            node_id.clone(),
            ms.http_port,
            ms.api_secret.clone(),
            tags,
            Arc::clone(&self.db),
            Arc::clone(&self.column_config_manager),
        );

        // Store the manager — enabling happens async via command
        let should_enable = settings.sync.enabled;

        // Use try_lock since we're in sync context during init
        *self.mesh_sync_manager.blocking_lock() = Some(manager);

        if should_enable {
            log::info!("Mesh sync configured (node: {}, farm: {}), will enable on startup", node_id, ms.farm_path);
        } else {
            log::info!("Mesh sync configured but not enabled (node: {}, farm: {})", node_id, ms.farm_path);
        }
    }

    /// Set the Tauri app handle on the mesh sync manager (needed for emitting events).
    pub async fn set_mesh_app_handle(&self, handle: tauri::AppHandle) {
        let lock = self.mesh_sync_manager.lock().await;
        if let Some(ref manager) = *lock {
            manager.set_app_handle(handle).await;
        }
    }

    /// Enable mesh sync (async — called after Tauri app starts).
    pub async fn enable_mesh_sync_if_configured(&self, settings: &crate::settings::AppSettings) {
        if !settings.sync.enabled {
            return;
        }
        let lock = self.mesh_sync_manager.lock().await;
        if let Some(ref manager) = *lock {
            manager.set_enabled(true).await;
        }
    }

    /// Shutdown mesh sync gracefully.
    pub async fn shutdown_mesh_sync(&self) {
        let lock = self.mesh_sync_manager.lock().await;
        if let Some(ref manager) = *lock {
            manager.shutdown().await;
        }
    }

    /// Reinitialize mesh sync with current settings (e.g. after farm_path change).
    pub async fn reinit_mesh_sync(&self, handle: tauri::AppHandle) {
        // Shutdown existing manager
        {
            let lock = self.mesh_sync_manager.lock().await;
            if let Some(ref manager) = *lock {
                manager.shutdown().await;
            }
        }

        // Drop old manager and create new one from current settings
        let settings = crate::settings::AppSettings::load();
        let ms = &settings.mesh_sync;

        if ms.farm_path.is_empty() {
            log::info!("Mesh sync reinit: farmPath empty, clearing manager");
            *self.mesh_sync_manager.lock().await = None;
            return;
        }

        let tags: Vec<String> = ms
            .tags
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let manager = MeshSyncManager::new(
            ms.farm_path.clone(),
            ms.node_id.clone(),
            ms.http_port,
            ms.api_secret.clone(),
            tags,
            Arc::clone(&self.db),
            Arc::clone(&self.column_config_manager),
        );

        manager.set_app_handle(handle).await;

        let should_enable = settings.sync.enabled;
        *self.mesh_sync_manager.lock().await = Some(manager);

        if should_enable {
            let lock = self.mesh_sync_manager.lock().await;
            if let Some(ref m) = *lock {
                m.set_enabled(true).await;
            }
        }

        log::info!("Mesh sync reinitialised (farm: {})", ms.farm_path);
    }
}
