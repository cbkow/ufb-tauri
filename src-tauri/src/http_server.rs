use crate::columns::ColumnConfigManager;
use crate::db::Database;
use crate::mesh_sync::SyncCommand;
use crate::peer_manager::PeerManager;
use axum::extract::{Path as AxumPath, State as AxumState};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Shared state for the HTTP server.
pub struct HttpState {
    pub db: Arc<Database>,
    pub api_secret: String,
    pub node_id: String,
    pub tags: Vec<String>,
    pub peer_manager: Arc<PeerManager>,
    pub is_leader: Arc<AtomicBool>,
    pub last_snapshot_time: Arc<AtomicU64>,
    pub enabled: Arc<AtomicBool>,
    pub command_tx: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<SyncCommand>>>,
    pub column_config_manager: Arc<ColumnConfigManager>,
    pub app_handle: Option<tauri::AppHandle>,
}

pub struct MeshHttpServer {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

impl MeshHttpServer {
    /// Start the HTTP server on the given port.
    pub fn start(port: u16, state: Arc<HttpState>) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let app = Router::new()
            .route("/api/status", get(handle_status))
            .route("/api/metadata/update", post(handle_metadata_update))
            .route("/api/metadata/{job_path}", get(handle_metadata_get))
            .route("/api/metadata/batch", post(handle_metadata_batch))
            .route("/api/table/update", post(handle_table_update))
            .route("/api/snapshot/notify", post(handle_snapshot_notify))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

        let task_handle = tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    log::error!("Failed to bind HTTP server on port {}: {}", port, e);
                    return;
                }
            };
            log::info!("Mesh HTTP server listening on port {}", port);

            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap_or_else(|e| log::error!("HTTP server error: {}", e));

            log::info!("Mesh HTTP server stopped");
        });

        Self {
            shutdown_tx: Some(shutdown_tx),
            task_handle: Some(task_handle),
        }
    }

    /// Signal shutdown and wait for the server to stop.
    pub async fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.task_handle.take() {
            let _ = handle.await;
        }
    }
}

// ── Auth helper ──

