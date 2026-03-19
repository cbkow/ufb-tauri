use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerEndpoint {
    pub node_id: String,
    pub ip: String,
    pub port: u16,
    pub timestamp_ms: i64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub node_id: String,
    pub tags: Vec<String>,
    pub endpoint: PeerEndpoint,
    pub is_alive: bool,
    pub is_leader: bool,
    pub failed_polls: i32,
    pub last_seen_ms: i64,
    pub has_udp_contact: bool,
    pub last_udp_contact_ms: i64,
}

/// HTTP status response from a peer's GET /api/status
#[derive(Debug, Deserialize)]
pub struct PeerStatusResponse {
    pub node_id: String,
    #[serde(default)]
    pub is_leader: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub enabled: bool,
}

pub struct PeerManager {
    farm_path: String,
    node_id: String,
    port: u16,
    tags: Vec<String>,
    peers: Mutex<HashMap<String, PeerInfo>>,
    is_leader: Mutex<bool>,
    leader_id: Mutex<Option<String>>,
    leader_endpoint: Mutex<Option<PeerEndpoint>>,
    /// Callback invoked when leadership changes. Parameter: am_leader.
    on_leadership_changed: Mutex<Option<Box<dyn Fn(bool) + Send>>>,
}

impl PeerManager {
    pub fn new(farm_path: String, node_id: String, port: u16, tags: Vec<String>) -> Self {
        Self {
            farm_path,
            node_id,
            port,
            tags,
            peers: Mutex::new(HashMap::new()),
            is_leader: Mutex::new(false),
            leader_id: Mutex::new(None),
            leader_endpoint: Mutex::new(None),
            on_leadership_changed: Mutex::new(None),
        }
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn is_leader(&self) -> bool {
        *self.is_leader.lock().unwrap()
    }

    pub fn get_current_leader_id(&self) -> Option<String> {
        self.leader_id.lock().unwrap().clone()
    }

    /// Get the leader's endpoint (for followers to POST edits to).
    pub fn get_leader_endpoint(&self) -> Option<PeerEndpoint> {
        self.leader_endpoint.lock().unwrap().clone()
    }

    pub fn get_peers(&self) -> Vec<PeerInfo> {
        self.peers.lock().unwrap().values().cloned().collect()
    }

    pub fn get_alive_peers(&self) -> Vec<PeerInfo> {
        self.peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.is_alive)
            .cloned()
            .collect()
    }

