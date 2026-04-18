use crate::config::{self, MountConfig, MountsConfig};
use crate::messages::{AckMsg, AgentToUfb, CacheStatsMsg, ErrorMsg, FreshnessSweepMsg, MountIdMsg, UfbToAgent};
use crate::orchestrator::Orchestrator;
use crate::state::MountEvent;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Top-level mount manager. Manages N mount instances.
pub struct MountService {
    mounts: HashMap<String, MountInstance>,
    ipc_tx: mpsc::Sender<AgentToUfb>,
    /// Servers with active SMB sessions — shared across all orchestrators.
    connected_servers: Arc<Mutex<HashSet<String>>>,
    /// Global cache root for sync mounts.
    cache_root: std::path::PathBuf,
    /// Per-domain caches shared with the VFS server (NFS on macOS, ProjFS
    /// on Windows) so UI-triggered drain + stats commands hit the same instance.
    #[cfg(any(target_os = "macos", windows))]
    shared_caches: Option<crate::sync::SharedCaches>,
    /// Registry of live sync servers — drained and torn down on cache-root
    /// change so the next spawn can re-open caches at the new location.
    #[cfg(any(target_os = "macos", windows))]
    sync_servers: Option<crate::sync::SyncServersRegistry>,
    /// Channel to tell main.rs "rebuild the sync servers now."
    /// `None` = rebuild all (cache-root change); `Some(domain)` = rebuild
    /// just that one domain (tray Start/Restart on a single sync mount).
    /// Fired after tearing down the old server(s).
    #[cfg(any(target_os = "macos", windows))]
    sync_respawn_tx: Option<mpsc::Sender<Option<String>>>,
}

