use crate::columns::ColumnConfigManager;
use crate::db::Database;
use crate::http_server::{HttpState, MeshHttpServer};
use crate::peer_manager::{get_local_ip_for_farm, PeerManager};
use crate::udp_notify::UdpNotify;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

// Mesh sync constants (matching C++ mesh_config.h)
pub const DEFAULT_HTTP_PORT: u16 = 49200;
pub const DEFAULT_UDP_PORT: u16 = 4244;
pub const DEFAULT_MULTICAST_GROUP: &str = "239.42.0.2";
pub const MULTICAST_TTL: u32 = 1;

pub const PEER_LOOP_INTERVAL_MS: u64 = 3000;
pub const UDP_HEARTBEAT_INTERVAL_MS: u64 = 3000;
pub const UDP_SILENCE_THRESHOLD_MS: u64 = 15000;
pub const PEER_DEAD_POLL_COUNT: i32 = 3;
pub const SNAPSHOT_INTERVAL_MS: u64 = 30000;
pub const EDIT_QUEUE_FLUSH_INTERVAL_MS: u64 = 5000;
pub const STALE_FILE_CLEANUP_MS: u64 = 7 * 24 * 3600 * 1000;
pub const MAX_EDIT_RETRY_COUNT: i32 = 10;

// ── Sync commands ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncCommand {
    EditMetadata {
        job_path: String,
        item_path: String,
        metadata_json: String,
        folder_name: String,
        is_tracked: bool,
    },
    BroadcastEdit {
        job_path: String,
        item_path: String,
        metadata_json: String,
        folder_name: String,
        is_tracked: bool,
    },
    TableChange {
        change_json: String,
    },
    BroadcastTableChange {
        change_json: String,
    },
    TakeSnapshot,
    RestoreSnapshot,
    FlushEditQueue,
    Shutdown,
}

// ── Edit queue ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditQueueEntry {
    pub job_path: String,
    pub item_path: String,
    pub metadata_json: String,
    pub folder_name: String,
    pub is_tracked: bool,
    pub queued_time: i64,
    pub retry_count: i32,
}

// ── Status ──

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncStatus {
    pub is_leader: bool,
    pub leader_id: String,
    pub peer_count: usize,
    pub last_snapshot_time: Option<i64>,
    pub pending_edits_count: usize,
    pub status_message: String,
    pub is_enabled: bool,
    pub is_configured: bool,
}

// ── Manager ──

pub struct MeshSyncManager {
    configured: bool,
    peer_manager: Arc<PeerManager>,
    udp_notify: Arc<UdpNotify>,
    is_leader: Arc<AtomicBool>,
    snapshot_needed: Arc<AtomicBool>,
    last_snapshot_time: Arc<AtomicU64>,
    enabled: Arc<AtomicBool>,
    command_tx: Arc<tokio::sync::Mutex<mpsc::UnboundedSender<SyncCommand>>>,
    edit_queue: Arc<tokio::sync::Mutex<Vec<EditQueueEntry>>>,
    db: Arc<Database>,
    farm_path: String,
    node_id: String,
    http_port: u16,
    api_secret: String,
    tags: Vec<String>,
    column_config_manager: Arc<ColumnConfigManager>,
    app_handle: tokio::sync::Mutex<Option<tauri::AppHandle>>,
    // Task handles for shutdown
    task_handles: tokio::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    http_server: tokio::sync::Mutex<Option<MeshHttpServer>>,
}

impl MeshSyncManager {
    pub fn new(
        farm_path: String,
        node_id: String,
        http_port: u16,
        api_secret: String,
        tags: Vec<String>,
        db: Arc<Database>,
        column_config_manager: Arc<ColumnConfigManager>,
    ) -> Self {
        let configured = !farm_path.is_empty() && !node_id.is_empty();

        let peer_manager = Arc::new(PeerManager::new(
            farm_path.clone(),
            node_id.clone(),
            http_port,
            tags.clone(),
        ));

        let udp_notify = Arc::new(UdpNotify::new(
            node_id.clone(),
            DEFAULT_UDP_PORT,
            DEFAULT_MULTICAST_GROUP.to_string(),
        ));

        let (command_tx, _command_rx) = mpsc::unbounded_channel();

        Self {
            configured,
            peer_manager,
            udp_notify,
            is_leader: Arc::new(AtomicBool::new(false)),
            snapshot_needed: Arc::new(AtomicBool::new(true)),
            last_snapshot_time: Arc::new(AtomicU64::new(0)),
            enabled: Arc::new(AtomicBool::new(false)),
            command_tx: Arc::new(tokio::sync::Mutex::new(command_tx)),
            edit_queue: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            db,
            farm_path,
            node_id,
            http_port,
            api_secret,
            tags,
            column_config_manager,
            app_handle: tokio::sync::Mutex::new(None),
            task_handles: tokio::sync::Mutex::new(Vec::new()),
            http_server: tokio::sync::Mutex::new(None),
        }
    }

