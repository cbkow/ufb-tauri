pub mod app_state;
pub mod backup;
pub mod drag_out;
pub mod bookmarks;
pub mod columns;
pub mod commands;
pub mod db;
pub mod file_ops;
pub mod http_server;
pub mod mesh_sync;
pub mod metadata;
pub mod peer_manager;
pub mod project_config;
pub mod search;
pub mod settings;
pub mod subscription;
pub mod thumbnails;
pub mod udp_notify;
pub mod mount_client;
pub mod explorer_pins;
pub mod shell_context_menu;
pub mod sync_aware;
pub mod system_icons;
pub mod transcode;
pub mod utils;

use app_state::AppState;
use settings::AppSettings;
use tauri::Emitter;
use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;

/// Resolve a union:// URI to a local path and open it in the file manager.
fn handle_union_uri(uri: &str) {
    let Some(parsed) = crate::utils::parse_path_uri(uri) else {
        log::warn!("Failed to parse union URI: {}", uri);
        return;
    };
    let settings = AppSettings::load();
    let local_path = crate::utils::translate_path(
        &parsed.source_os,
        &parsed.path,
        &settings.path_mappings,
    );
    log::info!("union:// → revealing: {}", local_path);
    if let Err(e) = crate::file_ops::reveal_in_file_manager(&local_path) {
        log::error!("Failed to reveal path from union:// link: {}", e);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Initialize app state
    let state = AppState::initialize().expect("Failed to initialize app state");

    // Load settings and init mesh sync (sync part — creates manager)
    // init_mesh_sync auto-populates nodeId/port and saves back to disk
    let mut settings = AppSettings::load();
    state.init_mesh_sync(&mut settings);

    // Clone settings for async startup
    let settings_for_startup = settings.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // argv[1] is the deep-link URI from the OS
            if let Some(uri) = argv.get(1) {
                if uri.starts_with("union://") {
                    // union:// links open the file manager — no need to focus the app window
                    handle_union_uri(uri);
                    return;
                }
                if uri.starts_with("ufb://") {
                    let _ = app.emit("deep-link-uri", uri.clone());
                }
            }
            // Focus existing window
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .setup(move |app| {
            // Deep-link listener for URIs arriving while app is running (or during cold start on macOS,
            // where the OS delivers the URL via Apple Events, not CLI args).
            let handle_dl = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                if let Some(url) = event.urls().first() {
                    let uri = url.to_string();
                    if uri.starts_with("union://") {
                        handle_union_uri(&uri);
                    } else if uri.starts_with("ufb://") {
                        // Store in pending_deep_link so the frontend can fetch it on mount
                        // (covers macOS cold start where webview isn't ready yet)
                        let state: tauri::State<'_, AppState> = handle_dl.state();
                        *state.pending_deep_link.lock().unwrap() = Some(uri.clone());
                        // Also emit for the case where the app is already running
                        let _ = handle_dl.emit("deep-link-uri", uri);
                    }
                }
            });

            // Cold-start: app launched directly via a deep link (first instance)
            if let Some(uri) = std::env::args().nth(1) {
                if uri.starts_with("union://") {
                    // union:// links just open the file manager — don't navigate in-app
                    handle_union_uri(&uri);
                } else if uri.starts_with("ufb://") {
                    // Store in AppState so frontend can fetch it on mount
                    let state: tauri::State<'_, AppState> = app.state();
                    *state.pending_deep_link.lock().unwrap() = Some(uri.clone());

                    // Also emit after a short delay for the event listener
                    let handle_cs = app.handle().clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        let _ = handle_cs.emit("deep-link-uri", uri);
                    });
                }
            }

            // Enable mesh sync after the async runtime is available
            let app_handle = app.handle().clone();
            let settings = settings_for_startup;
            tauri::async_runtime::spawn(async move {
                let state: tauri::State<'_, AppState> = app_handle.state();
                state.set_mesh_app_handle(app_handle.clone()).await;
                state.enable_mesh_sync_if_configured(&settings).await;
                state.set_transcode_app_handle(app_handle.clone()).await;

                // Auto-launch the mediamount-agent if not already running
                if !state.mount_client.is_agent_running() {
                    log::info!("Agent not running, auto-launching...");
                    if let Err(e) = crate::commands::mount_launch_agent() {
                        log::warn!("Failed to auto-launch agent: {}", e);
                    }
                }

                state.mount_client.start(app_handle.clone());

                // Sync Explorer nav pane pins in background (spawns reg.exe processes)
                #[cfg(windows)]
                {
                    let pins = crate::explorer_pins::collect_nav_pins(&state);
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = crate::explorer_pins::sync_nav_pins(&pins) {
                            log::warn!("Failed to sync Explorer nav pins: {}", e);
                        }
                    });
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Subscriptions
            commands::subscribe_to_job,
            commands::unsubscribe_from_job,
            commands::get_subscriptions,
            // Metadata
            commands::get_item_metadata,
            commands::upsert_item_metadata,
            commands::get_tracked_items,
            commands::get_all_tracked_items,
            commands::get_folder_metadata,
            commands::flush_metadata_writes,
            // Columns
            commands::get_column_defs,
            commands::add_column,
            commands::update_column,
            commands::delete_column,
            // Column Presets
            commands::get_column_presets,
            commands::save_column_preset,
            commands::delete_column_preset,
            commands::add_preset_column,
            // Bookmarks
            commands::get_bookmarks,
            commands::add_bookmark,
            commands::remove_bookmark,
            // File operations
            commands::list_directory,
            commands::create_directory,
            commands::rename_path,
            commands::delete_to_trash,
            commands::copy_files,
            commands::move_files,
            commands::clipboard_copy_paths,
            commands::clipboard_paste,
            commands::reveal_in_file_manager,
            commands::show_shell_context_menu,
            commands::open_file,
            // Search
            commands::search_files,
            // Config
            commands::load_project_config,
            commands::get_folder_type_config,
            commands::load_settings,
            commands::save_settings,
            // Mesh sync
            commands::get_mesh_status,
            commands::set_mesh_enabled,
            commands::trigger_flush_edits,
            commands::trigger_snapshot,
            commands::reinit_mesh_sync,
            commands::get_mesh_peers,
            // URI / Links
            commands::build_ufb_uri,
            commands::build_union_uri,
            commands::resolve_ufb_uri,
            // Special paths
            commands::get_special_paths,
            commands::get_drives,
            // Dialogs
            commands::pick_folder,
            // Drag
            commands::start_native_drag,
            // Backup
            commands::list_backups,
            commands::restore_backup,
            // Thumbnails
            commands::get_thumbnail,
            // System icons
            commands::get_system_icon,
            // Item creation
            commands::get_folder_add_mode,
            commands::detect_folder_layout_mode,
            commands::create_item_from_template,
            commands::create_date_prefixed_item,
            commands::create_job_from_template,
            // Transcode
            commands::transcode_add_jobs,
            commands::transcode_get_queue,
            commands::transcode_cancel_job,
            commands::transcode_remove_job,
            commands::transcode_clear_completed,
            // Mount (MediaMount Agent)
            commands::mount_get_states,
            commands::mount_is_connected,
            commands::mount_restart,
            commands::mount_start,
            commands::mount_stop,
            commands::mount_save_config,
            commands::mount_get_config,
            commands::mount_launch_agent,
            commands::mount_list_credential_keys,
            commands::mount_store_credentials,
            commands::mount_has_credentials,
            commands::mount_delete_credentials,
            commands::mount_hide_drives,
            commands::mount_unhide_drives,
            commands::mount_clear_sync_cache,
            commands::mount_create_symlinks,
            commands::trigger_freshness_sweep,
            // App lifecycle
            commands::relaunch_app,
            // Platform
            commands::get_platform,
            commands::mount_smb_share,
            // Deep link
            commands::get_pending_deep_link,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Graceful mesh sync shutdown
                let state: tauri::State<'_, AppState> = app_handle.state();
                tauri::async_runtime::block_on(async {
                    state.shutdown_mesh_sync().await;
                });
            }
        });
}
