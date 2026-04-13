use crate::app_state::AppState;
use crate::bookmarks::Bookmark;
use crate::columns::{ColumnDefinition, ColumnPreset};
use crate::file_ops::FileEntry;
use crate::project_config::{FolderTypeConfig, ProjectConfig};
use crate::settings::AppSettings;
use crate::subscription::{Subscription, TrackedItemRecord};
use crate::utils::{to_canonical_path, from_canonical_path};
use chrono::Local;
use tauri::{Emitter, State};

/// Load path mappings from settings (cached per-call).
fn load_mappings() -> Vec<crate::settings::PathMapping> {
    AppSettings::load().path_mappings
}

// ── Subscriptions ──

#[tauri::command]
pub async fn subscribe_to_job(
    state: State<'_, AppState>,
    job_path: String,
    job_name: String,
) -> Result<Subscription, String> {
    let mappings = load_mappings();
    let canonical_path = to_canonical_path(&job_path, &mappings);

    // On non-Windows, verify the path was actually translated to Windows-canonical format.
    // If not, no active mapping covers this location and we'd corrupt the DB with a native path.
    #[cfg(not(target_os = "windows"))]
    if !canonical_path.contains(':') {
        return Err("No active path mapping covers this location. Enable or add a mapping in Settings > Paths.".into());
    }

    let mut result = state.subscription_manager.subscribe_to_job(&canonical_path, &job_name)?;
    // Localize path for frontend
    result.job_path = from_canonical_path(&result.job_path, &mappings);
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "sub_add", "job_path": canonical_path, "job_name": job_name});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(result)
}

#[tauri::command]
pub async fn unsubscribe_from_job(state: State<'_, AppState>, job_path: String) -> Result<(), String> {
    let canonical_path = to_canonical_path(&job_path, &load_mappings());
    state.subscription_manager.unsubscribe_from_job(&canonical_path)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "sub_remove", "job_path": canonical_path});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(())
}

#[tauri::command]
pub fn get_subscriptions(state: State<AppState>) -> Result<Vec<Subscription>, String> {
    let mappings = load_mappings();
    let mut subs = state.subscription_manager.get_all_subscriptions()?;
    for sub in &mut subs {
        sub.job_path = from_canonical_path(&sub.job_path, &mappings);
    }
    Ok(subs)
}

// ── Metadata ──

#[tauri::command]
pub fn get_item_metadata(state: State<AppState>, item_path: String) -> Result<Option<String>, String> {
    let canonical_path = to_canonical_path(&item_path, &load_mappings());
    state.metadata_manager.get_metadata(&canonical_path)
}

#[tauri::command]
pub async fn upsert_item_metadata(
    state: State<'_, AppState>,
    job_path: String,
    item_path: String,
    folder_name: String,
    metadata_json: String,
    is_tracked: bool,
) -> Result<(), String> {
    let mappings = load_mappings();
    let canonical_job = to_canonical_path(&job_path, &mappings);
    let canonical_item = to_canonical_path(&item_path, &mappings);
    state.metadata_manager.write_immediate(
        &canonical_job,
        &canonical_item,
        &folder_name,
        &metadata_json,
        is_tracked,
    )?;

    // Also notify mesh sync if enabled
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        mesh.on_metadata_edited(&canonical_job, &canonical_item, &metadata_json, &folder_name, is_tracked).await;
    }

    Ok(())
}

#[tauri::command]
pub fn get_tracked_items(
    state: State<AppState>,
    job_path: String,
) -> Result<Vec<TrackedItemRecord>, String> {
    let mappings = load_mappings();
    let canonical_job = to_canonical_path(&job_path, &mappings);
    let mut items = state.subscription_manager.get_tracked_items(&canonical_job)?;
    for item in &mut items {
        item.item_path = from_canonical_path(&item.item_path, &mappings);
        item.job_path = from_canonical_path(&item.job_path, &mappings);
    }
    Ok(items)
}

#[tauri::command]
pub fn get_all_tracked_items(state: State<AppState>) -> Result<Vec<TrackedItemRecord>, String> {
    let mappings = load_mappings();
    let mut items = state.subscription_manager.get_all_tracked_items()?;
    for item in &mut items {
        item.item_path = from_canonical_path(&item.item_path, &mappings);
        item.job_path = from_canonical_path(&item.job_path, &mappings);
    }
    Ok(items)
}

#[tauri::command]
pub fn get_folder_metadata(
    state: State<AppState>,
    job_path: String,
    folder_name: String,
) -> Result<Vec<crate::subscription::ItemMetadataRecord>, String> {
    let mappings = load_mappings();
    let canonical_job = to_canonical_path(&job_path, &mappings);
    let mut records = state.subscription_manager.get_folder_item_metadata(&canonical_job, &folder_name)?;
    for record in &mut records {
        record.item_path = from_canonical_path(&record.item_path, &mappings);
    }
    Ok(records)
}

// ── Columns ──

#[tauri::command]
pub fn get_column_defs(
    state: State<AppState>,
    job_path: String,
    folder_name: String,
) -> Result<Vec<ColumnDefinition>, String> {
    let mappings = load_mappings();
    let canonical_job = to_canonical_path(&job_path, &mappings);
    let mut defs = state.column_config_manager.get_column_defs(&canonical_job, &folder_name)?;
    for def in &mut defs {
        def.job_path = from_canonical_path(&def.job_path, &mappings);
    }
    Ok(defs)
}

#[tauri::command]
pub async fn add_column(state: State<'_, AppState>, mut def: ColumnDefinition) -> Result<i64, String> {
    let mappings = load_mappings();
    def.job_path = to_canonical_path(&def.job_path, &mappings);
    let result = state.column_config_manager.add_column(&def)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let def_json = serde_json::to_string(&def).unwrap_or_default();
        let change = serde_json::json!({"action": "col_add", "def": def_json});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(result)
}

#[tauri::command]
pub async fn update_column(state: State<'_, AppState>, mut def: ColumnDefinition) -> Result<(), String> {
    let mappings = load_mappings();
    def.job_path = to_canonical_path(&def.job_path, &mappings);
    state.column_config_manager.update_column(&def)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let def_json = serde_json::to_string(&def).unwrap_or_default();
        let change = serde_json::json!({"action": "col_update", "def": def_json});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_column(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.column_config_manager.delete_column(id)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "col_delete", "id": id});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(())
}

// ── Column Presets ──

#[tauri::command]
pub fn get_column_presets(state: State<AppState>) -> Result<Vec<ColumnPreset>, String> {
    state.column_config_manager.get_column_presets()
}

#[tauri::command]
pub async fn save_column_preset(
    state: State<'_, AppState>,
    preset_name: String,
    column: ColumnDefinition,
) -> Result<i64, String> {
    let result = state.column_config_manager.save_column_preset(&preset_name, &column)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "preset_save", "preset_name": preset_name});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(result)
}

#[tauri::command]
pub async fn delete_column_preset(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.column_config_manager.delete_column_preset(id)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "preset_delete", "id": id});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(())
}

#[tauri::command]
pub async fn add_preset_column(
    state: State<'_, AppState>,
    preset_id: i64,
    job_path: String,
    folder_name: String,
) -> Result<i64, String> {
    let canonical_job = to_canonical_path(&job_path, &load_mappings());
    let col_id = state.column_config_manager.add_preset_column(preset_id, &canonical_job, &folder_name)?;
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "preset_add", "preset_id": preset_id, "job_path": canonical_job, "folder_name": folder_name});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }
    Ok(col_id)
}

// ── Bookmarks ──