    /// Start all mesh sync background tasks.
    pub async fn set_enabled(&self, enabled: bool) {
        if enabled && !self.configured {
            log::warn!("Cannot enable mesh sync: not configured");
            return;
        }

        if enabled == self.enabled.load(Ordering::Relaxed) {
            return;
        }

        if enabled {
            self.enabled.store(true, Ordering::Relaxed);
            log::info!("Starting mesh sync (node: {}, farm: {})", self.node_id, self.farm_path);

            // Create fresh channel pair on each enable
            let (new_tx, rx) = mpsc::unbounded_channel();
            *self.command_tx.lock().await = new_tx;

            // 1. Start UDP socket
            if let Err(e) = self.udp_notify.start().await {
                log::error!("Failed to start UDP: {}", e);
            }

            // 2. Send immediate heartbeat (use farm-aware IP for correct interface)
            let ip = get_local_ip_for_farm(&self.farm_path);
            if let Err(e) = self.udp_notify.send_heartbeat(&ip, self.http_port, &self.tags).await {
                log::warn!("Failed to send initial heartbeat: {}", e);
            }

            // 3. Start HTTP server
            let http_state = Arc::new(HttpState {
                db: self.db.clone(),
                api_secret: self.api_secret.clone(),
                node_id: self.node_id.clone(),
                tags: self.tags.clone(),
                peer_manager: self.peer_manager.clone(),
                is_leader: self.is_leader.clone(),
                last_snapshot_time: self.last_snapshot_time.clone(),
                enabled: self.enabled.clone(),
                command_tx: self.command_tx.clone(),
                column_config_manager: self.column_config_manager.clone(),
                app_handle: self.app_handle.lock().await.clone(),
            });
            let server = MeshHttpServer::start(self.http_port, http_state);
            *self.http_server.lock().await = Some(server);

            // 4. Load edit queue from disk
            self.load_edit_queue().await;

            // 5. If snapshot exists, queue restore
            let snapshot_path = self.snapshot_path();
            if snapshot_path.exists() {
                let _ = self.command_tx.lock().await.send(SyncCommand::RestoreSnapshot);
            }

            // 6. Spawn sync worker task
            {
                let db = self.db.clone();
                let farm_path = self.farm_path.clone();
                let is_leader = self.is_leader.clone();
                let snapshot_needed = self.snapshot_needed.clone();
                let last_snapshot_time = self.last_snapshot_time.clone();
                let edit_queue = self.edit_queue.clone();
                let peer_manager = self.peer_manager.clone();
                let api_secret = self.api_secret.clone();
                let enabled = self.enabled.clone();
                let ccm = self.column_config_manager.clone();
                let app_handle = self.app_handle.lock().await.clone();

                let handle = tokio::spawn(async move {
                    sync_worker_loop(
                        rx, db, farm_path, is_leader, snapshot_needed,
                        last_snapshot_time, edit_queue, peer_manager, api_secret, enabled, ccm,
                        app_handle,
                    ).await;
                });
                self.task_handles.lock().await.push(handle);
            }

            // 7. Spawn peer discovery task
            {
                let pm = self.peer_manager.clone();
                let enabled = self.enabled.clone();
                let is_leader = self.is_leader.clone();
                let cmd_tx = self.command_tx.clone();
                let handle = tokio::spawn(async move {
                    let client = reqwest::Client::new();
                    let mut had_alive_peers = false;
                    loop {
                        if !enabled.load(Ordering::Relaxed) {
                            break;
                        }
                        // Register, discover, poll, elect
                        if let Err(e) = pm.register_endpoint() {
                            log::warn!("Failed to register endpoint: {}", e);
                        }
                        if let Err(e) = pm.discover_peers() {
                            log::warn!("Failed to discover peers: {}", e);
                        }
                        pm.poll_peers(&client).await;
                        let was_leader = is_leader.load(Ordering::Relaxed);
                        pm.run_election();
                        let now_leader = pm.is_leader();
                        is_leader.store(now_leader, Ordering::Relaxed);
                        if was_leader != now_leader {
                            log::info!("Leadership changed: now {} (peers: {})", if now_leader { "LEADER" } else { "FOLLOWER" }, pm.get_peer_count());
                        }

                        // Detect peer count changes for catch-up
                        let alive_count = pm.get_alive_peers().len();
                        let has_alive_peers = alive_count > 0;

                        if has_alive_peers && !had_alive_peers {
                            if now_leader {
                                // Leader: a peer just came back — ensure snapshot is fresh
                                log::info!("Sync: Peer(s) reappeared, triggering snapshot for catch-up");
                                let _ = cmd_tx.lock().await.send(SyncCommand::TakeSnapshot);
                            } else {
                                // Follower: re-restore snapshot from shared storage
                                log::info!("Sync: Peers reappeared after isolation, re-restoring snapshot");
                                let _ = cmd_tx.lock().await.send(SyncCommand::RestoreSnapshot);
                            }
                        }
                        had_alive_peers = has_alive_peers;

                        pm.cleanup_stale_peers();
                        tokio::time::sleep(std::time::Duration::from_millis(PEER_LOOP_INTERVAL_MS)).await;
                    }
                });
                self.task_handles.lock().await.push(handle);
            }

            // 8. Spawn UDP heartbeat/listener task
            {
                let udp = self.udp_notify.clone();
                let pm = self.peer_manager.clone();
                let enabled = self.enabled.clone();
                let http_port = self.http_port;
                let tags = self.tags.clone();
                let farm_path_for_udp = self.farm_path.clone();
                let handle = tokio::spawn(async move {
                    let mut heartbeat_interval = tokio::time::interval(
                        std::time::Duration::from_millis(UDP_HEARTBEAT_INTERVAL_MS),
                    );
                    let mut poll_interval = tokio::time::interval(
                        std::time::Duration::from_millis(200),
                    );

                    loop {
                        if !enabled.load(Ordering::Relaxed) {
                            break;
                        }

                        tokio::select! {
                            _ = heartbeat_interval.tick() => {
                                let ip = get_local_ip_for_farm(&farm_path_for_udp);
                                if let Err(e) = udp.send_heartbeat(&ip, http_port, &tags).await {
                                    log::warn!("Heartbeat send failed: {}", e);
                                }
                            }
                            _ = poll_interval.tick() => {
                                let messages = udp.poll().await;
                                for msg in messages {
                                    let msg_type = msg.get("t").and_then(|v| v.as_str()).unwrap_or("");
                                    match msg_type {
                                        "hb" => {
                                            let node_id = msg.get("n").or(msg.get("from"))
                                                .and_then(|v| v.as_str()).unwrap_or("");
                                            let ip = msg.get("ip").and_then(|v| v.as_str()).unwrap_or("");
                                            let port = msg.get("port").and_then(|v| v.as_u64()).unwrap_or(49200) as u16;
                                            let tags: Vec<String> = msg.get("tags")
                                                .and_then(|v| v.as_array())
                                                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                                                .unwrap_or_default();
                                            if !node_id.is_empty() {
                                                pm.process_heartbeat(node_id, ip, port, &tags);
                                            }
                                        }
                                        "bye" => {
                                            let node_id = msg.get("n").or(msg.get("from"))
                                                .and_then(|v| v.as_str()).unwrap_or("");
                                            if !node_id.is_empty() {
                                                pm.process_goodbye(node_id);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                });
                self.task_handles.lock().await.push(handle);
            }

            log::info!("Mesh sync started");
        } else {
            // Disable
            self.shutdown().await;
        }
    }

    /// Graceful shutdown of all mesh sync tasks.
    pub async fn shutdown(&self) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }

        log::info!("Shutting down mesh sync...");

        // Signal shutdown
        let _ = self.command_tx.lock().await.send(SyncCommand::Shutdown);
        self.enabled.store(false, Ordering::Relaxed);

        // Send UDP goodbye
        self.udp_notify.stop(&self.tags).await;

        // Stop HTTP server
        if let Some(mut server) = self.http_server.lock().await.take() {
            server.stop().await;
        }

        // Save edit queue to disk
        self.save_edit_queue().await;

        // Remove endpoint
        self.peer_manager.unregister_endpoint();

        // Abort all tasks
        let mut handles = self.task_handles.lock().await;
        for handle in handles.drain(..) {
            handle.abort();
        }

        log::info!("Mesh sync shutdown complete");
    }

    /// Queue a metadata edit for sync.
    pub async fn on_metadata_edited(
        &self,
        job_path: &str,
        item_path: &str,
        metadata_json: &str,
        folder_name: &str,
        is_tracked: bool,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            log::debug!("Sync: on_metadata_edited called but sync is disabled");
            return;
        }
        log::info!("Sync: on_metadata_edited queuing for {}", item_path);
        let _ = self.command_tx.lock().await.send(SyncCommand::EditMetadata {
            job_path: job_path.to_string(),
            item_path: item_path.to_string(),
            metadata_json: metadata_json.to_string(),
            folder_name: folder_name.to_string(),
            is_tracked,
        });
    }

    /// Queue a table change (subscription or column) for sync.
    pub async fn on_table_changed(&self, change_json: &str) {
        if !self.enabled.load(Ordering::Relaxed) { return; }
        log::info!("Sync: on_table_changed: {}", change_json);
        let _ = self.command_tx.lock().await.send(SyncCommand::TableChange {
            change_json: change_json.to_string(),
        });
    }

    pub async fn trigger_flush_edits(&self) {
        let _ = self.command_tx.lock().await.send(SyncCommand::FlushEditQueue);
    }

    pub async fn trigger_snapshot(&self) {
        let _ = self.command_tx.lock().await.send(SyncCommand::TakeSnapshot);
    }

    /// Mark that a snapshot is needed (e.g. after subscription/column changes).
    pub fn mark_snapshot_needed(&self) {
        self.snapshot_needed.store(true, Ordering::Relaxed);
    }

    pub fn get_status(&self) -> MeshSyncStatus {
        let pm = &self.peer_manager;
        let pending = self.edit_queue.try_lock().map(|q| q.len()).unwrap_or(0);
        let snap_time = self.last_snapshot_time.load(Ordering::Relaxed);
        MeshSyncStatus {
            is_leader: self.is_leader.load(Ordering::Relaxed),
            leader_id: pm.get_current_leader_id().unwrap_or_default(),
            peer_count: pm.get_peer_count(),
            last_snapshot_time: if snap_time > 0 { Some(snap_time as i64) } else { None },
            pending_edits_count: pending,
            status_message: if self.enabled.load(Ordering::Relaxed) {
                if self.is_leader.load(Ordering::Relaxed) {
                    "Leader".to_string()
                } else {
                    "Follower".to_string()
                }
            } else {
                "Disabled".to_string()
            },
            is_enabled: self.enabled.load(Ordering::Relaxed),
            is_configured: self.configured,
        }
    }

    pub fn peer_manager(&self) -> &Arc<PeerManager> {
        &self.peer_manager
    }

    pub fn farm_path(&self) -> &str {
        &self.farm_path
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    pub fn api_secret(&self) -> &str {
        &self.api_secret
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Set the Tauri app handle (called after app starts).
    pub async fn set_app_handle(&self, handle: tauri::AppHandle) {
        *self.app_handle.lock().await = Some(handle);
    }

    // ── Snapshot path ──

    fn snapshot_path(&self) -> PathBuf {
        Path::new(&self.farm_path)
            .join("snapshots")
            .join("ufb_snapshot_v2.db")
    }

    // ── Edit queue persistence ──

    fn edit_queue_path(&self) -> PathBuf {
        crate::utils::get_app_data_dir().join("edit_queue.json")
    }

    async fn save_edit_queue(&self) {
        let queue = self.edit_queue.lock().await;
        if queue.is_empty() {
            let _ = std::fs::remove_file(self.edit_queue_path());
            return;
        }
        match serde_json::to_string_pretty(&*queue) {
            Ok(json) => {
                if let Err(e) = std::fs::write(self.edit_queue_path(), json) {
                    log::error!("Failed to save edit queue: {}", e);
                }
            }
            Err(e) => log::error!("Failed to serialize edit queue: {}", e),
        }
    }

    async fn load_edit_queue(&self) {
        let path = self.edit_queue_path();
        if !path.exists() {
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(json) => {
                match serde_json::from_str::<Vec<EditQueueEntry>>(&json) {
                    Ok(entries) => {
                        let count = entries.len();
                        *self.edit_queue.lock().await = entries;
                        if count > 0 {
                            log::info!("Loaded {} entries from edit queue", count);
                        }
                    }
                    Err(e) => log::warn!("Failed to parse edit queue: {}", e),
                }
            }
            Err(e) => log::warn!("Failed to read edit queue: {}", e),
        }
    }
}

// ── Sync worker loop ──

async fn sync_worker_loop(
    mut rx: mpsc::UnboundedReceiver<SyncCommand>,
    db: Arc<Database>,
    farm_path: String,
    is_leader: Arc<AtomicBool>,
    snapshot_needed: Arc<AtomicBool>,
    last_snapshot_time: Arc<AtomicU64>,
    edit_queue: Arc<tokio::sync::Mutex<Vec<EditQueueEntry>>>,
    peer_manager: Arc<PeerManager>,
    api_secret: String,
    enabled: Arc<AtomicBool>,
    _column_config_manager: Arc<ColumnConfigManager>,
    app_handle: Option<tauri::AppHandle>,
) {
    let client = reqwest::Client::new();
    let mut last_snapshot_check = tokio::time::Instant::now();
    let mut last_flush_check = tokio::time::Instant::now();

    log::info!("Sync worker loop started");

    loop {
        tokio::select! {
            Some(cmd) = rx.recv() => {
                match cmd {
                    SyncCommand::EditMetadata { job_path, item_path, metadata_json, folder_name, is_tracked } => {
                        log::info!("Sync: EditMetadata for {} (leader={})", item_path, is_leader.load(Ordering::Relaxed));
                        if is_leader.load(Ordering::Relaxed) {
                            // Leader: broadcast to all peers
                            broadcast_edit(&client, &peer_manager, &api_secret, &job_path, &item_path, &metadata_json, &folder_name, is_tracked).await;
                            snapshot_needed.store(true, Ordering::Relaxed);
                        } else {
                            // Follower: POST to leader, on failure queue
                            if let Some(leader_ep) = peer_manager.get_leader_endpoint() {
                                let url = format!("http://{}:{}/api/metadata/update", leader_ep.ip, leader_ep.port);
                                log::info!("Sync: Follower posting edit to leader at {}", url);
                                let body = serde_json::json!({
                                    "job_path": job_path,
                                    "item_path": item_path,
                                    "metadata": metadata_json,
                                    "folder_name": folder_name,
                                    "is_tracked": is_tracked,
                                });
                                let mut req = client.post(&url).json(&body);
                                if !api_secret.is_empty() {
                                    req = req.bearer_auth(&api_secret);
                                }
                                match req.timeout(std::time::Duration::from_secs(5)).send().await {
                                    Ok(resp) if resp.status().is_success() => {
                                        log::info!("Sync: Edit sent to leader OK");
                                    }
                                    Ok(resp) => {
                                        log::warn!("Sync: Leader returned {}, queuing edit", resp.status());
                                        let mut queue = edit_queue.lock().await;
                                        queue.push(EditQueueEntry {
                                            job_path, item_path, metadata_json, folder_name, is_tracked,
                                            queued_time: crate::utils::current_time_ms(), retry_count: 0,
                                        });
                                    }
                                    Err(e) => {
                                        log::warn!("Sync: Failed to reach leader: {}, queuing edit", e);
                                        let mut queue = edit_queue.lock().await;
                                        queue.push(EditQueueEntry {
                                            job_path, item_path, metadata_json, folder_name, is_tracked,
                                            queued_time: crate::utils::current_time_ms(), retry_count: 0,
                                        });
                                    }
                                }
                            } else {
                                log::warn!("Sync: No leader known, queuing edit for {}", item_path);
                                let mut queue = edit_queue.lock().await;
                                queue.push(EditQueueEntry {
                                    job_path, item_path, metadata_json, folder_name, is_tracked,
                                    queued_time: crate::utils::current_time_ms(), retry_count: 0,
                                });
                            }
                        }
                    }
                    SyncCommand::BroadcastEdit { job_path, item_path, metadata_json, folder_name, is_tracked } => {
                        broadcast_edit(&client, &peer_manager, &api_secret, &job_path, &item_path, &metadata_json, &folder_name, is_tracked).await;
                        snapshot_needed.store(true, Ordering::Relaxed);
                    }
                    SyncCommand::TableChange { change_json } => {
                        // Local DB write already done by command handler — just forward/broadcast
                        handle_table_change(&client, &peer_manager, &api_secret, &is_leader, &snapshot_needed, &change_json).await;
                    }
                    SyncCommand::BroadcastTableChange { change_json } => {
                        // Local DB write already done by HTTP handler — just fan out to other peers
                        broadcast_table_change(&client, &peer_manager, &api_secret, &change_json).await;
                        snapshot_needed.store(true, Ordering::Relaxed);
                    }
                    SyncCommand::TakeSnapshot => {
                        snapshot_to_db(&db, &farm_path);
                        snapshot_needed.store(false, Ordering::Relaxed);
                        last_snapshot_time.store(crate::utils::current_time_ms() as u64, Ordering::Relaxed);

                        // Notify followers
                        notify_snapshot(&client, &peer_manager, &api_secret).await;
                    }
                    SyncCommand::RestoreSnapshot => {
                        if !is_leader.load(Ordering::Relaxed) {
                            restore_from_snapshot(&db, &farm_path);
                            // Notify frontend to reload everything
                            if let Some(ref handle) = app_handle {
                                use tauri::Emitter;
                                let _ = handle.emit("mesh:data-refreshed", ());
                            }
                        }
                    }
                    SyncCommand::FlushEditQueue => {
                        flush_edit_queue(&client, &edit_queue, &peer_manager, &api_secret, &is_leader).await;
                    }
                    SyncCommand::Shutdown => {
                        log::info!("Sync worker received shutdown");
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                if !enabled.load(Ordering::Relaxed) {
                    break;
                }
                // Periodic tasks
                let now = tokio::time::Instant::now();

                // Leader: snapshot every 30s if needed
                if is_leader.load(Ordering::Relaxed)
                    && snapshot_needed.load(Ordering::Relaxed)
                    && now.duration_since(last_snapshot_check).as_millis() >= SNAPSHOT_INTERVAL_MS as u128
                {
                    snapshot_to_db(&db, &farm_path);
                    snapshot_needed.store(false, Ordering::Relaxed);
                    last_snapshot_time.store(crate::utils::current_time_ms() as u64, Ordering::Relaxed);
                    last_snapshot_check = now;

                    notify_snapshot(&client, &peer_manager, &api_secret).await;
                }

                // Follower: flush edit queue every 5s
                if !is_leader.load(Ordering::Relaxed)
                    && now.duration_since(last_flush_check).as_millis() >= EDIT_QUEUE_FLUSH_INTERVAL_MS as u128
                {
                    flush_edit_queue(&client, &edit_queue, &peer_manager, &api_secret, &is_leader).await;
                    last_flush_check = now;
                }
            }
        }
    }

    log::info!("Sync worker loop exited");
}

// ── Broadcast edit to all alive peers ──

async fn broadcast_edit(
    client: &reqwest::Client,
    peer_manager: &Arc<PeerManager>,
    api_secret: &str,
    job_path: &str,
    item_path: &str,
    metadata_json: &str,
    folder_name: &str,
    is_tracked: bool,
) {
    let peers = peer_manager.get_alive_peers();
    log::info!("Sync: Broadcasting edit for {} to {} alive peers", item_path, peers.len());
    let body = serde_json::json!({
        "job_path": job_path,
        "item_path": item_path,
        "metadata": metadata_json,
        "folder_name": folder_name,
        "is_tracked": is_tracked,
    });

    for peer in &peers {
        let url = format!(
            "http://{}:{}/api/metadata/update",
            peer.endpoint.ip, peer.endpoint.port
        );
        let mut req = client.post(&url).json(&body);
        if !api_secret.is_empty() {
            req = req.bearer_auth(api_secret);
        }
        match req.timeout(std::time::Duration::from_secs(5)).send().await {
            Ok(resp) if resp.status().is_success() => {
                log::info!("Sync: Broadcast to {} OK", peer.node_id);
            }
            Ok(resp) => {
                log::warn!("Sync: Broadcast to {} returned {}", peer.node_id, resp.status());
            }
            Err(e) => {
                log::warn!("Sync: Broadcast to {} failed: {}", peer.node_id, e);
            }
        }
    }
}

// ── Flush edit queue ──

async fn flush_edit_queue(
    client: &reqwest::Client,
    edit_queue: &Arc<tokio::sync::Mutex<Vec<EditQueueEntry>>>,
    peer_manager: &Arc<PeerManager>,
    api_secret: &str,
    is_leader: &Arc<AtomicBool>,
) {
    let mut queue = edit_queue.lock().await;
    if queue.is_empty() {
        return;
    }

    let leader_ep = if is_leader.load(Ordering::Relaxed) {
        None // Leader broadcasts directly
    } else {
        peer_manager.get_leader_endpoint()
    };

    let mut remaining = Vec::new();

    for mut entry in queue.drain(..) {
        if entry.retry_count >= MAX_EDIT_RETRY_COUNT {
            log::warn!("Dropping edit for {} after {} retries", entry.item_path, entry.retry_count);
            continue;
        }

        let success = if is_leader.load(Ordering::Relaxed) {
            // Leader: broadcast
            broadcast_edit(
                client, peer_manager, api_secret,
                &entry.job_path, &entry.item_path, &entry.metadata_json,
                &entry.folder_name, entry.is_tracked,
            ).await;
            true
        } else if let Some(ref ep) = leader_ep {
            let url = format!("http://{}:{}/api/metadata/update", ep.ip, ep.port);
            let body = serde_json::json!({
                "job_path": entry.job_path,
                "item_path": entry.item_path,
                "metadata": entry.metadata_json,
                "folder_name": entry.folder_name,
                "is_tracked": entry.is_tracked,
            });
            let mut req = client.post(&url).json(&body);
            if !api_secret.is_empty() {
                req = req.bearer_auth(api_secret);
            }
            matches!(
                req.timeout(std::time::Duration::from_secs(5)).send().await,
                Ok(resp) if resp.status().is_success()
            )
        } else {
            false
        };

        if !success {
            entry.retry_count += 1;
            remaining.push(entry);
        }
    }

    *queue = remaining;
}

// ── Snapshot: write local DB tables to shared snapshot file ──

fn snapshot_to_db(db: &Arc<Database>, farm_path: &str) {
    let snapshot_dir = Path::new(farm_path).join("snapshots");
    if let Err(e) = std::fs::create_dir_all(&snapshot_dir) {
        log::error!("Failed to create snapshot dir: {}", e);
        return;
    }

    let tmp_path = snapshot_dir.join("ufb_snapshot_v2.db.tmp");
    let final_path = snapshot_dir.join("ufb_snapshot_v2.db");

    // Remove old tmp if exists
    let _ = std::fs::remove_file(&tmp_path);

    let result = db.with_conn(|conn| {
        let snap_path_str = tmp_path.to_string_lossy().to_string();

        // Attach snapshot DB
        conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS snap;",
            snap_path_str.replace('\'', "''")
        ))?;

        // Create tables individually
        let create_tables = &[
            "CREATE TABLE IF NOT EXISTS snap.subscriptions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_path TEXT NOT NULL UNIQUE,
                 job_name TEXT NOT NULL,
                 is_active INTEGER NOT NULL DEFAULT 1,
                 subscribed_time INTEGER NOT NULL,
                 last_sync_time INTEGER,
                 sync_status TEXT NOT NULL DEFAULT 'Pending',
                 shot_count INTEGER NOT NULL DEFAULT 0
             );",
            "CREATE TABLE IF NOT EXISTS snap.column_definitions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_path TEXT NOT NULL,
                 folder_name TEXT NOT NULL,
                 column_name TEXT NOT NULL,
                 column_type TEXT NOT NULL DEFAULT 'text',
                 column_order INTEGER NOT NULL DEFAULT 0,
                 column_width REAL NOT NULL DEFAULT 120.0,
                 is_visible INTEGER NOT NULL DEFAULT 1,
                 default_value TEXT
             );",
            "CREATE TABLE IF NOT EXISTS snap.column_options (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 column_id INTEGER NOT NULL,
                 option_name TEXT NOT NULL,
                 option_color TEXT,
                 FOREIGN KEY (column_id) REFERENCES column_definitions(id) ON DELETE CASCADE
             );",
            "CREATE TABLE IF NOT EXISTS snap.item_metadata (
                 item_path TEXT NOT NULL,
                 job_path TEXT NOT NULL,
                 folder_name TEXT NOT NULL,
                 metadata_json TEXT NOT NULL DEFAULT '{}',
                 is_tracked INTEGER NOT NULL DEFAULT 0,
                 created_time INTEGER,
                 modified_time INTEGER,
                 device_id TEXT,
                 PRIMARY KEY (item_path)
             );",
            "CREATE TABLE IF NOT EXISTS snap.column_presets (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 preset_name TEXT NOT NULL UNIQUE,
                 columns_json TEXT NOT NULL,
                 created_time INTEGER NOT NULL,
                 modified_time INTEGER NOT NULL
             );",
        ];

        for sql in create_tables {
            if let Err(e) = conn.execute_batch(sql) {
                log::error!("Snapshot: failed to create table: {}", e);
                let _ = conn.execute_batch("DETACH DATABASE snap;");
                return Err(e);
            }
        }

        // Copy each table individually so one failure doesn't block the rest
        let copy_tables: &[(&str, &str)] = &[
            ("subscriptions",
             "DELETE FROM snap.subscriptions;
              INSERT OR REPLACE INTO snap.subscriptions (id, job_path, job_name, is_active, subscribed_time, last_sync_time, sync_status, shot_count)
                  SELECT id, job_path, job_name, is_active, subscribed_time, last_sync_time, sync_status, shot_count
                  FROM main.subscriptions;"),
            ("column_definitions",
             "DELETE FROM snap.column_definitions;
              INSERT OR REPLACE INTO snap.column_definitions (id, job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value)
                  SELECT id, job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value
                  FROM main.column_definitions;"),
            ("column_options",
             "DELETE FROM snap.column_options;
              INSERT OR REPLACE INTO snap.column_options (id, column_id, option_name, option_color)
                  SELECT id, column_id, option_name, option_color
                  FROM main.column_options;"),
            ("item_metadata",
             "DELETE FROM snap.item_metadata;
              INSERT OR REPLACE INTO snap.item_metadata (item_path, job_path, folder_name, metadata_json, is_tracked, created_time, modified_time, device_id)
                  SELECT item_path, job_path, folder_name, metadata_json, is_tracked, created_time, modified_time, device_id
                  FROM main.item_metadata;"),
            ("column_presets",
             "DELETE FROM snap.column_presets;
              INSERT OR REPLACE INTO snap.column_presets (id, preset_name, columns_json, created_time, modified_time)
                  SELECT id, preset_name, columns_json, created_time, modified_time
                  FROM main.column_presets;"),
        ];

        let mut any_copied = false;
        for (name, sql) in copy_tables {
            match conn.execute_batch(sql) {
                Ok(()) => {
                    any_copied = true;
                    log::debug!("Snapshot: copied {}", name);
                }
                Err(e) => {
                    log::warn!("Snapshot: skipping {} ({})", name, e);
                }
            }
        }

        conn.execute_batch("DETACH DATABASE snap;")?;

        if any_copied {
            Ok(())
        } else {
            Err(rusqlite::Error::QueryReturnedNoRows) // signal total failure
        }
    });

    match result {
        Ok(()) => {
            // Rename tmp to final
            if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
                log::error!("Failed to rename snapshot: {}", e);
            } else {
                log::info!("Snapshot written to {}", final_path.display());
            }
        }
        Err(e) => {
            log::error!("Failed to create snapshot: {}", e);
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
}

// ── Restore: read shared snapshot into local DB ──

fn restore_from_snapshot(db: &Arc<Database>, farm_path: &str) {
    let snapshot_path = Path::new(farm_path)
        .join("snapshots")
        .join("ufb_snapshot_v2.db");

    if !snapshot_path.exists() {
        log::info!("No v2 snapshot found at {}, nothing to restore", snapshot_path.display());
        return;
    }

    let snap_path_str = snapshot_path.to_string_lossy().to_string();

    let result = db.with_conn(|conn| {
        conn.execute_batch(&format!(
            "ATTACH DATABASE '{}' AS snap;",
            snap_path_str.replace('\'', "''")
        ))?;

        // Restore each table individually so a missing table doesn't block the rest
        let tables: &[(&str, &str)] = &[
            ("subscriptions",
             "-- Merge subscriptions: add new, remove deleted, preserve existing
              INSERT OR IGNORE INTO main.subscriptions (id, job_path, job_name, is_active, subscribed_time, last_sync_time, sync_status, shot_count)
                  SELECT id, job_path, job_name, is_active, subscribed_time, last_sync_time, sync_status, shot_count
                  FROM snap.subscriptions
                  WHERE job_path NOT IN (SELECT job_path FROM main.subscriptions);
              DELETE FROM main.subscriptions
              WHERE job_path NOT IN (SELECT job_path FROM snap.subscriptions);"),
            ("column_definitions",
             "DELETE FROM main.column_definitions;
              INSERT OR REPLACE INTO main.column_definitions (id, job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value)
                  SELECT id, job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value
                  FROM snap.column_definitions;"),
            ("column_options",
             "DELETE FROM main.column_options;
              INSERT OR REPLACE INTO main.column_options (id, column_id, option_name, option_color)
                  SELECT id, column_id, option_name, option_color
                  FROM snap.column_options;"),
            ("item_metadata",
             "-- Insert new rows from snapshot that don't exist locally
              INSERT OR IGNORE INTO main.item_metadata (item_path, job_path, folder_name, metadata_json, is_tracked, created_time, modified_time, device_id)
                  SELECT item_path, job_path, folder_name, metadata_json, is_tracked, created_time, modified_time, device_id
                  FROM snap.item_metadata
                  WHERE item_path NOT IN (SELECT item_path FROM main.item_metadata);
              -- Update existing rows only if snapshot has a newer modified_time
              UPDATE main.item_metadata SET
                  job_path = snap_row.job_path,
                  folder_name = snap_row.folder_name,
                  metadata_json = snap_row.metadata_json,
                  is_tracked = snap_row.is_tracked,
                  modified_time = snap_row.modified_time,
                  device_id = snap_row.device_id
              FROM (SELECT * FROM snap.item_metadata) AS snap_row
              WHERE main.item_metadata.item_path = snap_row.item_path
                AND (main.item_metadata.modified_time IS NULL
                     OR snap_row.modified_time > main.item_metadata.modified_time);
              -- Remove rows that exist locally but not in snapshot (deleted by other peers)
              DELETE FROM main.item_metadata
              WHERE item_path NOT IN (SELECT item_path FROM snap.item_metadata)
                AND job_path IN (SELECT DISTINCT job_path FROM snap.item_metadata);"),
            ("column_presets",
             "DELETE FROM main.column_presets;
              INSERT OR REPLACE INTO main.column_presets (id, preset_name, columns_json, created_time, modified_time)
                  SELECT id, preset_name, columns_json, created_time, modified_time
                  FROM snap.column_presets;"),
        ];

        for (name, sql) in tables {
            if let Err(e) = conn.execute_batch(sql) {
                log::warn!("Snapshot restore: skipping {} ({})", name, e);
            }
        }

        conn.execute_batch("DETACH DATABASE snap;")?;
        Ok(())
    });

    match result {
        Ok(()) => log::info!("Restored from snapshot {}", snapshot_path.display()),
        Err(e) => log::error!("Failed to restore from snapshot: {}", e),
    }
}

// ── Notify followers about new snapshot ──

async fn notify_snapshot(
    client: &reqwest::Client,
    peer_manager: &Arc<PeerManager>,
    api_secret: &str,
) {
    let peers = peer_manager.get_alive_peers();
    log::info!("Sync: Notifying {} peers about new snapshot", peers.len());
    for peer in &peers {
        let url = format!(
            "http://{}:{}/api/snapshot/notify",
            peer.endpoint.ip, peer.endpoint.port
        );
        let mut req = client.post(&url);
        if !api_secret.is_empty() {
            req = req.bearer_auth(api_secret);
        }
        match req.timeout(std::time::Duration::from_secs(5)).send().await {
            Ok(resp) if resp.status().is_success() => {
                log::info!("Sync: Snapshot notify to {} OK", peer.node_id);
            }
            Ok(resp) => {
                log::warn!("Sync: Snapshot notify to {} returned {}", peer.node_id, resp.status());
            }
            Err(e) => {
                log::warn!("Sync: Snapshot notify to {} failed: {}", peer.node_id, e);
            }
        }
    }
}

// ── Table change sync (subscriptions + columns) ──

/// Apply a table change to the local DB. The change_json is a JSON string with
/// an "action" field discriminating the operation.
pub fn apply_table_change(db: &Arc<Database>, change_json: &str) -> Result<(), String> {
    let change: serde_json::Value = serde_json::from_str(change_json)
        .map_err(|e| format!("Invalid table change JSON: {}", e))?;

    let action = change.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "sub_add" => {
            let job_path = change.get("job_path").and_then(|v| v.as_str()).unwrap_or("");
            let job_name = change.get("job_name").and_then(|v| v.as_str()).unwrap_or("");
            if job_path.is_empty() {
                return Err("sub_add: missing job_path".to_string());
            }
            let now = chrono::Utc::now().timestamp_millis();
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO subscriptions (job_path, job_name, subscribed_time)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![job_path, job_name, now],
                )?;
                Ok(())
            }).map_err(|e| e.to_string())
        }
        "sub_remove" => {
            let job_path = change.get("job_path").and_then(|v| v.as_str()).unwrap_or("");
            if job_path.is_empty() {
                return Err("sub_remove: missing job_path".to_string());
            }
            db.with_conn(|conn| {
                conn.execute("DELETE FROM subscriptions WHERE job_path = ?1", [job_path])?;
                conn.execute("DELETE FROM item_metadata WHERE job_path = ?1", [job_path])?;
                Ok(())
            }).map_err(|e| e.to_string())
        }
        "col_add" => {
            let def_str = change.get("def").and_then(|v| v.as_str()).unwrap_or("");
            let def: crate::columns::ColumnDefinition = serde_json::from_str(def_str)
                .map_err(|e| format!("col_add: invalid def: {}", e))?;
            db.with_conn(|conn| {
                // Skip if this column already exists (idempotent — avoids duplicates
                // when the originating follower receives its own change back via broadcast)
                let exists: bool = conn.query_row(
                    "SELECT COUNT(*) > 0 FROM column_definitions
                     WHERE job_path = ?1 AND folder_name = ?2 AND column_name = ?3",
                    rusqlite::params![def.job_path, def.folder_name, def.column_name],
                    |row| row.get(0),
                )?;
                if exists {
                    log::info!("Sync: col_add skipped (already exists): {}", def.column_name);
                    return Ok(());
                }

                conn.execute(
                    "INSERT INTO column_definitions
                     (job_path, folder_name, column_name, column_type, column_order, column_width, is_visible, default_value)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        def.job_path, def.folder_name, def.column_name, def.column_type,
                        def.column_order, def.column_width, def.is_visible as i64, def.default_value,
                    ],
                )?;
                let col_id = conn.last_insert_rowid();
                for opt in &def.options {
                    conn.execute(
                        "INSERT INTO column_options (column_id, option_name, option_color) VALUES (?1, ?2, ?3)",
                        rusqlite::params![col_id, opt.name, opt.color],
                    )?;
                }
                Ok(())
            }).map_err(|e| e.to_string())
        }
        "col_update" => {
            let def_str = change.get("def").and_then(|v| v.as_str()).unwrap_or("");
            let def: crate::columns::ColumnDefinition = serde_json::from_str(def_str)
                .map_err(|e| format!("col_update: invalid def: {}", e))?;
            let col_id = def.id.ok_or("col_update: missing column id")?;
            db.with_conn(|conn| {
                conn.execute(
                    "UPDATE column_definitions SET
                         column_name = ?1, column_type = ?2, column_order = ?3,
                         column_width = ?4, is_visible = ?5, default_value = ?6
                     WHERE id = ?7",
                    rusqlite::params![
                        def.column_name, def.column_type, def.column_order,
                        def.column_width, def.is_visible as i64, def.default_value, col_id,
                    ],
                )?;
                conn.execute("DELETE FROM column_options WHERE column_id = ?1", [col_id])?;
                for opt in &def.options {
                    conn.execute(
                        "INSERT INTO column_options (column_id, option_name, option_color) VALUES (?1, ?2, ?3)",
                        rusqlite::params![col_id, opt.name, opt.color],
                    )?;
                }
                Ok(())
            }).map_err(|e| e.to_string())
        }
        "col_delete" => {
            let id = change.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if id == 0 {
                return Err("col_delete: missing or zero id".to_string());
            }
            db.with_conn(|conn| {
                conn.execute("DELETE FROM column_options WHERE column_id = ?1", [id])?;
                conn.execute("DELETE FROM column_definitions WHERE id = ?1", [id])?;
                Ok(())
            }).map_err(|e| e.to_string())
        }
        _ => Err(format!("Unknown table change action: {}", action)),
    }
}

/// Broadcast a table change to all alive peers via HTTP.
async fn broadcast_table_change(
    client: &reqwest::Client,
    peer_manager: &Arc<PeerManager>,
    api_secret: &str,
    change_json: &str,
) {
    let peers = peer_manager.get_alive_peers();
    log::info!("Sync: Broadcasting table change to {} alive peers", peers.len());

    let body: serde_json::Value = serde_json::from_str(change_json).unwrap_or_default();

    for peer in &peers {
        let url = format!(
            "http://{}:{}/api/table/update",
            peer.endpoint.ip, peer.endpoint.port
        );
        let mut req = client.post(&url).json(&body);
        if !api_secret.is_empty() {
            req = req.bearer_auth(api_secret);
        }
        match req.timeout(std::time::Duration::from_secs(5)).send().await {
            Ok(resp) if resp.status().is_success() => {
                log::info!("Sync: Table change broadcast to {} OK", peer.node_id);
            }
            Ok(resp) => {
                log::warn!("Sync: Table change broadcast to {} returned {}", peer.node_id, resp.status());
            }
            Err(e) => {
                log::warn!("Sync: Table change broadcast to {} failed: {}", peer.node_id, e);
            }
        }
    }
}

/// Orchestrator: handle a table change command from the sync worker.
/// The local DB write was already done by the originating command handler.
/// This function only handles forwarding (follower→leader) or broadcasting (leader→peers).
async fn handle_table_change(
    client: &reqwest::Client,
    peer_manager: &Arc<PeerManager>,
    api_secret: &str,
    is_leader: &Arc<AtomicBool>,
    snapshot_needed: &Arc<AtomicBool>,
    change_json: &str,
) {
    if is_leader.load(Ordering::Relaxed) {
        // Leader: broadcast to all peers, mark snapshot needed
        broadcast_table_change(client, peer_manager, api_secret, change_json).await;
        snapshot_needed.store(true, Ordering::Relaxed);
    } else {
        // Follower: POST to leader (leader will apply + broadcast to others)
        if let Some(leader_ep) = peer_manager.get_leader_endpoint() {
            let url = format!("http://{}:{}/api/table/update", leader_ep.ip, leader_ep.port);
            log::info!("Sync: Follower posting table change to leader at {}", url);
            let body: serde_json::Value = serde_json::from_str(change_json).unwrap_or_default();
            let mut req = client.post(&url).json(&body);
            if !api_secret.is_empty() {
                req = req.bearer_auth(api_secret);
            }
            match req.timeout(std::time::Duration::from_secs(5)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    log::info!("Sync: Table change sent to leader OK");
                }
                Ok(resp) => {
                    log::warn!("Sync: Leader returned {} for table change (snapshot will catch up)", resp.status());
                }
                Err(e) => {
                    log::warn!("Sync: Failed to reach leader for table change: {} (snapshot will catch up)", e);
                }
            }
        } else {
            log::warn!("Sync: No leader known for table change (snapshot will catch up)");
        }
    }
}