struct MountInstance {
    config: MountConfig,
    event_tx: mpsc::Sender<MountEvent>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl MountService {
    pub fn new(ipc_tx: mpsc::Sender<AgentToUfb>) -> Self {
        Self {
            mounts: HashMap::new(),
            ipc_tx,
            connected_servers: Arc::new(Mutex::new(HashSet::new())),
            cache_root: crate::config::MountConfig::default_cache_root(),
            #[cfg(any(target_os = "macos", windows))]
            shared_caches: None,
            #[cfg(any(target_os = "macos", windows))]
            sync_servers: None,
            #[cfg(any(target_os = "macos", windows))]
            sync_respawn_tx: None,
        }
    }

    /// Inject the per-domain VFS cache map. Call after `new` but before
    /// `start_from_config` so UI drain/stats commands have access.
    #[cfg(any(target_os = "macos", windows))]
    pub fn set_shared_caches(&mut self, caches: crate::sync::SharedCaches) {
        self.shared_caches = Some(caches);
    }

    /// Inject the sync-server registry + respawn channel. Required for
    /// live cache-root switching and per-mount tray lifecycle: on detected
    /// change, MountService tears down server(s) in the registry then
    /// fires the respawn signal so `main.rs` can rebuild. Payload is
    /// `None` (all mounts) or `Some(domain)` (single mount).
    #[cfg(any(target_os = "macos", windows))]
    pub fn set_sync_servers(
        &mut self,
        registry: crate::sync::SyncServersRegistry,
        respawn_tx: mpsc::Sender<Option<String>>,
    ) {
        self.sync_servers = Some(registry);
        self.sync_respawn_tx = Some(respawn_tx);
    }

    /// Load config and start all enabled mounts.
    pub async fn start_from_config(&mut self) {
        let config = config::load_config();
        self.apply_config(config).await;
    }

    /// Reload config from disk and apply changes.
    pub async fn reload_config(&mut self) {
        log::info!("Reloading config...");
        let config = config::load_config();
        self.apply_config(config).await;
    }

    /// Apply a config, starting new mounts and stopping removed ones.
    async fn apply_config(&mut self, config: MountsConfig) {
        // Clean up stale Cloud Files sync root registrations and redirect active
        // CF nav entries to point at the junction path instead of the cache path
        #[cfg(windows)]
        {
            let active_sync_mounts: std::collections::HashMap<String, String> = config
                .mounts
                .iter()
                .filter(|m| m.enabled && m.sync_enabled)
                .map(|m| (m.id.clone(), m.volume_path()))
                .collect();
            // ProjFS: no CF sync root registrations to clean up.
            // crate::sync::SyncRoot::cleanup_stale_roots(&active_sync_mounts);
            let _ = active_sync_mounts;
        }

        // macOS: clean up stale entries in ~/ufb/mounts/ from removed mounts.
        // Entries may be symlinks (plain-SMB mounts point at /Volumes/<share>)
        // or real directories (NFS mount points for sync mounts). Both live in
        // the same namespace now so toggling sync on a mount doesn't move the
        // user-facing path — orphan classification has to handle both shapes.
        #[cfg(target_os = "macos")]
        {
            let active_names: std::collections::HashSet<String> = config
                .mounts
                .iter()
                .filter(|m| m.enabled)
                .map(|m| m.share_name())
                .collect();
            let base = config::MountConfig::volumes_base();
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = match path.file_name().and_then(|n| n.to_str()) {
                        Some(n) => n.to_string(),
                        None => continue,
                    };
                    if active_names.contains(&name) {
                        continue;
                    }
                    let md = match std::fs::symlink_metadata(&path) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if md.file_type().is_symlink() {
                        let _ = std::fs::remove_file(&path);
                        log::info!("Removed stale symlink: {}", path.display());
                    } else if md.is_dir() {
                        // Directory entry: could be a live NFS mount point left
                        // over from a now-disabled mount, or just an empty dir.
                        if crate::sync::nfs_server::is_mounted(&path) {
                            log::info!("Umounting stale NFS mount: {}", path.display());
                            let _ = std::process::Command::new("umount")
                                .arg(&path)
                                .status();
                            // Try forced umount if the gentle one left it mounted
                            if crate::sync::nfs_server::is_mounted(&path) {
                                let _ = std::process::Command::new("umount")
                                    .arg("-f")
                                    .arg(&path)
                                    .status();
                            }
                        }
                        match std::fs::remove_dir(&path) {
                            Ok(()) => log::info!("Removed stale dir: {}", path.display()),
                            Err(e) => log::warn!(
                                "Could not remove stale dir {} (may still be mounted or non-empty): {}",
                                path.display(),
                                e
                            ),
                        }
                    }
                }
            }

            // Warn about share name collisions
            for (name, ids) in config.share_name_collisions() {
                log::warn!(
                    "Share name collision: '{}' used by mounts [{}]. Set mount_path_macos to resolve.",
                    name,
                    ids.join(", ")
                );
            }
        }

        // Detect cache root change — restart all sync mounts to re-register at new path
        let new_cache_root = config.cache_root();
        if new_cache_root != self.cache_root {
            log::info!(
                "Sync cache root changed: {:?} → {:?}. Restarting all sync mounts.",
                self.cache_root, new_cache_root
            );
            // Stop all sync mounts (they'll be re-started below with the new cache root)
            let sync_ids: Vec<String> = self.mounts.iter()
                .filter(|(_, inst)| inst.config.sync_enabled)
                .map(|(id, _)| id.clone())
                .collect();
            for id in sync_ids {
                if let Some(instance) = self.mounts.remove(&id) {
                    log::info!("Stopping sync mount {} for cache migration", id);
                    let _ = instance.event_tx.send(MountEvent::Stop).await;
                }
            }

            // Tear down every VFS server (NFS on macOS, WinFsp on Windows)
            // so cache files stop being held open. Each `shutdown_and_wait`
            // drains in-flight callbacks, unmounts the OS mount point, and
            // drops its `Arc<Cache>` clone.
            //
            // Only fire the respawn signal if we actually had servers to
            // tear down. On initial startup, MountService's `cache_root`
            // field is seeded from the default while the loaded config
            // may specify a custom root — which looks like a "change" to
            // apply_config but the first-spawn path in main.rs handles
            // it. Firing respawn here would cause a duplicate spawn.
            #[cfg(any(target_os = "macos", windows))]
            {
                let mut torn_down_any = false;
                if let Some(registry) = self.sync_servers.as_ref() {
                    let drained: Vec<_> = {
                        let mut guard = registry.lock().await;
                        guard.drain().collect()
                    };
                    for (domain, handle) in drained {
                        log::info!("[sync] tearing down server for {}", domain);
                        handle.shutdown_and_wait().await;
                        torn_down_any = true;
                    }
                }
                if torn_down_any {
                    // Drop all shared cache entries. Each Arc<Cache>
                    // refcount goes to zero once servers have shut down,
                    // closing the SQLite pool and letting the next open()
                    // bind the new path.
                    if let Some(caches) = self.shared_caches.as_ref() {
                        if let Ok(mut guard) = caches.write() {
                            guard.clear();
                        }
                    }
                    // Fire the respawn signal. `main.rs` listens and
                    // rebuilds servers with the new cache_root().
                    // `None` = full rebuild across every sync-enabled mount.
                    if let Some(tx) = self.sync_respawn_tx.as_ref() {
                        let _ = tx.try_send(None);
                    }
                }
            }
        }
        self.cache_root = new_cache_root;