#[tauri::command]
pub fn get_bookmarks(state: State<AppState>) -> Result<Vec<Bookmark>, String> {
    let mappings = load_mappings();
    let mut bookmarks = state.bookmark_manager.get_all_bookmarks()?;
    for bm in &mut bookmarks {
        bm.path = from_canonical_path(&bm.path, &mappings);
    }
    Ok(bookmarks)
}

#[tauri::command]
pub fn add_bookmark(
    state: State<AppState>,
    path: String,
    display_name: String,
    is_project_folder: bool,
) -> Result<Bookmark, String> {
    let mappings = load_mappings();
    let canonical_path = to_canonical_path(&path, &mappings);
    let mut result = state.bookmark_manager.add_bookmark(&canonical_path, &display_name, is_project_folder)?;
    result.path = from_canonical_path(&result.path, &mappings);
    #[cfg(windows)]
    sync_explorer_pins(&state);
    Ok(result)
}

#[tauri::command]
pub fn remove_bookmark(state: State<AppState>, path: String) -> Result<(), String> {
    let canonical_path = to_canonical_path(&path, &load_mappings());
    let result = state.bookmark_manager.remove_bookmark(&canonical_path);
    #[cfg(windows)]
    sync_explorer_pins(&state);
    result
}

#[cfg(windows)]
fn sync_explorer_pins(state: &AppState) {
    let pins = crate::explorer_pins::collect_nav_pins(state);
    if let Err(e) = crate::explorer_pins::sync_nav_pins(&pins) {
        log::warn!("Failed to sync Explorer nav pins: {}", e);
    }
}

// ── File Operations ──

#[tauri::command]
pub async fn list_directory(path: String) -> Result<Vec<FileEntry>, String> {
    let result = tokio::task::spawn_blocking(move || {
        crate::file_ops::list_directory(&path)
    })
    .await
    .map_err(|e| format!("Directory listing task failed: {}", e))?;
    result
}

#[tauri::command]
pub fn create_directory(path: String) -> Result<(), String> {
    crate::file_ops::create_directory(&path)
}

#[tauri::command]
pub fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    crate::file_ops::rename_path(&old_path, &new_path)
}

/// Generate a unique operation ID for progress tracking.
fn new_op_id(prefix: &str) -> String {
    format!(
        "{}_{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}

/// Run a copy operation on a blocking thread with progress events.
async fn do_copy_with_progress(
    app: &tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<(), String> {
    let op_id = new_op_id("copy");
    let _ = app.emit(
        "fileop:started",
        serde_json::json!({ "id": &op_id, "operation": "copy", "itemsTotal": sources.len() }),
    );

    let app2 = app.clone();
    let op_id2 = op_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let dest_path = std::path::Path::new(&dest);
        let total = sources.len();
        let mut last_emit = std::time::Instant::now();
        let mut errors: Vec<String> = Vec::new();
        let mut succeeded: usize = 0;

        for (i, src) in sources.iter().enumerate() {
            let src_path = std::path::Path::new(src);
            let file_name = match src_path.file_name() {
                Some(f) => f,
                None => {
                    errors.push(format!("Invalid source path: {}", src));
                    continue;
                }
            };
            let target = dest_path.join(file_name);

            // Sync-aware: copy within sync root creates a dehydrated placeholder (instant)
            match crate::sync_aware::sync_copy(src_path, dest_path) {
                Ok(true) => { succeeded += 1; continue; }
                Ok(false) => {} // Not in sync root, fall through to normal copy
                Err(e) => { errors.push(e); continue; }
            }

            let item_result = if src_path.is_dir() {
                let mut options = fs_extra::dir::CopyOptions::new();
                options.overwrite = true;
                options.copy_inside = true;
                let app3 = app2.clone();
                let op_id3 = op_id2.clone();
                fs_extra::dir::copy_with_progress(src_path, &target, &options, |info| {
                    if last_emit.elapsed().as_millis() >= 100 {
                        let _ = app3.emit(
                            "fileop:progress",
                            serde_json::json!({
                                "id": &op_id3,
                                "totalBytes": info.total_bytes,
                                "copiedBytes": info.copied_bytes,
                                "currentFile": &info.file_name,
                                "itemsTotal": total,
                                "itemsDone": i,
                            }),
                        );
                        last_emit = std::time::Instant::now();
                    }
                    fs_extra::dir::TransitProcessResult::ContinueOrAbort
                })
                .map_err(|e| format!("Failed to copy dir '{}': {}", src, e))
                .map(|_| ())
            } else {
                let mut options = fs_extra::file::CopyOptions::new();
                options.overwrite = true;
                let app3 = app2.clone();
                let op_id3 = op_id2.clone();
                let fname = file_name.to_string_lossy().to_string();
                fs_extra::file::copy_with_progress(src_path, &target, &options, |info| {
                    if last_emit.elapsed().as_millis() >= 100 {
                        let _ = app3.emit(
                            "fileop:progress",
                            serde_json::json!({
                                "id": &op_id3,
                                "totalBytes": info.total_bytes,
                                "copiedBytes": info.copied_bytes,
                                "currentFile": &fname,
                                "itemsTotal": total,
                                "itemsDone": i,
                            }),
                        );
                        last_emit = std::time::Instant::now();
                    }
                })
                .map_err(|e| format!("Failed to copy file '{}': {}", src, e))
                .map(|_| ())
            };

            match item_result {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(e),
            }
        }
        (succeeded, errors)
    })
    .await
    .map_err(|e| format!("Copy task failed: {}", e))?;

    let (succeeded, errors) = result;
    let failed = errors.len();

    if failed > 0 && succeeded == 0 {
        let _ = app.emit(
            "fileop:error",
            serde_json::json!({ "id": &op_id, "error": errors.join("; ") }),
        );
        return Err(errors.join("; "));
    }

    let _ = app.emit(
        "fileop:completed",
        serde_json::json!({
            "id": &op_id,
            "succeeded": succeeded,
            "failed": failed,
            "errors": errors,
        }),
    );

    Ok(())
}

/// Run a move operation on a blocking thread with progress events.
async fn do_move_with_progress(
    app: &tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<(), String> {
    let op_id = new_op_id("move");
    let _ = app.emit(
        "fileop:started",
        serde_json::json!({ "id": &op_id, "operation": "move", "itemsTotal": sources.len() }),
    );

    let app2 = app.clone();
    let op_id2 = op_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let dest_path = std::path::Path::new(&dest);
        let total = sources.len();
        let mut last_emit = std::time::Instant::now();
        let mut errors: Vec<String> = Vec::new();
        let mut succeeded: usize = 0;

        for (i, src) in sources.iter().enumerate() {
            let src_path = std::path::Path::new(src);
            let file_name = match src_path.file_name() {
                Some(f) => f,
                None => {
                    errors.push(format!("Invalid source path: {}", src));
                    continue;
                }
            };
            let target = dest_path.join(file_name);

            // Sync-aware: move within sync root uses fs::rename (instant)
            match crate::sync_aware::sync_move(src_path, dest_path) {
                Ok(true) => { succeeded += 1; continue; }
                Ok(false) => {} // Not in sync root, fall through to normal move
                Err(e) => { errors.push(e); continue; }
            }

            let item_result = if src_path.is_dir() {
                let mut options = fs_extra::dir::CopyOptions::new();
                options.overwrite = true;
                options.copy_inside = true;
                let app3 = app2.clone();
                let op_id3 = op_id2.clone();
                fs_extra::dir::move_dir_with_progress(src_path, &target, &options, |info| {
                    if last_emit.elapsed().as_millis() >= 100 {
                        let _ = app3.emit(
                            "fileop:progress",
                            serde_json::json!({
                                "id": &op_id3,
                                "totalBytes": info.total_bytes,
                                "copiedBytes": info.copied_bytes,
                                "currentFile": &info.file_name,
                                "itemsTotal": total,
                                "itemsDone": i,
                            }),
                        );
                        last_emit = std::time::Instant::now();
                    }
                    fs_extra::dir::TransitProcessResult::ContinueOrAbort
                })
                .map_err(|e| format!("Failed to move dir '{}': {}", src, e))
                .map(|_| ())
            } else {
                let mut options = fs_extra::file::CopyOptions::new();
                options.overwrite = true;
                let app3 = app2.clone();
                let op_id3 = op_id2.clone();
                let fname = file_name.to_string_lossy().to_string();
                fs_extra::file::move_file_with_progress(src_path, &target, &options, |info| {
                    if last_emit.elapsed().as_millis() >= 100 {
                        let _ = app3.emit(
                            "fileop:progress",
                            serde_json::json!({
                                "id": &op_id3,
                                "totalBytes": info.total_bytes,
                                "copiedBytes": info.copied_bytes,
                                "currentFile": &fname,
                                "itemsTotal": total,
                                "itemsDone": i,
                            }),
                        );
                        last_emit = std::time::Instant::now();
                    }
                })
                .map_err(|e| format!("Failed to move file '{}': {}", src, e))
                .map(|_| ())
            };

            match item_result {
                Ok(()) => succeeded += 1,
                Err(e) => errors.push(e),
            }
        }
        (succeeded, errors)
    })
    .await
    .map_err(|e| format!("Move task failed: {}", e))?;

    let (succeeded, errors) = result;
    let failed = errors.len();

    if failed > 0 && succeeded == 0 {
        let _ = app.emit(
            "fileop:error",
            serde_json::json!({ "id": &op_id, "error": errors.join("; ") }),
        );
        return Err(errors.join("; "));
    }

    let _ = app.emit(
        "fileop:completed",
        serde_json::json!({
            "id": &op_id,
            "succeeded": succeeded,
            "failed": failed,
            "errors": errors,
        }),
    );

    Ok(())
}

