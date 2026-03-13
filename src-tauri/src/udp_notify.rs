use socket2::{Domain, Protocol, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

/// UDP multicast for peer heartbeat and notifications.
/// Wire-compatible with C++ UdpNotify: port 4244, group 239.42.0.2, TTL 1.
///
/// Message formats (JSON):
///   Heartbeat: {"t":"hb","from":"nodeId","n":"nodeId","ip":"x.x.x.x","port":49200,"tags":["leader"]}
///   Goodbye:   {"t":"bye","from":"nodeId","n":"nodeId"}
pub struct UdpNotify {
    node_id: String,
    multicast_port: u16,
    multicast_group: Ipv4Addr,
    socket: Mutex<Option<Arc<UdpSocket>>>,
}

impl UdpNotify {
    pub fn new(node_id: String, multicast_port: u16, multicast_group: String) -> Self {
        let group = multicast_group
            .parse::<Ipv4Addr>()
            .unwrap_or(Ipv4Addr::new(239, 42, 0, 2));
        Self {
            node_id,
            multicast_port,
            multicast_group: group,
            socket: Mutex::new(None),
        }
    }

    /// Create and bind the persistent multicast socket.
    pub async fn start(&self) -> Result<(), String> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| format!("Failed to create socket: {}", e))?;

        sock.set_reuse_address(true)
            .map_err(|e| format!("Failed to set SO_REUSEADDR: {}", e))?;

        // On Windows, also set SO_REUSEADDR for port sharing
        #[cfg(windows)]
        {
            // socket2 handles this via set_reuse_address on Windows
        }

        sock.set_nonblocking(true)
            .map_err(|e| format!("Failed to set non-blocking: {}", e))?;

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, self.multicast_port);
        sock.bind(&bind_addr.into())
            .map_err(|e| format!("Failed to bind UDP {}:{}: {}", Ipv4Addr::UNSPECIFIED, self.multicast_port, e))?;

        sock.join_multicast_v4(&self.multicast_group, &Ipv4Addr::UNSPECIFIED)
            .map_err(|e| format!("Failed to join multicast group: {}", e))?;

        sock.set_multicast_ttl_v4(1)
            .map_err(|e| format!("Failed to set multicast TTL: {}", e))?;

        // Convert socket2::Socket → std::net::UdpSocket → tokio::net::UdpSocket
        let std_socket: std::net::UdpSocket = sock.into();
        let tokio_socket = UdpSocket::from_std(std_socket)
            .map_err(|e| format!("Failed to convert to tokio socket: {}", e))?;

        *self.socket.lock().await = Some(Arc::new(tokio_socket));
        log::info!("UDP multicast started on port {} (group {})", self.multicast_port, self.multicast_group);
        Ok(())
    }

    /// Stop the socket and send a goodbye message.
    pub async fn stop(&self, tags: &[String]) {
        // Try to send goodbye before dropping
        if let Err(e) = self.send_goodbye(tags).await {
            log::warn!("Failed to send UDP goodbye: {}", e);
        }
        *self.socket.lock().await = None;
        log::info!("UDP multicast stopped");
    }

    /// Get a clone of the socket Arc for the listener task.
    pub async fn get_socket(&self) -> Option<Arc<UdpSocket>> {
        self.socket.lock().await.clone()
    }

    pub fn is_running(&self) -> bool {
        // Synchronous check — try_lock to avoid blocking
        self.socket
            .try_lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// Send a wire-compatible heartbeat.
    pub async fn send_heartbeat(&self, ip: &str, http_port: u16, tags: &[String]) -> Result<(), String> {
        let msg = serde_json::json!({
            "t": "hb",
            "from": self.node_id,
            "n": self.node_id,
            "ip": ip,
            "port": http_port,
            "tags": tags,
        });
        self.send_multicast(&msg).await
    }

    /// Send a wire-compatible goodbye.
    pub async fn send_goodbye(&self, _tags: &[String]) -> Result<(), String> {
        let msg = serde_json::json!({
            "t": "bye",
            "from": self.node_id,
            "n": self.node_id,
        });
        self.send_multicast(&msg).await
    }

    /// Send a JSON message to the multicast group.
    async fn send_multicast(&self, message: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(message)
            .map_err(|e| format!("Failed to serialize: {}", e))?;

        if json.len() > 1400 {
            return Err("Message too large for UDP multicast".to_string());
        }

        let guard = self.socket.lock().await;
        let socket = guard.as_ref().ok_or("UDP socket not started")?;

        let dest = SocketAddr::V4(SocketAddrV4::new(self.multicast_group, self.multicast_port));
        socket
            .send_to(json.as_bytes(), dest)
            .await
            .map_err(|e| format!("Failed to send UDP: {}", e))?;

        Ok(())
    }

    /// Non-blocking drain: read all available packets and return parsed JSON values.
    /// Filters out self-sent packets (where msg["from"] == self.node_id).
    pub async fn poll(&self) -> Vec<serde_json::Value> {
        let guard = self.socket.lock().await;
        let socket = match guard.as_ref() {
            Some(s) => s,
            None => return vec![],
        };

        let mut messages = Vec::new();
        let mut buf = [0u8; 2048];

        // Drain all available packets
        loop {
            match socket.try_recv_from(&mut buf) {
                Ok((len, _addr)) => {
                    if let Ok(text) = std::str::from_utf8(&buf[..len]) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                            // Filter self-sent
                            if val.get("from").and_then(|v| v.as_str()) != Some(&self.node_id) {
                                messages.push(val);
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        messages
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn multicast_port(&self) -> u16 {
        self.multicast_port
    }
}