        let new_ids: std::collections::HashSet<String> =
            config.mounts.iter().map(|m| m.id.clone()).collect();

        // Stop mounts that are no longer in config
        let to_remove: Vec<String> = self
            .mounts
            .keys()
            .filter(|id| !new_ids.contains(*id))
            .cloned()
            .collect();

        for id in to_remove {
            log::info!("Removing mount: {}", id);
            if let Some(instance) = self.mounts.remove(&id) {
                let _ = instance.event_tx.send(MountEvent::Stop).await;
            }
        }

        // Start/update mounts from config
        for mount_config in config.mounts {
            if !mount_config.enabled {
                // If mount exists but is now disabled, stop it
                if let Some(instance) = self.mounts.remove(&mount_config.id) {
                    log::info!("Disabling mount: {}", mount_config.id);
                    let _ = instance.event_tx.send(MountEvent::Stop).await;
                }
                continue;
            }

            if let Some(instance) = self.mounts.get(&mount_config.id) {
                // Mount exists — check if config changed
                if instance.config != mount_config {
                    log::info!("Config changed for mount: {}", mount_config.id);
                    let _ = instance
                        .event_tx
                        .send(MountEvent::ConfigChanged {
                            new_config: mount_config.clone(),
                        })
                        .await;
                    // Update stored config
                    if let Some(instance) = self.mounts.get_mut(&mount_config.id) {
                        instance.config = mount_config;
                    }
                }
                continue;
            }

            self.start_mount(mount_config).await;
        }
    }

    async fn start_mount(&mut self, config: MountConfig) {
        let mount_id = config.id.clone();

        log::info!("Starting mount: {}", mount_id);

        let mut orchestrator = Orchestrator::new(
            config.clone(),
            self.cache_root.clone(),
            self.ipc_tx.clone(),
            self.connected_servers.clone(),
        );
        let event_tx = orchestrator.event_sender();

        // Run orchestrator in background task
        let id_clone = mount_id.clone();
        let task_handle = tokio::spawn(async move {
            orchestrator.run().await;
            log::info!("[{}] Orchestrator exited", id_clone);
        });

        // Store the mount instance
        self.mounts.insert(
            mount_id,
            MountInstance {
                config,
                event_tx: event_tx.clone(),
                task_handle,
            },
        );
    }

    /// Handle a command from UFB.
    pub async fn handle_command(&mut self, cmd: UfbToAgent) {
        match cmd {
            UfbToAgent::Ping => {
                let _ = self.ipc_tx.send(AgentToUfb::Pong).await;
            }
            UfbToAgent::ReloadConfig => {
                self.reload_config().await;
            }
            UfbToAgent::GetStates => {
                // Trigger state update emission from all mounts
                for (_, instance) in &self.mounts {
                    let _ = instance.event_tx.send(MountEvent::RequestStateUpdate).await;
                }
            }
            UfbToAgent::StartMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Start, &msg.command_id)
                    .await;
            }
            UfbToAgent::StopMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Stop, &msg.command_id)
                    .await;
            }
            UfbToAgent::RestartMount(msg) => {
                self.send_to_mount(&msg.mount_id, MountEvent::Restart, &msg.command_id)
                    .await;
            }
            UfbToAgent::ClearSyncCache(msg) => {
                if self.try_drain_cache(&msg).await {
                    return;
                }
                // Fall-through: orchestrator's ClearSyncCache handler does
                // per-platform teardown for legacy paths.
                self.send_to_mount(&msg.mount_id, MountEvent::ClearSyncCache, &msg.command_id)
                    .await;
            }
            UfbToAgent::GetCacheStats(msg) => {
                self.handle_get_cache_stats(msg).await;
            }
            UfbToAgent::CreateSymlinks => {
                #[cfg(windows)]
                {
                    log::info!("CreateSymlinks command received — launching elevated instance");
                    if let Err(e) = crate::platform::windows::elevation::launch_elevated_symlink_creation() {
                        log::error!("Elevation launch failed: {}", e);
                    }
                }
                #[cfg(not(windows))]
                {
                    log::debug!("CreateSymlinks ignored on this platform");
                }
            }
            UfbToAgent::Quit => {
                log::info!("Quit command received via IPC, shutting down...");
                self.shutdown().await;
                std::process::exit(0);
            }
            UfbToAgent::FreshnessSweep(msg) => {
                self.handle_freshness_sweep(msg).await;
            }
        }
    }

    /// Trigger a cross-platform freshness sweep — user-driven hint that "now
    /// would be a good time to invalidate stale caches." Runs platform-native
    /// signaling rather than crawling.
    ///
    /// macOS: post the same Darwin notification the watcher uses; the
    /// extension responds with `signalEnumerator(.workingSet)` → `getChanges`
    /// drains any drift the agent has already detected.
    ///
    /// Windows: log only for now. The CF filter's `opened` and
    /// `fetch_placeholders` hooks do per-access freshness; a broader sweep
    /// would walk known-hydrated paths and stat them, which we'll add when
    /// it earns its keep against real workloads.
    async fn handle_freshness_sweep(&self, msg: FreshnessSweepMsg) {
        let domains: Vec<String> = if let Some(d) = msg.domain.clone() {
            vec![d]
        } else {
            // All currently-enabled mount share names.
            self.mounts
                .values()
                .map(|m| m.config.share_name())
                .collect()
        };

        log::info!(
            "[freshness-sweep] domains={:?} (from cmd {})",
            domains,
            msg.command_id
        );

        #[cfg(target_os = "macos")]
        {
            for d in &domains {
                crate::sync::macos_watcher::post_darwin_notification(d);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Windows / Linux: no per-platform sweep wired yet. The opportunistic
            // hooks (CF `opened`, `fetch_placeholders`) cover most cases.
            let _ = &domains;
        }

        if !msg.command_id.is_empty() {
            let _ = self
                .ipc_tx
                .send(AgentToUfb::Ack(AckMsg {
                    command_id: msg.command_id,
                }))
                .await;
        }
    }

    /// Try to drain the VFS cache for a mount inline. Returns `true` when
    /// handled (NFS on macOS, ProjFS on Windows); `false` means the caller
    /// should fall back to the orchestrator path.
    #[cfg(any(target_os = "macos", windows))]
    async fn try_drain_cache(&self, msg: &MountIdMsg) -> bool {
        let Some(ref caches) = self.shared_caches else {
            return false;
        };
        let share_name = self
            .mounts
            .get(&msg.mount_id)
            .map(|instance| instance.config.share_name())
            .unwrap_or_else(|| msg.mount_id.clone());
        let cache = {
            let guard = caches.read().unwrap();
            guard.get(&share_name).cloned()
        };
        let Some(cache) = cache else {
            return false;
        };

        #[cfg(target_os = "macos")]
        let (count, bytes) = cache.drain_all().await;
        #[cfg(windows)]
        let (count, bytes) = cache.drain_all();

        log::info!(
            "[{}] Drain cache: {} files, {:.1} MB",
            msg.mount_id,
            count,
            bytes as f64 / 1_048_576.0,
        );
        if !msg.command_id.is_empty() {
            let _ = self
                .ipc_tx
                .send(AgentToUfb::Ack(AckMsg {
                    command_id: msg.command_id.clone(),
                }))
                .await;
        }
        let (hb, hc) = cache.cache_stats();
        let _ = self
            .ipc_tx
            .send(AgentToUfb::CacheStats(CacheStatsMsg {
                mount_id: msg.mount_id.clone(),
                hydrated_bytes: hb,
                hydrated_count: hc,
                command_id: String::new(),
            }))
            .await;
        true
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    async fn try_drain_cache(&self, _msg: &MountIdMsg) -> bool {
        false
    }

    /// Respond to `UfbToAgent::GetCacheStats` with current cache size.
    /// Reads from the shared VFS cache on both macOS and Windows.
    async fn handle_get_cache_stats(&self, msg: MountIdMsg) {
        let (hydrated_bytes, hydrated_count) = {
            #[cfg(any(target_os = "macos", windows))]
            {
                let zeros = (0u64, 0u64);
                let share_name = self
                    .mounts
                    .get(&msg.mount_id)
                    .map(|m| m.config.share_name());
                match (share_name, self.shared_caches.as_ref()) {
                    (Some(share), Some(caches)) => {
                        let cache = caches.read().unwrap().get(&share).cloned();
                        cache.map(|c| c.cache_stats()).unwrap_or(zeros)
                    }
                    _ => zeros,
                }
            }
            #[cfg(not(any(target_os = "macos", windows)))]
            {
                let _ = &msg;
                (0u64, 0u64)
            }
        };
        let _ = self
            .ipc_tx
            .send(AgentToUfb::CacheStats(CacheStatsMsg {
                mount_id: msg.mount_id,
                hydrated_bytes,
                hydrated_count,
                command_id: msg.command_id,
            }))
            .await;
    }

    async fn send_to_mount(&self, mount_id: &str, event: MountEvent, command_id: &str) {
        match self.mounts.get(mount_id) {
            Some(instance) => {
                if instance.event_tx.send(event).await.is_ok() {
                    if !command_id.is_empty() {
                        let _ = self
                            .ipc_tx
                            .send(AgentToUfb::Ack(AckMsg {
                                command_id: command_id.into(),
                            }))
                            .await;
                    }
                }
            }
            None => {
                if !command_id.is_empty() {
                    let _ = self
                        .ipc_tx
                        .send(AgentToUfb::Error(ErrorMsg {
                            command_id: command_id.into(),
                            message: format!("unknown mount: {}", mount_id),
                        }))
                        .await;
                }
            }
        }
    }

    /// Route a MountEvent directly to a mount's orchestrator (used by
    /// tray commands).
    ///
    /// For sync-enabled mounts, we also manage the VFS server (WinFsp /
    /// NFS) lifecycle here — the orchestrator tracks its own state but
    /// has no reference to the `SyncServersRegistry`, so MountService is
    /// the only place that can tear down / rebuild the VFS backend.
    ///
    /// - `Stop`: shut down the sync server first, then forward the event
    ///   so the orchestrator transitions through its state machine.
    /// - `Start`: forward first (orchestrator re-establishes the SMB
    ///   session), then fire a scoped respawn for this domain.
    /// - `Restart`: shut down server + forward event + scoped respawn.
    pub async fn route_event(&self, mount_id: &str, event: MountEvent) {
        let is_sync = self.mounts.get(mount_id)
            .map(|i| i.config.sync_enabled)
            .unwrap_or(false);

        if !is_sync {
            if let Some(instance) = self.mounts.get(mount_id) {
                let _ = instance.event_tx.send(event).await;
            } else {
                log::warn!("route_event: unknown mount {}", mount_id);
            }
            return;
        }

        #[cfg(any(target_os = "macos", windows))]
        {
            match event {
                MountEvent::Stop => {
                    self.shutdown_sync_server(mount_id).await;
                    if let Some(instance) = self.mounts.get(mount_id) {
                        let _ = instance.event_tx.send(MountEvent::Stop).await;
                    }
                }
                MountEvent::Start => {
                    if let Some(instance) = self.mounts.get(mount_id) {
                        let _ = instance.event_tx.send(MountEvent::Start).await;
                    }
                    self.request_sync_server_spawn(mount_id).await;
                }
                MountEvent::Restart => {
                    self.shutdown_sync_server(mount_id).await;
                    if let Some(instance) = self.mounts.get(mount_id) {
                        let _ = instance.event_tx.send(MountEvent::Restart).await;
                    }
                    self.request_sync_server_spawn(mount_id).await;
                }
                other => {
                    if let Some(instance) = self.mounts.get(mount_id) {
                        let _ = instance.event_tx.send(other).await;
                    }
                }
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            if let Some(instance) = self.mounts.get(mount_id) {
                let _ = instance.event_tx.send(event).await;
            }
        }
    }

    /// Shutdown and remove a single domain's sync server from the registry.
    /// Safe to call if the domain isn't registered — no-op. Also drops the
    /// per-domain `shared_caches` entry so the next spawn reopens cache
    /// files fresh at the configured root.
    #[cfg(any(target_os = "macos", windows))]
    async fn shutdown_sync_server(&self, mount_id: &str) {
        // Resolve share_name from mount config; the registry is keyed by
        // share_name (domain), not mount_id.
        let domain = match self.mounts.get(mount_id) {
            Some(inst) => inst.config.share_name(),
            None => {
                log::warn!("shutdown_sync_server: unknown mount {}", mount_id);
                return;
            }
        };

        if let Some(registry) = self.sync_servers.as_ref() {
            let handle = registry.lock().await.remove(&domain);
            if let Some(h) = handle {
                log::info!("[sync] tearing down server for {} (via tray)", domain);
                h.shutdown_and_wait().await;
            }
        }
        if let Some(caches) = self.shared_caches.as_ref() {
            if let Ok(mut guard) = caches.write() {
                guard.remove(&domain);
            }
        }
    }

    /// Fire a scoped respawn signal for a single domain. `main.rs` listens
    /// and will rebuild just that one sync server rather than the whole set.
    #[cfg(any(target_os = "macos", windows))]
    async fn request_sync_server_spawn(&self, mount_id: &str) {
        let domain = match self.mounts.get(mount_id) {
            Some(inst) => inst.config.share_name(),
            None => return,
        };
        if let Some(tx) = self.sync_respawn_tx.as_ref() {
            let _ = tx.try_send(Some(domain));
        }
    }

    /// Stop all mounts gracefully and wait for orchestrators to finish.
    pub async fn shutdown(&mut self) {
        log::info!("Shutting down all mounts...");

        // Send Stop to all mounts
        let mut handles = Vec::new();
        for (id, instance) in self.mounts.drain() {
            log::info!("Stopping mount: {}", id);
            let _ = instance.event_tx.send(MountEvent::Stop).await;
            handles.push((id, instance.task_handle));
        }

        // Wait for all orchestrators to finish (with timeout)
        for (id, handle) in handles {
            match tokio::time::timeout(std::time::Duration::from_secs(15), handle).await {
                Ok(Ok(())) => log::info!("Mount {} shut down cleanly", id),
                Ok(Err(e)) => log::error!("Mount {} task panicked: {}", id, e),
                Err(_) => log::warn!("Mount {} shutdown timed out after 15s", id),
            }
        }

        log::info!("All mounts shut down");
    }
}