#[tauri::command]
pub async fn delete_to_trash(app: tauri::AppHandle, paths: Vec<String>) -> Result<(), String> {
    let op_id = new_op_id("delete");
    let _ = app.emit(
        "fileop:started",
        serde_json::json!({ "id": &op_id, "operation": "delete", "itemsTotal": paths.len() }),
    );

    let app2 = app.clone();
    let op_id2 = op_id.clone();

    let result = tokio::task::spawn_blocking(move || {
        let total = paths.len();
        let mut errors: Vec<String> = Vec::new();
        let mut succeeded: usize = 0;
        let mut trash_failed: Vec<String> = Vec::new();

        // Phase 1: try trash::delete per item (fast path), emit progress
        for (i, path) in paths.iter().enumerate() {
            let _ = app2.emit(
                "fileop:progress",
                serde_json::json!({
                    "id": &op_id2,
                    "itemsTotal": total,
                    "itemsDone": i,
                    "currentFile": path,
                }),
            );

            if crate::file_ops::try_trash_one(path).is_ok() {
                succeeded += 1;
            } else {
                trash_failed.push(path.clone());
            }
        }

        // Phase 2: batch fallback for all paths that couldn't be recycled
        // (single SHFileOperationW call on Windows instead of one per file)
        if !trash_failed.is_empty() {
            match crate::file_ops::fallback_delete(&trash_failed) {
                Ok(()) => succeeded += trash_failed.len(),
                Err(e) => {
                    for path in &trash_failed {
                        errors.push(format!("{}: {}", path, e));
                    }
                }
            }
        }

        (succeeded, errors)
    })
    .await
    .map_err(|e| format!("Delete task failed: {}", e))?;

    let (succeeded, errors) = result;
    let failed = errors.len();

    if failed > 0 && succeeded == 0 {
        let _ = app.emit(
            "fileop:error",
            serde_json::json!({ "id": &op_id, "error": errors.join("; ") }),
        );
        return Err(errors.join("; "));
    }

    let _ = app.emit(
        "fileop:completed",
        serde_json::json!({
            "id": &op_id,
            "succeeded": succeeded,
            "failed": failed,
            "errors": errors,
        }),
    );

    Ok(())
}