    pub fn get_peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    pub fn set_on_leadership_changed<F: Fn(bool) + Send + 'static>(&self, f: F) {
        *self.on_leadership_changed.lock().unwrap() = Some(Box::new(f));
    }

    /// Get the phonebook directory: {farm_path}/nodes/
    fn phonebook_dir(&self) -> PathBuf {
        Path::new(&self.farm_path).join("nodes")
    }

    /// Write this node's endpoint to the phonebook (includes tags).
    pub fn register_endpoint(&self) -> Result<(), String> {
        let dir = self.phonebook_dir().join(&self.node_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create node dir: {}", e))?;

        let endpoint = PeerEndpoint {
            node_id: self.node_id.clone(),
            ip: get_local_ip(),
            port: self.port,
            timestamp_ms: crate::utils::current_time_ms(),
            tags: self.tags.clone(),
        };

        let json = serde_json::to_string_pretty(&endpoint)
            .map_err(|e| format!("Failed to serialize endpoint: {}", e))?;
        std::fs::write(dir.join("endpoint.json"), json)
            .map_err(|e| format!("Failed to write endpoint: {}", e))
    }

    /// Remove this node's endpoint from the phonebook.
    pub fn unregister_endpoint(&self) {
        let path = self.phonebook_dir().join(&self.node_id).join("endpoint.json");
        let _ = std::fs::remove_file(&path);
    }

    /// Discover peers from the phonebook directory.
    ///
    /// New peers are only marked alive if their endpoint.json timestamp is
    /// fresh (within the last 15 seconds).  Stale entries are inserted as
    /// not-alive so they won't win an election until confirmed by a
    /// successful HTTP poll or UDP heartbeat.
    pub fn discover_peers(&self) -> Result<Vec<PeerEndpoint>, String> {
        let nodes_dir = self.phonebook_dir();
        if !nodes_dir.exists() {
            return Ok(vec![]);
        }

        let mut endpoints = Vec::new();
        let entries = std::fs::read_dir(&nodes_dir)
            .map_err(|e| format!("Failed to read nodes dir: {}", e))?;

        let now = crate::utils::current_time_ms();
        const FRESH_THRESHOLD_MS: i64 = 15_000;

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let endpoint_file = entry.path().join("endpoint.json");
            if let Ok(content) = std::fs::read_to_string(&endpoint_file) {
                if let Ok(ep) = serde_json::from_str::<PeerEndpoint>(&content) {
                    if ep.node_id == self.node_id {
                        continue;
                    }
                    let is_fresh = (now - ep.timestamp_ms) < FRESH_THRESHOLD_MS;
                    // Update or insert peer from phonebook
                    let mut peers = self.peers.lock().unwrap();
                    let peer = peers.entry(ep.node_id.clone()).or_insert_with(|| {
                        if !is_fresh {
                            log::info!(
                                "Discovered peer {} with stale endpoint (age={}s), marking not-alive until verified",
                                ep.node_id,
                                (now - ep.timestamp_ms) / 1000
                            );
                        }
                        PeerInfo {
                            node_id: ep.node_id.clone(),
                            tags: ep.tags.clone(),
                            endpoint: ep.clone(),
                            is_alive: is_fresh,
                            is_leader: false,
                            failed_polls: 0,
                            last_seen_ms: if is_fresh { now } else { 0 },
                            has_udp_contact: false,
                            last_udp_contact_ms: 0,
                        }
                    });
                    peer.endpoint = ep.clone();
                    peer.tags = ep.tags.clone();
                    endpoints.push(ep);
                }
            }
        }

        Ok(endpoints)
    }

    /// Run leader election — 3-tier sort matching C++:
    /// 1. has "leader" tag → first (desc)
    /// 2. has "noleader" tag → last (asc)
    /// 3. alphabetical node_id
    pub fn run_election(&self) {
        let was_leader = *self.is_leader.lock().unwrap();

        // Collect candidates: alive peers + self
        struct Candidate {
            node_id: String,
            has_leader_tag: bool,
            has_noleader_tag: bool,
            endpoint: Option<PeerEndpoint>,
        }

        let mut candidates = Vec::new();

        {
            let peers = self.peers.lock().unwrap();
            for p in peers.values() {
                if !p.is_alive {
                    continue;
                }
                candidates.push(Candidate {
                    node_id: p.node_id.clone(),
                    has_leader_tag: p.tags.iter().any(|t| t == "leader"),
                    has_noleader_tag: p.tags.iter().any(|t| t == "noleader"),
                    endpoint: Some(p.endpoint.clone()),
                });
            }
        }

        // Add self
        candidates.push(Candidate {
            node_id: self.node_id.clone(),
            has_leader_tag: self.tags.iter().any(|t| t == "leader"),
            has_noleader_tag: self.tags.iter().any(|t| t == "noleader"),
            endpoint: None,
        });

        // Sort: leader tag desc, noleader tag asc, then alphabetical
        candidates.sort_by(|a, b| {
            // leader tag first (true > false, so reverse)
            b.has_leader_tag
                .cmp(&a.has_leader_tag)
                .then_with(|| {
                    // noleader tag last (true < false for ranking)
                    a.has_noleader_tag.cmp(&b.has_noleader_tag)
                })
                .then_with(|| a.node_id.cmp(&b.node_id))
        });

        let leader = candidates.first().map(|c| c.node_id.clone());
        let am_leader = leader.as_deref() == Some(&self.node_id);

        // Update leader endpoint for followers
        if !am_leader {
            let ep = candidates.first().and_then(|c| c.endpoint.clone());
            *self.leader_endpoint.lock().unwrap() = ep;
        } else {
            *self.leader_endpoint.lock().unwrap() = None;
        }

        // Update peer is_leader flags
        {
            let mut peers = self.peers.lock().unwrap();
            for p in peers.values_mut() {
                p.is_leader = leader.as_deref() == Some(&p.node_id);
            }
        }

        *self.is_leader.lock().unwrap() = am_leader;
        *self.leader_id.lock().unwrap() = leader;

        // Fire callback if leadership changed
        if was_leader != am_leader {
            log::info!(
                "Leadership changed: {} is now {}",
                self.node_id,
                if am_leader { "LEADER" } else { "FOLLOWER" }
            );
            if let Some(ref cb) = *self.on_leadership_changed.lock().unwrap() {
                cb(am_leader);
            }
        }
    }

    /// Poll peers via HTTP GET /api/status.
    ///
    /// - Alive peers: polled every cycle.
    /// - Not-alive peers: polled once per cycle to verify whether they've
    ///   come back (needed so stale-phonebook peers can become alive).
    /// UDP heartbeats can still resurrect peers via `process_heartbeat`,
    /// but HTTP is the sole authority for marking peers dead.
    pub async fn poll_peers(&self, client: &reqwest::Client) {
        let now = crate::utils::current_time_ms();

        // Snapshot peers to avoid holding lock during HTTP calls
        let peer_list: Vec<(String, PeerEndpoint, bool, bool, i64)> = {
            let peers = self.peers.lock().unwrap();
            peers
                .values()
                .map(|p| {
                    (
                        p.node_id.clone(),
                        p.endpoint.clone(),
                        p.is_alive,
                        p.has_udp_contact,
                        p.last_seen_ms,
                    )
                })
                .collect()
        };

        for (node_id, endpoint, is_alive, _has_udp, _last_seen) in peer_list {
            let url = format!("http://{}:{}/api/status", endpoint.ip, endpoint.port);
            match client.get(&url).timeout(std::time::Duration::from_secs(3)).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(status) = resp.json::<PeerStatusResponse>().await {
                        let mut peers = self.peers.lock().unwrap();
                        if let Some(p) = peers.get_mut(&node_id) {
                            if !p.is_alive {
                                log::info!("Peer {} verified alive via HTTP poll", node_id);
                            }
                            p.is_alive = true;
                            p.failed_polls = 0;
                            p.last_seen_ms = now;
                            p.tags = status.tags;
                        }
                    }
                }
                _ => {
                    let mut peers = self.peers.lock().unwrap();
                    if let Some(p) = peers.get_mut(&node_id) {
                        if p.is_alive {
                            p.failed_polls += 1;
                            // Dead if failed_polls >= 3 (HTTP is authoritative for liveness)
                            if p.failed_polls >= crate::mesh_sync::PEER_DEAD_POLL_COUNT {
                                p.is_alive = false;
                                log::info!("Peer {} marked dead (failed_polls={})", node_id, p.failed_polls);
                            }
                        }
                        // Not-alive peers: just leave them not-alive, don't increment
                        // failed_polls (they were never verified to begin with)
                    }
                }
            }
        }

        // UDP silence detection: clear has_udp_contact if silent > 15s
        {
            let mut peers = self.peers.lock().unwrap();
            for p in peers.values_mut() {
                if p.has_udp_contact && (now - p.last_udp_contact_ms) > crate::mesh_sync::UDP_SILENCE_THRESHOLD_MS as i64 {
                    p.has_udp_contact = false;
                }
            }
        }
    }

    /// Process a UDP heartbeat from a peer (with tags).
    pub fn process_heartbeat(&self, node_id: &str, ip: &str, port: u16, tags: &[String]) {
        let now = crate::utils::current_time_ms();
        let mut peers = self.peers.lock().unwrap();
        let peer = peers.entry(node_id.to_string()).or_insert_with(|| PeerInfo {
            node_id: node_id.to_string(),
            tags: tags.to_vec(),
            endpoint: PeerEndpoint {
                node_id: node_id.to_string(),
                ip: ip.to_string(),
                port,
                timestamp_ms: now,
                tags: tags.to_vec(),
            },
            is_alive: true,
            is_leader: false,
            failed_polls: 0,
            last_seen_ms: now,
            has_udp_contact: true,
            last_udp_contact_ms: now,
        });
        peer.last_seen_ms = now;
        peer.has_udp_contact = true;
        peer.last_udp_contact_ms = now;
        peer.is_alive = true;
        peer.failed_polls = 0;
        peer.endpoint.ip = ip.to_string();
        peer.endpoint.port = port;
        peer.tags = tags.to_vec();
        peer.endpoint.tags = tags.to_vec();
    }

    /// Mark a peer as dead (not removed — matching C++) and trigger election.
    pub fn process_goodbye(&self, node_id: &str) {
        {
            let mut peers = self.peers.lock().unwrap();
            if let Some(p) = peers.get_mut(node_id) {
                p.is_alive = false;
                p.has_udp_contact = false;
            }
        }
        self.run_election();
    }

    /// Clean up stale peers.
    ///
    /// - Dead peers whose endpoint.json is gone → remove from map.
    /// - Dead peers whose endpoint.json is very old (> 1 hour) → delete
    ///   the file and remove from map. This handles nodes that crashed
    ///   without calling unregister_endpoint().
    pub fn cleanup_stale_peers(&self) {
        let now = crate::utils::current_time_ms();
        const STALE_ENDPOINT_MS: i64 = 3600 * 1000; // 1 hour

        let mut peers = self.peers.lock().unwrap();
        let phonebook = self.phonebook_dir();
        peers.retain(|node_id, p| {
            if !p.is_alive {
                let ep_path = phonebook.join(node_id).join("endpoint.json");
                if !ep_path.exists() {
                    log::info!("Removing stale peer {} (no endpoint.json)", node_id);
                    return false;
                }
                // Delete very old endpoint files from dead peers
                if (now - p.endpoint.timestamp_ms) > STALE_ENDPOINT_MS {
                    log::info!(
                        "Removing dead peer {} and deleting stale endpoint.json (age={}m)",
                        node_id,
                        (now - p.endpoint.timestamp_ms) / 60_000
                    );
                    let _ = std::fs::remove_file(&ep_path);
                    // Also try to remove the node directory if empty
                    let _ = std::fs::remove_dir(phonebook.join(node_id));
                    return false;
                }
            }
            true
        });
    }
}

/// Get local IP address using the UDP socket trick:
/// Connect a UDP socket to 8.8.8.8:80 and read the local address.
pub fn get_local_ip() -> String {
    match std::net::UdpSocket::bind("0.0.0.0:0") {
        Ok(sock) => {
            if sock.connect("8.8.8.8:80").is_ok() {
                if let Ok(addr) = sock.local_addr() {
                    return addr.ip().to_string();
                }
            }
            "127.0.0.1".to_string()
        }
        Err(_) => "127.0.0.1".to_string(),
    }
}