fn check_auth(state: &HttpState, headers: &HeaderMap) -> Result<(), StatusCode> {
    if state.api_secret.is_empty() {
        return Ok(()); // Open access
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", state.api_secret);
    if auth == expected {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

// ── Route handlers ──

/// GET /api/status — no auth required. Wire-compatible with C++.
#[derive(Serialize)]
struct StatusResponse {
    node_id: String,
    is_leader: bool,
    peer_count: usize,
    last_snapshot_time: u64,
    enabled: bool,
    tags: Vec<String>,
}

async fn handle_status(AxumState(state): AxumState<Arc<HttpState>>) -> Json<StatusResponse> {
    Json(StatusResponse {
        node_id: state.node_id.clone(),
        is_leader: state.is_leader.load(Ordering::Relaxed),
        peer_count: state.peer_manager.get_peer_count(),
        last_snapshot_time: state.last_snapshot_time.load(Ordering::Relaxed),
        enabled: state.enabled.load(Ordering::Relaxed),
        tags: state.tags.clone(),
    })
}

/// POST /api/metadata/update — Bearer auth. Upserts metadata, queues BroadcastEdit if leader.
#[derive(Deserialize)]
struct MetadataUpdateRequest {
    job_path: String,
    item_path: String,
    metadata: String,
    folder_name: String,
    #[serde(default)]
    is_tracked: bool,
}

async fn handle_metadata_update(
    AxumState(state): AxumState<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<MetadataUpdateRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_auth(&state, &headers)?;
    log::info!("HTTP: Received metadata update for {} (leader={})", body.item_path, state.is_leader.load(Ordering::Relaxed));

    // Upsert to local DB
    let result = state.db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO item_metadata (item_path, job_path, folder_name, metadata_json, is_tracked, modified_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(item_path) DO UPDATE SET
                 metadata_json = excluded.metadata_json,
                 is_tracked = excluded.is_tracked,
                 modified_time = excluded.modified_time",
            rusqlite::params![
                body.item_path,
                body.job_path,
                body.folder_name,
                body.metadata,
                body.is_tracked as i64,
                crate::utils::current_time_ms(),
            ],
        )?;
        Ok(())
    });

    if let Err(e) = result {
        log::error!("Failed to upsert metadata from peer: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Emit Tauri event so frontend refreshes metadata
    if let Some(ref handle) = state.app_handle {
        use tauri::Emitter;
        let _ = handle.emit("mesh:metadata-changed", serde_json::json!({
            "job_path": body.job_path,
            "item_path": body.item_path,
            "folder_name": body.folder_name,
        }));
    }

    // If leader, broadcast to other peers
    if state.is_leader.load(Ordering::Relaxed) {
        let _ = state.command_tx.lock().await.send(SyncCommand::BroadcastEdit {
            job_path: body.job_path,
            item_path: body.item_path,
            metadata_json: body.metadata,
            folder_name: body.folder_name,
            is_tracked: body.is_tracked,
        });
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}

/// GET /api/metadata/:jobPath — Bearer auth. Returns metadata records for a job.
async fn handle_metadata_get(
    AxumState(state): AxumState<Arc<HttpState>>,
    headers: HeaderMap,
    AxumPath(job_path): AxumPath<String>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    check_auth(&state, &headers)?;

    let decoded = urlencoding::decode(&job_path).map(|s| s.into_owned()).unwrap_or(job_path);

    let result = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT item_path, job_path, folder_name, metadata_json, is_tracked, modified_time
             FROM item_metadata WHERE job_path = ?1",
        )?;
        let rows = stmt.query_map([decoded.as_str()], |row| {
            Ok(serde_json::json!({
                "item_path": row.get::<_, String>(0)?,
                "job_path": row.get::<_, String>(1)?,
                "folder_name": row.get::<_, String>(2)?,
                "metadata": row.get::<_, String>(3)?,
                "is_tracked": row.get::<_, i64>(4)? != 0,
                "modified_time": row.get::<_, Option<i64>>(5)?,
            }))
        })?;
        let mut records = Vec::new();
        for row in rows {
            if let Ok(val) = row {
                records.push(val);
            }
        }
        Ok(records)
    });

    match result {
        Ok(records) => Ok(Json(records)),
        Err(e) => {
            log::error!("Failed to query metadata: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// POST /api/metadata/batch — Bearer auth. Same as GET but via POST body.
#[derive(Deserialize)]
struct BatchRequest {
    job_path: String,
    #[serde(default)]
    _since: Option<i64>,
}

async fn handle_metadata_batch(
    AxumState(state): AxumState<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<BatchRequest>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    check_auth(&state, &headers)?;

    let result = state.db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT item_path, job_path, folder_name, metadata_json, is_tracked, modified_time
             FROM item_metadata WHERE job_path = ?1",
        )?;
        let rows = stmt.query_map([&body.job_path], |row| {
            Ok(serde_json::json!({
                "item_path": row.get::<_, String>(0)?,
                "job_path": row.get::<_, String>(1)?,
                "folder_name": row.get::<_, String>(2)?,
                "metadata": row.get::<_, String>(3)?,
                "is_tracked": row.get::<_, i64>(4)? != 0,
                "modified_time": row.get::<_, Option<i64>>(5)?,
            }))
        })?;
        let mut records = Vec::new();
        for row in rows {
            if let Ok(val) = row {
                records.push(val);
            }
        }
        Ok(records)
    });

    match result {
        Ok(records) => Ok(Json(records)),
        Err(e) => {
            log::error!("Failed to query metadata batch: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// POST /api/table/update — Bearer auth. Applies subscription/column changes.
async fn handle_table_update(
    AxumState(state): AxumState<Arc<HttpState>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_auth(&state, &headers)?;

    let change_json = body.to_string();
    let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
    log::info!("HTTP: Received table update action={} (leader={})", action, state.is_leader.load(Ordering::Relaxed));

    // Apply to local DB
    if let Err(e) = crate::mesh_sync::apply_table_change(&state.db, &change_json) {
        log::error!("Failed to apply table change from peer: {}", e);
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Invalidate column caches if column operation
    if action.starts_with("col_") {
        state.column_config_manager.invalidate_all_caches();
    }

    // If leader, broadcast to other peers
    if state.is_leader.load(Ordering::Relaxed) {
        let _ = state.command_tx.lock().await.send(SyncCommand::BroadcastTableChange {
            change_json,
        });
    }

    // Emit Tauri event so frontend refreshes
    if let Some(ref handle) = state.app_handle {
        use tauri::Emitter;
        let _ = handle.emit("mesh:table-changed", &body);
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}

/// POST /api/snapshot/notify — Bearer auth. Queues RestoreSnapshot if not leader.
async fn handle_snapshot_notify(
    AxumState(state): AxumState<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    check_auth(&state, &headers)?;

    if !state.is_leader.load(Ordering::Relaxed) {
        log::info!("HTTP: Received snapshot notify, queuing restore");
        let _ = state.command_tx.lock().await.send(SyncCommand::RestoreSnapshot);
    } else {
        log::info!("HTTP: Ignoring snapshot notify (I am leader)");
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}