#[tauri::command]
pub async fn copy_files(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<(), String> {
    do_copy_with_progress(&app, sources, dest).await
}

#[tauri::command]
pub async fn move_files(
    app: tauri::AppHandle,
    sources: Vec<String>,
    dest: String,
) -> Result<(), String> {
    do_move_with_progress(&app, sources, dest).await
}

#[tauri::command]
pub fn clipboard_copy_paths(paths: Vec<String>) -> Result<(), String> {
    crate::file_ops::clipboard_copy_paths(&paths)
}

#[tauri::command]
pub async fn clipboard_paste(
    app: tauri::AppHandle,
    dest: String,
) -> Result<Vec<String>, String> {
    // Read clipboard paths (fast — usually instant)
    let paths = tokio::task::spawn_blocking(|| crate::file_ops::clipboard_paste_paths())
        .await
        .map_err(|e| format!("Clipboard task failed: {}", e))??;

    if !paths.is_empty() {
        do_copy_with_progress(&app, paths.clone(), dest).await?;
    }
    Ok(paths)
}

#[tauri::command]
pub fn reveal_in_file_manager(path: String) -> Result<(), String> {
    crate::file_ops::reveal_in_file_manager(&path)
}

#[tauri::command]
pub fn show_shell_context_menu(path: String) -> Result<(), String> {
    crate::shell_context_menu::show_shell_context_menu(&path)
}

#[tauri::command]
pub fn open_file(path: String) -> Result<(), String> {
    crate::file_ops::open_file(&path)
}

// ── Search ──

#[tauri::command]
pub fn search_files(query: String, scope_path: Option<String>) -> Result<Vec<FileEntry>, String> {
    crate::search::search_files(&query, scope_path.as_deref())
}

// ── Project Config ──

#[tauri::command]
pub fn load_project_config(job_path: String) -> Result<ProjectConfig, String> {
    ProjectConfig::load_for_job(&job_path)
}

#[tauri::command]
pub fn get_folder_type_config(
    job_path: String,
    folder_type: String,
) -> Result<Option<FolderTypeConfig>, String> {
    let config = ProjectConfig::load_for_job(&job_path)?;
    Ok(config.get_folder_type_config(&folder_type).cloned())
}

// ── Settings ──

#[tauri::command]
pub fn load_settings() -> Result<AppSettings, String> {
    Ok(AppSettings::load())
}

#[tauri::command]
pub fn save_settings(settings: AppSettings) -> Result<(), String> {
    settings.save()
}

// ── Mesh Sync ──

#[tauri::command]
pub async fn get_mesh_status(
    state: State<'_, AppState>,
) -> Result<crate::mesh_sync::MeshSyncStatus, String> {
    let lock = state.mesh_sync_manager.lock().await;
    match lock.as_ref() {
        Some(mesh) => Ok(mesh.get_status()),
        None => Ok(crate::mesh_sync::MeshSyncStatus {
            is_leader: false,
            leader_id: String::new(),
            peer_count: 0,
            last_snapshot_time: None,
            pending_edits_count: 0,
            status_message: "Not configured".to_string(),
            is_enabled: false,
            is_configured: false,
        }),
    }
}

#[tauri::command]
pub async fn set_mesh_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let lock = state.mesh_sync_manager.lock().await;
    if let Some(ref mesh) = *lock {
        mesh.set_enabled(enabled).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn trigger_flush_edits(state: State<'_, AppState>) -> Result<(), String> {
    let lock = state.mesh_sync_manager.lock().await;
    if let Some(ref mesh) = *lock {
        mesh.trigger_flush_edits().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn trigger_snapshot(state: State<'_, AppState>) -> Result<(), String> {
    let lock = state.mesh_sync_manager.lock().await;
    if let Some(ref mesh) = *lock {
        mesh.trigger_snapshot().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn reinit_mesh_sync(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.reinit_mesh_sync(app_handle).await;
    Ok(())
}

#[tauri::command]
pub async fn get_mesh_peers(
    state: State<'_, AppState>,
) -> Result<Vec<crate::peer_manager::PeerInfo>, String> {
    let lock = state.mesh_sync_manager.lock().await;
    match lock.as_ref() {
        Some(mesh) => Ok(mesh.peer_manager().get_peers()),
        None => Ok(vec![]),
    }
}

// ── URI / Links ──

/// Build a ufb:/// URI from a native path. Includes OS prefix.
#[tauri::command]
pub fn build_ufb_uri(path: String) -> String {
    crate::utils::build_path_uri(&path)
}

/// Build a union:/// URI from a native path. Includes OS prefix.
#[tauri::command]
pub fn build_union_uri(path: String) -> String {
    crate::utils::build_union_uri(&path)
}

/// Resolve a ufb:/// or union:/// URI to a local native path.
/// Uses path mappings from settings to translate cross-OS paths.
#[tauri::command]
pub fn resolve_ufb_uri(uri: String) -> Result<String, String> {
    let parsed = crate::utils::parse_path_uri(&uri)
        .ok_or_else(|| "Invalid URI format".to_string())?;

    let settings = crate::settings::AppSettings::load();
    let local_path = crate::utils::translate_path(
        &parsed.source_os,
        &parsed.path,
        &settings.path_mappings,
    );
    Ok(local_path)
}

// ── Special Paths ──

#[tauri::command]
pub fn get_special_paths() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Some(p) = dirs::home_dir() {
        map.insert("home".into(), p.to_string_lossy().to_string());
    }
    if let Some(p) = dirs::desktop_dir() {
        map.insert("desktop".into(), p.to_string_lossy().to_string());
    }
    if let Some(p) = dirs::document_dir() {
        map.insert("documents".into(), p.to_string_lossy().to_string());
    }
    if let Some(p) = dirs::download_dir() {
        map.insert("downloads".into(), p.to_string_lossy().to_string());
    }
    map
}

/// List available drive letters (Windows) or mount points.
#[tauri::command]
pub fn get_drives() -> Vec<(String, String)> {
    let mut drives = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let no_drives = read_no_drives_mask();
        for letter in b'A'..=b'Z' {
            let bit = (letter - b'A') as u32;
            if no_drives & (1 << bit) != 0 {
                continue; // Hidden via NoDrives policy
            }
            let root = format!("{}:\\", letter as char);
            let path = std::path::Path::new(&root);
            if path.exists() {
                let label = format!("{}: Drive", letter as char);
                drives.push((root, label));
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // On Linux/macOS, show common mount points
        for mount in &["/", "/home"] {
            if std::path::Path::new(mount).exists() {
                drives.push((mount.to_string(), mount.to_string()));
            }
        }
        // Also list /Volumes/* on macOS and /mnt/* on Linux
        for parent in &["/Volumes", "/mnt", "/media"] {
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        let p = entry.path().to_string_lossy().to_string();
                        let name = entry.file_name().to_string_lossy().to_string();
                        drives.push((p, name));
                    }
                }
            }
        }
        // Parse /proc/mounts for CIFS/NFS mounts not under /mnt or /media
        #[cfg(target_os = "linux")]
        if let Ok(mounts_content) = std::fs::read_to_string("/proc/mounts") {
            let known_paths: std::collections::HashSet<String> =
                drives.iter().map(|(p, _)| p.clone()).collect();
            for line in mounts_content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let fs_type = parts[2];
                    let mount_point = parts[1];
                    if (fs_type == "cifs" || fs_type == "nfs" || fs_type == "nfs4")
                        && !known_paths.contains(mount_point)
                    {
                        let source = parts[0];
                        let label = format!("{} ({})", mount_point, source);
                        drives.push((mount_point.to_string(), label));
                    }
                }
            }
        }
    }
    drives
}

// ── Dialogs ──

#[tauri::command]
pub async fn pick_folder(title: Option<String>) -> Result<Option<String>, String> {
    let result = tokio::task::spawn_blocking(move || {
        let mut dialog = rfd::FileDialog::new();
        if let Some(t) = title {
            dialog = dialog.set_title(t);
        }
        dialog.pick_folder()
    })
    .await
    .map_err(|e| format!("Dialog task failed: {}", e))?;

    Ok(result.map(|p| p.to_string_lossy().to_string()))
}

// ── Drag ──

/// Start a native OS drag operation with the given file paths.
/// Windows: DoDragDrop is blocking — runs as a sync command on the main thread.
/// macOS: Uses the `drag` crate, dispatched to main thread via app.run_on_main_thread.
#[tauri::command]
pub async fn start_native_drag(
    #[allow(unused)] app: tauri::AppHandle,
    #[allow(unused)] window: tauri::WebviewWindow,
    paths: Vec<String>,
) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    {
        // DoDragDrop is blocking but pumps its own message loop — safe to call
        // from the main UI thread. Must NOT run on the async runtime (blocks it).
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let window_clone = window.clone();
        let paths_clone = paths.clone();

        app.run_on_main_thread(move || {
            let result = crate::drag_out::start_native_drag(&window_clone, &paths_clone);
            let _ = tx.send(result);
        }).map_err(|e| format!("Failed to dispatch to main thread: {}", e))?;

        rx.recv().map_err(|e| format!("Drag thread error: {}", e))?
    }
    #[cfg(target_os = "macos")]
    {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let window_clone = window.clone();
        let paths_clone = paths.clone();

        app.run_on_main_thread(move || {
            let result = crate::drag_out::start_native_drag(&window_clone, &paths_clone);
            let _ = tx.send(result);
        }).map_err(|e| format!("Failed to dispatch to main thread: {}", e))?;

        rx.recv().map_err(|e| format!("Drag thread error: {}", e))?
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (app, window);
        Err("Native drag not implemented on this platform".into())
    }
}

// ── Backup ──

#[tauri::command]
pub fn list_backups(
    state: State<AppState>,
    job_path: String,
) -> Result<Vec<crate::backup::BackupInfo>, String> {
    state.backup_manager.list_backups(&job_path)
}

#[tauri::command]
pub fn restore_backup(
    state: State<AppState>,
    job_path: String,
    filename: String,
) -> Result<String, String> {
    state.backup_manager.restore_backup(&job_path, &filename)
}

/// Flush metadata writes — no-op since writes are now immediate.
/// Kept for frontend API compatibility.
#[tauri::command]
pub fn flush_metadata_writes(_state: State<AppState>) -> Result<usize, String> {
    Ok(0)
}

// ── Thumbnails ──

/// Get a thumbnail for a file as base64-encoded PNG.
/// Returns null if the file type isn't supported.
/// Uses a semaphore to limit concurrent extractions (12 max).
#[tauri::command]
pub async fn get_thumbnail(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<Option<String>, String> {
    match state.thumbnail_manager.get_or_generate_async(file_path).await? {
        Some(png_bytes) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(Some(format!("data:image/png;base64,{}", b64)))
        }
        None => Ok(None),
    }
}

// ── Sync Cache ──

#[tauri::command]
pub async fn mount_clear_sync_cache(
    state: State<'_, AppState>,
    mount_id: String,
) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::ClearSyncCache(
            crate::mount_client::MountIdMsg {
                mount_id,
                command_id: String::new(),
            },
        ))
        .await
}

/// Tell the agent to launch an elevated instance for symlink creation.
#[tauri::command]
pub async fn mount_create_symlinks(state: State<'_, AppState>) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::CreateSymlinks)
        .await
}

/// User-driven freshness signal — fired on window focus, F5/Ctrl+R, refresh
/// buttons, project tab activation. Forwards to the agent, which posts the
/// platform's freshness signal (Darwin notification on macOS so the
/// FileProvider extension signals .workingSet, surfacing any drift the
/// agent's opportunistic hooks have detected).
///
/// `domain` optionally narrows the sweep to a single share; `None` sweeps
/// all enabled mounts.
#[tauri::command]
pub async fn trigger_freshness_sweep(
    state: State<'_, AppState>,
    domain: Option<String>,
) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::FreshnessSweep(
            crate::mount_client::FreshnessSweepMsg {
                domain,
                command_id: String::new(),
            },
        ))
        .await
}

// ── System Icons ──

/// Get the OS-native file type icon as a base64-encoded PNG data URL.
/// Cached by extension — only one OS API call per unique extension.
#[tauri::command]
pub fn get_system_icon(
    state: State<'_, AppState>,
    extension: String,
    size: u32,
) -> Result<Option<String>, String> {
    match state.system_icon_cache.get_icon(&extension, size)? {
        Some(png_bytes) => {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
            Ok(Some(format!("data:image/png;base64,{}", b64)))
        }
        None => Ok(None),
    }
}

// ── Transcode ──

#[tauri::command]
pub async fn transcode_add_jobs(
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<Vec<crate::transcode::TranscodeJob>, String> {
    Ok(state.transcode_manager.add_jobs(paths).await)
}

#[tauri::command]
pub async fn transcode_get_queue(
    state: State<'_, AppState>,
) -> Result<Vec<crate::transcode::TranscodeJob>, String> {
    Ok(state.transcode_manager.get_queue().await)
}

#[tauri::command]
pub async fn transcode_cancel_job(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    state.transcode_manager.cancel_job(&id).await
}

#[tauri::command]
pub async fn transcode_remove_job(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    state.transcode_manager.remove_job(&id).await;
    Ok(())
}

#[tauri::command]
pub async fn transcode_clear_completed(
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.transcode_manager.clear_completed().await;
    Ok(())
}

// ── App lifecycle ──

#[tauri::command]
pub async fn relaunch_app(app: tauri::AppHandle) -> Result<(), String> {
    app.restart();
}

// ── Platform ──

#[tauri::command]
pub fn get_platform() -> String {
    crate::utils::current_os_tag().to_string()
}

/// Mount an SMB share via `mount -t cifs` with pkexec for elevation (Linux).
/// Returns the local mount path (under ~/.local/share/ufb/mnt/<share>/).
#[tauri::command]
pub fn mount_smb_share(host: String, share: String, username: String, password: String) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        // Build mount point under ~/.local/share/ufb/mnt/<share>
        let data_dir = crate::utils::get_app_data_dir();
        let mount_dir = data_dir.join("mnt").join(&share);
        std::fs::create_dir_all(&mount_dir)
            .map_err(|e| format!("Failed to create mount dir: {}", e))?;

        let mount_path = mount_dir.to_string_lossy().to_string();
        let unc = format!("//{}/{}", host, share);

        // Check if already mounted
        if let Ok(mounts) = std::fs::read_to_string("/proc/mounts") {
            for line in mounts.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts[1] == mount_path {
                    log::info!("Share {} already mounted at {}", unc, mount_path);
                    return Ok(mount_path);
                }
            }
        }

        // Write temporary credentials file (readable only by us)
        let cred_path = data_dir.join(".smb_cred_tmp");
        let cred_content = format!("username={}\npassword={}\n", username, password);
        std::fs::write(&cred_path, &cred_content)
            .map_err(|e| format!("Failed to write credentials file: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600));
        }

        let uid = unsafe { libc::getuid() }.to_string();
        let gid = unsafe { libc::getgid() }.to_string();

        let mount_opts = format!(
            "credentials={},uid={},gid={},file_mode=0644,dir_mode=0755,vers=3.0,rw",
            cred_path.display(), uid, gid
        );

        // Use pkexec for GUI sudo prompt
        let output = std::process::Command::new("pkexec")
            .args(["mount", "-t", "cifs", &unc, &mount_path, "-o", &mount_opts])
            .output();

        // Clean up credentials file immediately
        let _ = std::fs::remove_file(&cred_path);

        match output {
            Ok(o) if o.status.success() => {
                log::info!("Mounted {} at {}", unc, mount_path);
                Ok(mount_path)
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                Err(format!("mount failed: {}", stderr.trim()))
            }
            Err(e) => Err(format!("Failed to run pkexec mount: {}", e)),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (host, share, username, password);
        Err("SMB mounting not yet implemented on macOS. Use Finder > Go > Connect to Server to mount SMB shares.".into())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (host, share, username, password);
        Err("SMB mounting is not available on this platform".into())
    }
}

// ── Deep Link ──

#[tauri::command]
pub fn get_pending_deep_link(state: tauri::State<'_, crate::app_state::AppState>) -> Option<String> {
    state.pending_deep_link.lock().unwrap().take()
}

// ── Item Creation ──

/// Resolve the bundled project template directory.
fn find_bundled_template_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let candidates = [
        // Windows / dev: next to exe
        exe_dir.join("assets/projectTemplate"),
        // Dev build: one level up
        exe_dir.join("../assets/projectTemplate"),
        // macOS .app bundle: Contents/Resources/
        exe_dir.join("../Resources/assets/projectTemplate"),
    ];
    candidates.into_iter().find(|p| p.is_dir())
}

/// Detect the "add item" mode for a folder by inspecting the bundled project template.
/// Returns: "shot" (has _t_* template), "date_prefixed" (has 000000* placeholder),
/// "none" (audition), or "folder" (plain directory).
#[tauri::command]
pub fn get_folder_add_mode(folder_name: String) -> String {
    if folder_name.to_lowercase() == "audition" {
        return "none".to_string();
    }

    let Some(template_root) = find_bundled_template_dir() else {
        return "folder".to_string();
    };

    // Case-insensitive folder match in template
    let folder_dir = match std::fs::read_dir(&template_root) {
        Ok(entries) => {
            let lower = folder_name.to_lowercase();
            entries
                .filter_map(|e| e.ok())
                .find(|e| e.file_name().to_string_lossy().to_lowercase() == lower)
                .map(|e| e.path())
        }
        Err(_) => None,
    };

    let Some(folder_dir) = folder_dir else {
        return "folder".to_string();
    };

    if !folder_dir.is_dir() {
        return "folder".to_string();
    }

    // Scan children for _t_* (shot template) or 000000* (date-prefixed placeholder)
    if let Ok(children) = std::fs::read_dir(&folder_dir) {
        for child in children.filter_map(|e| e.ok()) {
            let name = child.file_name().to_string_lossy().to_string();
            if name.starts_with("_t_") && child.path().is_dir() {
                return "shot".to_string();
            }
            if name.starts_with("000000") && child.path().is_dir() {
                return "date_prefixed".to_string();
            }
        }
    }

    "folder".to_string()
}

/// Detect whether a folder should use layout mode "C" (shot/asset grid) or "B" (basic browser).
/// Always scans children for shot-like subdirectory patterns to confirm layout.
/// Config flags (isShot/isAsset) alone don't force Mode C — the items must actually
/// contain project/renders-like subdirectories.
#[tauri::command]
pub fn detect_folder_layout_mode(job_path: String, folder_name: String) -> Result<String, String> {
    // Scan children for shot/asset-like directory patterns
    let folder_path = std::path::Path::new(&job_path).join(&folder_name);
    if folder_path.is_dir() {
        let entries = std::fs::read_dir(&folder_path)
            .map_err(|e| format!("Failed to read directory: {}", e))?;

        // Common subdirectory names found inside shot/asset items
        const PROJECT_NAMES: &[&str] = &[
            "project", "projects", "scenes", "scene", "work", "source", "src",
            "maya", "houdini", "nuke", "ae", "c4d", "blender", "flame", "fusion",
            "resolve", "premiere", "aftereffects", "pfx", "matchmove", "roto",
        ];
        const RENDER_NAMES: &[&str] = &[
            "renders", "render", "output", "outputs", "comp", "comps",
            "export", "exports", "deliverables", "plates", "precomp",
        ];

        let mut checked = 0;
        let mut hits = 0;

        for entry in entries {
            if checked >= 10 {
                break;
            }
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip hidden/dot dirs
            if entry.file_name().to_str().map_or(false, |n| n.starts_with('.')) {
                continue;
            }
            checked += 1;

            // Check if this child dir has subdirs matching shot-like patterns
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                let sub_names: Vec<String> = sub_entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().to_str().map(|s| s.to_lowercase()))
                    .collect();

                let found_project = sub_names.iter().any(|n| PROJECT_NAMES.contains(&n.as_str()));
                let found_render = sub_names.iter().any(|n| RENDER_NAMES.contains(&n.as_str()));

                // Either a project+render combo, or at least 2 recognized subdirs
                if found_project && found_render {
                    return Ok("C".to_string());
                }
                if found_project || found_render {
                    hits += 1;
                }
            }
        }

        // If multiple children have at least one recognized subdir, likely a shot folder
        if hits >= 2 {
            return Ok("C".to_string());
        }
    }

    Ok("B".to_string())
}

/// Recursively find files named "template.*" or "_template.*" and rename them
/// to "{item_name}_v001.{ext}".
fn rename_template_files(dir: &std::path::Path, item_name: &str) {
    let walker = match std::fs::read_dir(dir) {
        Ok(w) => w,
        Err(_) => return,
    };
    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            rename_template_files(&path, item_name);
        } else if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
            let lower = fname.to_lowercase();
            let is_template = lower.starts_with("template.") || lower.starts_with("_template.");
            if is_template {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let new_name = format!("{}_v001.{}", item_name, ext);
                    let new_path = path.with_file_name(&new_name);
                    let _ = std::fs::rename(&path, &new_path);
                }
            }
        }
    }
}

/// Create a new item directory from a template or as an empty folder.
/// First checks projectConfig for a template path, then falls back to the
/// bundled project template (looks for a `_t_*` subdirectory).
#[tauri::command]
pub fn create_item_from_template(
    job_path: String,
    folder_path: String,
    item_name: String,
) -> Result<String, String> {
    let folder_path_obj = std::path::Path::new(&folder_path);
    let folder_name = folder_path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let dest = folder_path_obj.join(&item_name);
    let dest_str = dest.to_string_lossy().to_string();

    // 1. Try projectConfig template (if the job has one)
    let mut template_dir: Option<std::path::PathBuf> = None;
    if let Ok(config) = ProjectConfig::load_for_job(&job_path) {
        if let Some(ftc) = config.get_folder_type_config(folder_name) {
            if !ftc.add_action_template_file.is_empty() {
                let p = std::path::PathBuf::from(&ftc.add_action_template_file);
                if p.is_dir() { template_dir = Some(p); }
            } else if !ftc.add_action_template.is_empty() {
                let p = std::path::Path::new(&job_path).join(&ftc.add_action_template);
                if p.is_dir() { template_dir = Some(p); }
            }
        }
    }

    // 2. Fall back to bundled template: look for _t_* subdir in projectTemplate/{folder_name}/
    if template_dir.is_none() {
        if let Some(bundled_root) = find_bundled_template_dir() {
            // Case-insensitive match
            let lower = folder_name.to_lowercase();
            if let Ok(entries) = std::fs::read_dir(&bundled_root) {
                for entry in entries.filter_map(|e| e.ok()) {
                    if entry.file_name().to_string_lossy().to_lowercase() == lower {
                        if let Ok(children) = std::fs::read_dir(entry.path()) {
                            for child in children.filter_map(|e| e.ok()) {
                                let name = child.file_name().to_string_lossy().to_string();
                                if name.starts_with("_t_") && child.path().is_dir() {
                                    template_dir = Some(child.path());
                                    break;
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    if let Some(template) = template_dir {
        let mut opts = fs_extra::dir::CopyOptions::new();
        opts.copy_inside = true;
        opts.content_only = true;
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("Failed to create destination directory: {}", e))?;
        fs_extra::dir::copy(&template, &dest, &opts)
            .map_err(|e| format!("Failed to copy template: {}", e))?;

        // Rename template files: template.ext or _template.ext → {item_name}_v001.ext
        rename_template_files(&dest, &item_name);
    } else {
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    Ok(dest_str)
}

/// Create a new job folder from the bundled project template.
/// Copies `assets/projectTemplate/` into `{parent_path}/{job_number}_{job_name}/`,
/// then auto-subscribes to the new job.
#[tauri::command]
pub async fn create_job_from_template(
    state: State<'_, AppState>,
    parent_path: String,
    job_number: String,
    job_name: String,
) -> Result<String, String> {
    let folder_name = format!("{}_{}", job_number, job_name);
    let full_path = std::path::Path::new(&parent_path).join(&folder_name);
    let full_path_str = full_path.to_string_lossy().to_string();

    if full_path.exists() {
        return Err(format!("Folder already exists: {}", full_path_str));
    }

    // Resolve template directory
    let exe = std::env::current_exe()
        .map_err(|e| format!("Failed to get current exe: {}", e))?;
    let exe_dir = exe.parent().ok_or("Failed to get exe directory")?;

    let candidates = [
        // Windows / dev: next to exe
        exe_dir.join("assets/projectTemplate"),
        // Dev build: one level up
        exe_dir.join("../assets/projectTemplate"),
        // macOS .app bundle: Contents/Resources/
        exe_dir.join("../Resources/assets/projectTemplate"),
    ];
    let template_dir = candidates
        .iter()
        .find(|p| p.is_dir())
        .ok_or_else(|| "Project template directory not found".to_string())?;

    // Copy template tree
    std::fs::create_dir_all(&full_path)
        .map_err(|e| format!("Failed to create job directory: {}", e))?;
    let mut opts = fs_extra::dir::CopyOptions::new();
    opts.copy_inside = true;
    opts.content_only = true;
    fs_extra::dir::copy(template_dir, &full_path, &opts)
        .map_err(|e| format!("Failed to copy template: {}", e))?;

    // Auto-subscribe (store canonical path in DB)
    let canonical_path = to_canonical_path(&full_path_str, &load_mappings());
    state.subscription_manager.subscribe_to_job(&canonical_path, &folder_name)?;

    // Notify mesh sync
    if let Some(ref mesh) = *state.mesh_sync_manager.lock().await {
        let change = serde_json::json!({"action": "sub_add", "job_path": canonical_path, "job_name": folder_name});
        mesh.on_table_changed(&change.to_string()).await;
        mesh.mark_snapshot_needed();
    }

    Ok(full_path_str)
}

/// Create a date-prefixed item directory (for asset/posting folders).
/// Format: `{YYMMDD}{x}_{base_name}/` where x is a-z.
/// The suffix letter is based on ALL existing items with that date prefix,
/// not just items with the same base name. So if `271015a_foo` exists,
/// the next item is `271015b_bar` regardless of the name.
#[tauri::command]
pub fn create_date_prefixed_item(
    folder_path: String,
    base_name: String,
) -> Result<String, String> {
    let today = Local::now().format("%y%m%d").to_string();
    let folder = std::path::Path::new(&folder_path);

    if !folder.is_dir() {
        return Err(format!("Folder does not exist: {}", folder_path));
    }

    // Collect existing folder names that start with today's date prefix
    let existing: Vec<String> = std::fs::read_dir(folder)
        .map_err(|e| format!("Failed to read directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .filter(|n| n.starts_with(&today))
        .collect();

    // Find first letter suffix not used by any existing folder
    for suffix in b'a'..=b'z' {
        let suffix_char = suffix as char;
        let prefix = format!("{}{}", today, suffix_char);
        if !existing.iter().any(|n| n.starts_with(&prefix)) {
            let dir_name = format!("{}_{}", prefix, base_name);
            let full_path = folder.join(&dir_name);
            std::fs::create_dir_all(&full_path)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
            return Ok(full_path.to_string_lossy().to_string());
        }
    }

    Err(format!(
        "All date-prefixed slots for {} are taken (a-z exhausted)",
        today
    ))
}

// ── Mount (MediaMount Agent) ──

/// Credential info returned by mount_list_credential_keys (never includes passwords).
#[derive(serde::Serialize)]
pub struct CredentialInfo {
    pub key: String,
    pub username: String,
}

const CRED_PREFIX: &str = "mediamount_";

/// Ensure a credential key has the mediamount_ prefix.
fn normalize_cred_key(key: &str) -> String {
    if key.starts_with(CRED_PREFIX) {
        key.to_string()
    } else {
        format!("{}{}", CRED_PREFIX, key)
    }
}

/// Strip the mediamount_ prefix for display.
fn strip_cred_prefix(key: &str) -> String {
    key.strip_prefix(CRED_PREFIX).unwrap_or(key).to_string()
}

#[tauri::command]
pub fn mount_list_credential_keys() -> Result<Vec<CredentialInfo>, String> {
    #[cfg(windows)]
    {
        list_credential_keys_windows()
    }
    #[cfg(not(windows))]
    {
        let cred_path = if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(".local/share/ufb/credentials.json")
        } else {
            return Ok(vec![]);
        };
        let data: std::collections::HashMap<String, serde_json::Value> =
            std::fs::read_to_string(&cred_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        let mut results = Vec::new();
        for (key, val) in &data {
            if key.starts_with(CRED_PREFIX) {
                let username = val.get("u").and_then(|v| v.as_str()).unwrap_or("").to_string();
                results.push(CredentialInfo {
                    key: strip_cred_prefix(key),
                    username,
                });
            }
        }
        results.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(results)
    }
}

#[cfg(windows)]
fn list_credential_keys_windows() -> Result<Vec<CredentialInfo>, String> {
    use windows::Win32::Security::Credentials::{
        CredEnumerateW, CredFree, CREDENTIALW, CRED_ENUMERATE_FLAGS,
    };

    let filter: Vec<u16> = "mediamount_*\0".encode_utf16().collect();
    let mut count: u32 = 0;
    let mut creds_ptr: *mut *mut CREDENTIALW = std::ptr::null_mut();

    let result = unsafe {
        CredEnumerateW(
            windows::core::PCWSTR(filter.as_ptr()),
            CRED_ENUMERATE_FLAGS(0),
            &mut count,
            &mut creds_ptr,
        )
    };

    match result {
        Ok(()) => {
            let mut results = Vec::new();
            for i in 0..count as isize {
                let cred = unsafe { &**creds_ptr.offset(i) };
                let target = unsafe { cred.TargetName.to_string() }.unwrap_or_default();
                let username = unsafe { cred.UserName.to_string() }.unwrap_or_default();
                results.push(CredentialInfo {
                    key: strip_cred_prefix(&target),
                    username,
                });
            }
            unsafe { CredFree(creds_ptr as *const _) };
            results.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(results)
        }
        Err(_) => Ok(vec![]), // No credentials found
    }
}

#[tauri::command]
pub async fn mount_get_states(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, crate::mount_client::MountStateUpdateMsg>, String> {
    Ok(state.mount_client.get_states().await)
}

#[tauri::command]
pub async fn mount_is_connected(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.mount_client.is_connected().await)
}

#[tauri::command]
pub async fn mount_restart(state: State<'_, AppState>, mount_id: String) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::RestartMount(
            crate::mount_client::MountIdMsg {
                mount_id,
                command_id: uuid::Uuid::new_v4().to_string(),
            },
        ))
        .await
}

#[tauri::command]
pub async fn mount_start(state: State<'_, AppState>, mount_id: String) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::StartMount(
            crate::mount_client::MountIdMsg {
                mount_id,
                command_id: uuid::Uuid::new_v4().to_string(),
            },
        ))
        .await
}

#[tauri::command]
pub async fn mount_stop(state: State<'_, AppState>, mount_id: String) -> Result<(), String> {
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::StopMount(
            crate::mount_client::MountIdMsg {
                mount_id,
                command_id: uuid::Uuid::new_v4().to_string(),
            },
        ))
        .await
}

#[tauri::command]
pub async fn mount_save_config(
    state: State<'_, AppState>,
    config: crate::mount_client::MountsConfig,
) -> Result<(), String> {
    crate::mount_client::save_mount_config(&config)?;
    // Tell agent to reload
    state
        .mount_client
        .send_command(crate::mount_client::UfbToAgent::ReloadConfig)
        .await
}

#[tauri::command]
pub fn mount_get_config() -> Result<crate::mount_client::MountsConfig, String> {
    Ok(crate::mount_client::load_mount_config())
}

#[tauri::command]
pub fn mount_launch_agent() -> Result<(), String> {
    let agent_name = if cfg!(target_os = "windows") {
        "mediamount-agent.exe"
    } else {
        "mediamount-agent"
    };

    // Search order: dev build path (when running from target/), next to UFB exe,
    // bundle Resources, then PATH.  Dev path is checked first so a freshly-built
    // agent always wins over a stale copy that may linger next to the UFB exe.
    let mut agent_path = std::path::PathBuf::from(agent_name);

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dir_str = dir.to_string_lossy();
            let is_dev = dir_str.contains("target/debug") || dir_str.contains("target\\debug")
                || dir_str.contains("target/release") || dir_str.contains("target\\release");

            // Dev: UFB is in src-tauri/target/debug/,
            // agent is in mediamount-agent/target/debug/
            if is_dev {
                let dev_path = dir.join("../../../mediamount-agent/target/debug").join(agent_name);
                if let Ok(canon) = std::fs::canonicalize(&dev_path) {
                    agent_path = canon;
                }
            }

            // Production: next to UFB exe (skip if dev path already found)
            if !agent_path.exists() || agent_path == std::path::PathBuf::from(agent_name) {
                let sidecar = dir.join(agent_name);
                if sidecar.exists() {
                    agent_path = sidecar;
                }
            }

            // macOS .app bundle: Contents/MacOS/ or Contents/Resources/
            #[cfg(target_os = "macos")]
            if !agent_path.exists() || agent_path == std::path::PathBuf::from(agent_name) {
                if let Some(contents_dir) = dir.parent() {
                    let resources = contents_dir.join("Resources").join(agent_name);
                    if resources.exists() {
                        agent_path = resources;
                    }
                }
            }
        }
    }

    log::info!("Launching agent: {}", agent_path.display());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new(&agent_path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Failed to launch agent at {}: {}", agent_path.display(), e))?;
    }

    #[cfg(not(windows))]
    {
        std::process::Command::new(&agent_path)
            .spawn()
            .map_err(|e| format!("Failed to launch agent at {}: {}", agent_path.display(), e))?;
    }

    // macOS: also launch the Swift tray companion app if available
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                // Production: UFB.app in Contents/Resources/
                let tray_candidates = [
                    dir.join("../Resources/UFB.app"),
                    dir.join("../Resources/MediaMountTray.app"), // legacy fallback
                ];
                for tray_path in &tray_candidates {
                    if tray_path.exists() {
                        log::info!("Launching tray app: {}", tray_path.display());
                        let _ = std::process::Command::new("open")
                            .arg(tray_path)
                            .spawn();
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub fn mount_store_credentials(
    key: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let key = normalize_cred_key(&key);
    #[cfg(windows)]
    {
        store_credentials_windows(&key, &username, &password)
    }
    #[cfg(not(windows))]
    {
        // File-based credential store matching the agent's format
        // Stored in ~/.local/share/ufb/credentials.json (chmod 600)
        let cred_path = {
            let dir = if let Some(home) = std::env::var_os("HOME") {
                std::path::PathBuf::from(home).join(".local/share/ufb")
            } else {
                crate::utils::get_app_data_dir()
            };
            let _ = std::fs::create_dir_all(&dir);
            dir.join("credentials.json")
        };
        let mut data: std::collections::HashMap<String, serde_json::Value> =
            std::fs::read_to_string(&cred_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        data.insert(key.clone(), serde_json::json!({ "u": username, "p": password }));
        let json = serde_json::to_string_pretty(&data)
            .map_err(|e| format!("Failed to serialize: {}", e))?;
        std::fs::write(&cred_path, &json)
            .map_err(|e| format!("Failed to write credentials: {}", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600));
        }
        log::info!("Stored credentials for key: {}", key);
        Ok(())
    }
}

#[tauri::command]
pub fn mount_has_credentials(key: String) -> Result<bool, String> {
    let key = normalize_cred_key(&key);
    #[cfg(windows)]
    {
        has_credentials_windows(&key)
    }
    #[cfg(not(windows))]
    {
        let cred_path = if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(".local/share/ufb/credentials.json")
        } else {
            return Ok(false);
        };
        let data: std::collections::HashMap<String, serde_json::Value> =
            std::fs::read_to_string(&cred_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        Ok(data.contains_key(&key))
    }
}

#[tauri::command]
pub fn mount_delete_credentials(key: String) -> Result<(), String> {
    let key = normalize_cred_key(&key);
    #[cfg(windows)]
    {
        delete_credentials_windows(&key)
    }
    #[cfg(not(windows))]
    {
        let cred_path = if let Some(home) = std::env::var_os("HOME") {
            std::path::PathBuf::from(home).join(".local/share/ufb/credentials.json")
        } else {
            return Ok(());
        };
        let mut data: std::collections::HashMap<String, serde_json::Value> =
            std::fs::read_to_string(&cred_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        data.remove(&key);
        let json = serde_json::to_string_pretty(&data).unwrap_or_default();
        let _ = std::fs::write(&cred_path, &json);
        Ok(())
    }
}

#[cfg(windows)]
fn store_credentials_windows(key: &str, username: &str, password: &str) -> Result<(), String> {
    use windows::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();
    let user: Vec<u16> = format!("{}\0", username).encode_utf16().collect();
    let pass_bytes: Vec<u8> = password.as_bytes().to_vec();

    let cred = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: windows::core::PWSTR(target.as_ptr() as *mut _),
        UserName: windows::core::PWSTR(user.as_ptr() as *mut _),
        CredentialBlobSize: pass_bytes.len() as u32,
        CredentialBlob: pass_bytes.as_ptr() as *mut _,
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };

    unsafe {
        CredWriteW(&cred, 0)
            .map_err(|e| format!("Failed to store credentials: {}", e))?;
    }

    log::info!("Stored credentials for key: {}", key);
    Ok(())
}

#[cfg(windows)]
fn has_credentials_windows(key: &str) -> Result<bool, String> {
    use windows::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();
    let mut cred_ptr: *mut CREDENTIALW = std::ptr::null_mut();

    let result = unsafe {
        CredReadW(
            windows::core::PCWSTR(target.as_ptr()),
            CRED_TYPE_GENERIC,
            0,
            &mut cred_ptr,
        )
    };

    match result {
        Ok(()) => {
            unsafe { CredFree(cred_ptr as *const _) };
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

#[cfg(windows)]
fn delete_credentials_windows(key: &str) -> Result<(), String> {
    use windows::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

    let target: Vec<u16> = format!("{}\0", key).encode_utf16().collect();

    unsafe {
        CredDeleteW(
            windows::core::PCWSTR(target.as_ptr()),
            CRED_TYPE_GENERIC,
            0,
        )
        .map_err(|e| format!("Failed to delete credentials: {}", e))?;
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn read_no_drives_mask() -> u32 {
    use std::os::windows::process::CommandExt;
    let output = std::process::Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\Explorer",
            "/v",
            "NoDrives",
        ])
        .creation_flags(0x08000000)
        .output();
    if let Ok(o) = output {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            if line.contains("NoDrives") {
                if let Some(hex) = line.split_whitespace().last() {
                    if let Some(stripped) = hex.strip_prefix("0x") {
                        return u32::from_str_radix(stripped, 16).unwrap_or(0);
                    }
                    return hex.parse::<u32>().unwrap_or(0);
                }
            }
        }
    }
    0
}

#[tauri::command]
pub fn mount_hide_drives(letters: Vec<String>) -> Result<(), String> {
    #[cfg(windows)]
    {
        hide_drives_elevated(&letters, true)
    }
    #[cfg(not(windows))]
    {
        let _ = letters;
        Err("Not supported on this platform".into())
    }
}

#[tauri::command]
pub fn mount_unhide_drives(letters: Vec<String>) -> Result<(), String> {
    #[cfg(windows)]
    {
        hide_drives_elevated(&letters, false)
    }
    #[cfg(not(windows))]
    {
        let _ = letters;
        Err("Not supported on this platform".into())
    }
}

#[cfg(windows)]
fn hide_drives_elevated(letters: &[String], hide: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let mut bit_ops = Vec::new();
    for letter in letters {
        if let Some(ch) = letter.chars().next() {
            let upper = ch.to_ascii_uppercase();
            if upper.is_ascii_uppercase() {
                let bit = (upper as u32) - ('A' as u32);
                if hide {
                    bit_ops.push(format!("$mask = $mask -bor (1 -shl {})", bit));
                } else {
                    bit_ops.push(format!("$mask = $mask -band (-bnot (1 -shl {}))", bit));
                }
            }
        }
    }

    if bit_ops.is_empty() {
        return Ok(());
    }

    let script = format!(
        "$key = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Policies\\Explorer'\n\
         if (-not (Test-Path $key)) {{ New-Item -Path $key -Force | Out-Null }}\n\
         $current = (Get-ItemProperty -Path $key -Name NoDrives -ErrorAction SilentlyContinue).NoDrives\n\
         if ($null -eq $current) {{ $current = 0 }}\n\
         $mask = [int]$current\n\
         {}\n\
         if ($mask -eq 0) {{ Remove-ItemProperty -Path $key -Name NoDrives -ErrorAction SilentlyContinue }}\n\
         else {{ Set-ItemProperty -Path $key -Name NoDrives -Value $mask -Type DWord }}\n",
        bit_ops.join("\n")
    );

    // Write script to a temp file to avoid quoting hell
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("ufb_nodrives.ps1");
    std::fs::write(&script_path, &script)
        .map_err(|e| format!("Failed to write temp script: {}", e))?;

    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-WindowStyle", "Hidden",
            "-Command",
            &format!(
                "Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile','-WindowStyle','Hidden','-ExecutionPolicy','Bypass','-File','{}'",
                script_path.to_string_lossy()
            ),
        ])
        .creation_flags(0x08000000)
        .status()
        .map_err(|e| format!("Failed to launch elevated PowerShell: {}", e))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&script_path);

    if status.success() {
        log::info!("Drive visibility changed (hide={}) for {:?}", hide, letters);
        Ok(())
    } else {
        Err("Elevated command failed or was cancelled".into())
    }
}

