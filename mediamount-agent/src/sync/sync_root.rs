/// Sync root lifecycle — registration, session management, teardown.

use cloud_filter::root::{
    HydrationPolicy, HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder,
    SyncRootInfo,
};
use std::collections::HashMap;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::cache::CacheIndex;
use super::connectivity::NasConnectivity;
use super::filter::NasSyncFilter;
use super::watcher::NasWatcher;
use super::write_through::{EchoSuppressor, WriteThrough};

const PROVIDER_NAME: &str = "MediaMount";

/// Manages the lifecycle of a single Cloud Files sync root.
pub struct SyncRoot {
    mount_id: String,
    nas_root: PathBuf,
    client_root: PathBuf,
    /// Held to keep the CF session alive. Dropped on teardown.
    _connection: Option<cloud_filter::root::Connection<NasSyncFilter>>,
    /// Write-through pipeline (local saves → NAS upload → placeholder conversion).
    write_through: Option<WriteThrough>,
    /// NAS watcher — kept separately so we can stop it (the filter is moved into the CF session).
    nas_watcher: Option<Arc<NasWatcher>>,
    /// Shared connectivity state — read by all components, driven by orchestrator heartbeat.
    connectivity: Arc<NasConnectivity>,
    /// Cache index for LRU eviction.
    cache: Arc<CacheIndex>,
}

impl SyncRoot {
    /// Start or reconnect a sync root.
    /// Tries to reconnect to an existing registration first (preserves placeholders).
    /// Falls back to fresh registration if the root isn't registered yet.
    pub fn start(
        mount_id: &str,
        display_name: &str,
        nas_root: PathBuf,
        client_root: PathBuf,
        cache_limit_bytes: u64,
        volume_path: &str,
    ) -> Result<Self, String> {
        // Ensure client directory exists
        fs::create_dir_all(&client_root)
            .map_err(|e| format!("Failed to create sync root dir {:?}: {}", client_root, e))?;

        // Verify NAS is reachable
        fs::read_dir(&nas_root)
            .map_err(|e| format!("NAS unreachable at {:?}: {}", nas_root, e))?;

        // Shared state (needed before connect)
        let echo = Arc::new(EchoSuppressor::new());
        let connectivity = Arc::new(NasConnectivity::new());
        let open_handles = Arc::new(Mutex::new(HashMap::new()));

        // Cache index (SQLite, per-mount)
        let (cache_index, needs_repair) = CacheIndex::open(
            &client_root,
            mount_id,
            cache_limit_bytes,
            open_handles.clone(),
        );
        let cache = Arc::new(cache_index);

        // If DB was corrupt, rebuild with dehydrate-all
        if needs_repair {
            cache.rebuild(true);
        }

        // Create watcher
        let watcher = Arc::new(NasWatcher::new(
            nas_root.clone(),
            client_root.clone(),
            echo.clone(),
            cache.clone(),
        ));

        // Helper to create a fresh filter instance
        let make_filter = |oh: Arc<Mutex<HashMap<PathBuf, u32>>>| {
            NasSyncFilter::new(
                nas_root.clone(),
                client_root.clone(),
                watcher.clone(),
                echo.clone(),
                connectivity.clone(),
                cache.clone(),
                oh,
            )
        };

        // Try to reconnect to existing registration first (preserves placeholders).
        // If connect fails, register fresh. Session::connect() consumes the filter,
        // so we create a new instance for the registration path.
        let (connection, is_reconnect) = {
            let filter = make_filter(open_handles.clone());
            match Session::new().connect(&client_root, filter) {
                Ok(conn) => {
                    log::info!(
                        "[sync] Reconnected to existing sync root '{}' at {:?}",
                        mount_id, client_root
                    );
                    (conn, true)
                }
                Err(e) => {
                    log::info!(
                        "[sync] No existing registration for '{}' ({}), registering fresh",
                        mount_id, e
                    );
                    let filter = make_filter(open_handles.clone());
                    let conn = Self::register_fresh(mount_id, display_name, &client_root, filter)?;
                    log::info!(
                        "[sync] Registered new sync root '{}' at {:?}",
                        mount_id, client_root
                    );
                    (conn, false)
                }
            }
        };

        // Update last_connected timestamp
        cache.update_last_connected();

        // If DB is empty and this isn't a corruption rebuild, scan for hydrated files
        if !needs_repair && cache.total_cached_bytes() == 0 && is_reconnect {
            cache.rebuild(false); // scan and index, don't dehydrate
        }

        // Ensure every locally-existing directory is registered in visited_folders.
        // This is a local-only walk (no NAS I/O). Folders already in the DB keep
        // their mtime; new ones get mtime=0 which forces reconcile_startup to diff them.
        if is_reconnect {
            let seeded = Self::seed_visited_folders(&nas_root, &client_root, &cache);
            log::info!("[sync] Seeded {} local folders into visited_folders", seeded);
        }

        // Seed the watched map for overflow fallback (full_diff_all_watched).
        // Live events use prefix swap and don't need the map.
        watcher.register(nas_root.clone(), client_root.clone());
        if is_reconnect {
            for (nas_dir, client_dir, _mtime) in cache.visited_folders() {
                watcher.register(nas_dir, client_dir);
            }
        }

        // Start the NAS watcher thread
        watcher.start();

        // Start the write-through pipeline
        let write_through = WriteThrough::start(
            client_root.clone(),
            nas_root.clone(),
            echo,
            connectivity.clone(),
        );

        // Redirect the CF-created Explorer nav entry from the cache path to the
        // junction path. The CF API auto-pins the cache dir in Explorer's sidebar;
        // we patch TargetFolderPath so it points to C:\Volumes\ufb\{share} instead.
        Self::redirect_sync_root_nav_entry(mount_id, volume_path);

        Ok(Self {
            mount_id: mount_id.to_string(),
            nas_root,
            client_root,
            _connection: Some(connection),
            write_through: Some(write_through),
            nas_watcher: Some(watcher),
            connectivity,
            cache,
        })
    }

    /// Full registration of a new sync root.
    fn register_fresh(
        mount_id: &str,
        display_name: &str,
        client_root: &Path,
        filter: NasSyncFilter,
    ) -> Result<cloud_filter::root::Connection<NasSyncFilter>, String> {
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(
                SecurityId::current_user()
                    .map_err(|e| format!("Failed to get current user SID: {}", e))?,
            )
            .account_name(mount_id)
            .build();

        // Clean up stale registration if any
        let _ = sync_root_id.unregister();

        let icon_path = Self::find_icon();
        sync_root_id
            .register(
                SyncRootInfo::default()
                    .with_display_name(display_name)
                    .with_hydration_type(HydrationType::Full)
                    .with_hydration_policy(
                        HydrationPolicy::StreamingAllowed
                            | HydrationPolicy::AutoDehydrationAllowed
                            | HydrationPolicy::AllowFullRestartHydration,
                    )
                    .with_population_type(PopulationType::Full)
                    .with_allow_pinning(true)
                    .with_icon(&icon_path)
                    .with_version(env!("CARGO_PKG_VERSION"))
                    .with_path(client_root)
                    .map_err(|e| format!("Invalid sync root path: {}", e))?,
            )
            .map_err(|e| format!("Failed to register sync root: {}", e))?;

        let connection = Session::new()
            .connect(client_root, filter)
            .map_err(|e| format!("Failed to connect sync filter: {}", e))?;

        Ok(connection)
    }

    /// Disconnect the sync root session. Does NOT unregister — placeholders survive.
    /// Call `unregister()` separately to fully remove the sync root.
    pub fn stop(&mut self) {
        log::info!("[sync] Stopping sync root '{}'", self.mount_id);

        // Update last_connected timestamp before stopping
        self.cache.update_last_connected();

        // Stop write-through first (cancels pending uploads)
        if let Some(mut wt) = self.write_through.take() {
            wt.stop();
        }

        // Stop the NAS watcher (cancels ReadDirectoryChangesW)
        if let Some(watcher) = self.nas_watcher.take() {
            watcher.stop();
        }

        // Drop the connection (disconnects CF session — but registration persists!)
        self._connection.take();

        log::info!("[sync] Sync root '{}' stopped (registration preserved)", self.mount_id);
    }

    /// Fully remove the sync root registration. Only call when user disables sync.
    pub fn unregister(mount_id: &str) {
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(SecurityId::current_user().unwrap())
            .account_name(mount_id)
            .build();

        if let Err(e) = sync_root_id.unregister() {
            log::warn!(
                "[sync] Failed to unregister sync root '{}': {}",
                mount_id, e
            );
        } else {
            log::info!("[sync] Sync root '{}' unregistered", mount_id);
        }
    }

    /// Remove sync root registrations that no longer have a matching mount in config,
    /// Remove stale sync root registrations and redirect active CF nav entries
    /// to point at the junction path instead of the cache path.
    /// `active_sync_mounts` maps mount_id → volume_path for enabled sync mounts.
    pub fn cleanup_stale_roots(active_sync_mounts: &HashMap<String, String>) {
        // Enumerate SyncRootManager registry keys for our provider
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager",
            ])
            .creation_flags(0x08000000)
            .output();

        let text = match output {
            Ok(ref o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(e) => {
                log::debug!("[sync] Failed to query SyncRootManager: {}", e);
                return;
            }
        };

        // Each registry subkey name is the sync root ID.
        // Format: MediaMount!{SID}!{mount_id}
        // We match on "MediaMount!" prefix and extract mount_id as the last segment.
        let provider_prefix = format!("{}!", PROVIDER_NAME);

        for line in text.lines() {
            let trimmed = line.trim();
            let key_name = match trimmed.rsplit('\\').next() {
                Some(k) if k.starts_with(&provider_prefix) => k,
                _ => continue,
            };
            // Extract mount_id: everything after the last '!'
            let mount_id = match key_name.rsplit('!').next() {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };

            if let Some(volume_path) = active_sync_mounts.get(mount_id) {
                // Active — redirect CF nav entry to junction path
                Self::redirect_sync_root_nav_entry(mount_id, volume_path);
            } else {
                // Stale — read the NamespaceCLSID before unregister destroys the key
                let clsid = Self::lookup_namespace_clsid(mount_id);
                log::info!(
                    "[sync] Removing stale sync root '{}' (no matching mount in config)",
                    mount_id
                );
                Self::unregister(mount_id);
                // Now remove the orphaned nav entry
                if let Some(ref clsid) = clsid {
                    Self::remove_nav_entry_by_clsid(clsid);
                }
            }
        }

        // Second pass: remove orphaned NamespaceCLSIDs left in Desktop\NameSpace
        // from sync roots that were unregistered in a previous run (SyncRootManager
        // key gone, but the CLSID entry was never cleaned up).
        Self::remove_orphaned_cf_nav_entries();
    }

    /// Get shared connectivity state for the orchestrator.
    pub fn connectivity(&self) -> Arc<NasConnectivity> {
        self.connectivity.clone()
    }

    /// Stop the NAS watcher (e.g., when NAS goes offline).
    pub fn stop_watcher(&self) {
        if let Some(ref w) = self.nas_watcher {
            w.stop();
        }
    }

    /// Restart the NAS watcher after reconnect.
    pub fn restart_watcher(&self) {
        if let Some(ref w) = self.nas_watcher {
            w.restart();
        }
    }

    /// Walk local directories and ensure each has an entry in visited_folders.
    /// No NAS I/O — purely local filesystem + SQLite. Existing entries keep their
    /// mtime; new ones get mtime=0 which tells reconcile_startup to diff them.
    /// Returns the number of folders seeded.
    fn seed_visited_folders(nas_root: &Path, client_root: &Path, cache: &Arc<CacheIndex>) -> u32 {
        let mut count = 0u32;
        // Seed the root itself
        cache.ensure_visited_folder(nas_root, client_root);
        count += 1;
        // Seed all subdirectories
        Self::seed_visited_inner(nas_root, client_root, client_root, cache, &mut count);
        count
    }

    fn seed_visited_inner(
        nas_root: &Path,
        client_root: &Path,
        dir: &Path,
        cache: &Arc<CacheIndex>,
        count: &mut u32,
    ) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name.starts_with('#') || name.starts_with('@') {
                continue;
            }

            // Map local dir to NAS dir via prefix swap
            let relative = match path.strip_prefix(client_root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let nas_dir = nas_root.join(relative);

            // Insert with mtime=0 if not already in DB (existing entries keep their mtime)
            cache.ensure_visited_folder(&nas_dir, &path);
            *count += 1;

            // Recurse
            Self::seed_visited_inner(nas_root, client_root, &path, cache, count);
        }
    }

    /// Startup reconciliation: diff visited folders against NAS to catch offline changes.
    /// Uses folder mtime to skip unchanged folders, then three-way diff for changed ones.
    /// Returns (folders_checked, folders_changed, files_added, files_removed, files_updated).
    pub fn reconcile_startup(&self) -> (u32, u32, u32, u32, u32) {
        let visited = self.cache.visited_folders();
        if visited.is_empty() {
            log::info!("[sync] Startup reconciliation: no visited folders in DB");
            return (0, 0, 0, 0, 0);
        }

        log::info!(
            "[sync] Startup reconciliation: checking {} visited folders",
            visited.len()
        );

        let mut folders_checked = 0u32;
        let mut folders_changed = 0u32;
        let mut total_added = 0u32;
        let mut total_removed = 0u32;
        let mut total_updated = 0u32;

        for (nas_dir, client_dir, stored_mtime) in &visited {
            folders_checked += 1;

            // Skip folders where client dir no longer exists (user deleted sync root content)
            if !client_dir.is_dir() {
                log::debug!(
                    "[sync] Reconcile skip {:?}: client dir gone",
                    client_dir
                );
                continue;
            }

            // Stat NAS folder — skip if unreachable
            let nas_meta = match fs::metadata(nas_dir) {
                Ok(m) => m,
                Err(e) => {
                    log::debug!(
                        "[sync] Reconcile skip {:?}: NAS stat failed ({})",
                        nas_dir, e
                    );
                    continue;
                }
            };

            // Compare folder mtime — skip unchanged
            let current_mtime = nas_meta
                .modified()
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64
                })
                .unwrap_or(0);

            if current_mtime == *stored_mtime && *stored_mtime != 0 {
                log::debug!(
                    "[sync] Reconcile skip {:?}: mtime unchanged ({})",
                    nas_dir, current_mtime
                );
                continue;
            }

            folders_changed += 1;

            // Folder changed — three-way diff: DB vs NAS vs Local
            let (added, removed, updated) =
                self.reconcile_folder(nas_dir, client_dir, current_mtime);
            total_added += added;
            total_removed += removed;
            total_updated += updated;
        }

        // Second pass: scan visited folders for untracked real files that were
        // deleted on NAS (in #recycle). Honors remote deletions for files created
        // locally while offline. Untracked files NOT in #recycle are left for
        // write-through to upload naturally.
        let last_connected = self.cache.last_connected_at().unwrap_or(0);
        for (_nas_dir, client_dir, _mtime) in &visited {
            if !client_dir.is_dir() {
                continue;
            }
            let entries = match fs::read_dir(client_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Local directory not on NAS — create it
                    let relative = match path.strip_prefix(&self.client_root) {
                        Ok(r) => r,
                        Err(_) => continue,
                    };
                    let nas_dir = self.nas_root.join(relative);
                    if !nas_dir.exists() {
                        match fs::create_dir_all(&nas_dir) {
                            Ok(()) => log::info!(
                                "[sync-reconcile] Created NAS directory: {}",
                                relative.display()
                            ),
                            Err(e) => log::debug!(
                                "[sync-reconcile] Failed to create NAS dir {:?}: {}",
                                nas_dir, e
                            ),
                        }
                    }
                    continue;
                }
                if super::cache::is_cf_placeholder(&path) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let relative = match path.strip_prefix(&self.client_root) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let nas_path = self.nas_root.join(relative);
                if nas_path.exists() {
                    continue; // Already on NAS
                }
                if self.was_recycled_while_offline(&name, &path, last_connected) {
                    match fs::remove_file(&path) {
                        Ok(()) => {
                            total_removed += 1;
                            log::info!("[sync-reconcile] - {} (recycled on NAS)", name);
                        }
                        Err(e) => log::warn!("[sync-reconcile] Remove failed {}: {}", name, e),
                    }
                }
                // else: leave for write-through to upload
            }
        }

        log::info!(
            "[sync] Startup reconciliation done: {}/{} folders changed, +{} -{} ~{}",
            folders_changed,
            folders_checked,
            total_added,
            total_removed,
            total_updated,
        );

        (
            folders_checked,
            folders_changed,
            total_added,
            total_removed,
            total_updated,
        )
    }

    /// Check if a file was deleted on the NAS while we were offline (exists in #recycle
    /// with mtime after last_connected_at) AND the local file matches the recycled version.
    /// Returns true only if safe to honor the deletion — i.e., the local file isn't a
    /// newer/different version that was never uploaded.
    fn was_recycled_while_offline(
        &self,
        filename: &str,
        local_path: &Path,
        last_connected: i64,
    ) -> bool {
        let recycle_path = self.nas_root.join("#recycle").join(filename);
        let recycle_meta = match fs::metadata(&recycle_path) {
            Ok(m) => m,
            Err(_) => return false,
        };

        let recycle_mtime = recycle_meta
            .modified()
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
            .unwrap_or(0);

        // Was it recycled while we were offline?
        if recycle_mtime <= last_connected {
            return false;
        }

        // Compare local file against recycled version — only honor deletion if
        // the local file matches (same size AND not modified after recycling).
        let local_meta = match fs::metadata(local_path) {
            Ok(m) => m,
            Err(_) => return false,
        };

        let local_mtime = local_meta
            .modified()
            .map(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            })
            .unwrap_or(0);

        let size_matches = local_meta.len() == recycle_meta.len();
        let local_not_newer = local_mtime <= recycle_mtime;

        if !size_matches || !local_not_newer {
            log::info!(
                "[sync-reconcile] Keeping {} — local differs from recycled (local: {} bytes mtime={}, recycled: {} bytes mtime={})",
                filename, local_meta.len(), local_mtime, recycle_meta.len(), recycle_mtime
            );
            return false;
        }

        true
    }

    /// Three-way diff for a single folder: DB (known state) vs NAS (current) vs Local (current).
    /// NAS is truth. Returns (added, removed, updated).
    fn reconcile_folder(
        &self,
        nas_dir: &Path,
        client_dir: &Path,
        current_mtime: i64,
    ) -> (u32, u32, u32) {
        use cloud_filter::{metadata::Metadata, placeholder_file::PlaceholderFile};
        use std::collections::{HashMap, HashSet};

        let mut added = 0u32;
        let mut removed = 0u32;
        let mut updated = 0u32;

        // 1. DB state: known files in this folder (path, nas_size, nas_mtime)
        let db_files = self.cache.known_files_in_folder(client_dir);
        let db_map: HashMap<String, (i64, i64)> = db_files
            .iter()
            .map(|(path, size, mtime)| {
                let name = Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                (name, (*size, *mtime))
            })
            .collect();

        // 2. NAS state: current directory listing
        let nas_entries: HashMap<String, (u64, i64)> = fs::read_dir(nas_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                !n.starts_with('#') && !n.starts_with('@') && !n.starts_with('.')
            })
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                if meta.is_dir() {
                    return None; // Only reconcile files, not subdirs
                }
                let name = e.file_name().to_string_lossy().to_string();
                let mtime = meta
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64
                    })
                    .unwrap_or(0);
                Some((name, (meta.len(), mtime)))
            })
            .collect();

        // 3. Local state: files currently on disk in client folder
        let local_entries: HashSet<String> = fs::read_dir(client_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        let nas_names: HashSet<&String> = nas_entries.keys().collect();
        let local_names: HashSet<&String> = local_entries.iter().collect();

        // New on NAS, not in local → push placeholder
        for name in nas_names.difference(&local_names) {
            let nas_path = nas_dir.join(*name);
            let (size, mtime) = nas_entries[*name];
            let blob = nas_path.to_string_lossy().as_bytes().to_vec();
            let cf_meta = Metadata::file().size(size);
            let placeholder = PlaceholderFile::new(*name)
                .metadata(cf_meta)
                .mark_in_sync()
                .blob(blob);
            match placeholder.create::<PathBuf>(client_dir.to_path_buf()) {
                Ok(_) => {
                    let client_path = client_dir.join(*name);
                    self.cache.record_known_file(&client_path, size, mtime);
                    added += 1;
                    log::debug!("[sync-reconcile] + {} ({})", name, size);
                }
                Err(e) => log::debug!("[sync-reconcile] Failed to add {}: {}", name, e),
            }
        }

        // In local but gone from NAS → classify and handle
        let last_connected = self.cache.last_connected_at().unwrap_or(0);

        for name in local_names.difference(&nas_names) {
            let client_path = client_dir.join(*name);
            if client_path.is_dir() {
                continue;
            }

            if super::cache::is_cf_placeholder(&client_path) {
                // CF placeholder with no NAS backing — safe to remove
                match fs::remove_file(&client_path) {
                    Ok(()) => {
                        self.cache.remove_known_file(&client_path);
                        removed += 1;
                        log::debug!("[sync-reconcile] - {} (placeholder)", name);
                    }
                    Err(e) => log::debug!("[sync-reconcile] Remove skipped {}: {}", name, e),
                }
            } else if self.was_recycled_while_offline(name, &client_path, last_connected) {
                // Real file deleted on NAS while offline — honor the deletion
                match fs::remove_file(&client_path) {
                    Ok(()) => {
                        self.cache.remove_known_file(&client_path);
                        removed += 1;
                        log::info!("[sync-reconcile] - {} (recycled on NAS)", name);
                    }
                    Err(e) => log::warn!("[sync-reconcile] Remove failed {}: {}", name, e),
                }
            } else {
                // Real file not on NAS — leave for write-through to upload
                log::info!("[sync-reconcile] Untracked local file, leaving for upload: {}", name);
            }
        }

        // Files in both NAS and local — check for metadata changes (size/mtime)
        for name in nas_names.intersection(&local_names) {
            let (nas_size, nas_mtime) = nas_entries[*name];
            if let Some(&(db_size, db_mtime)) = db_map.get(*name) {
                if nas_size as i64 == db_size && nas_mtime == db_mtime {
                    continue; // No change
                }
            }
            // Size or mtime changed — update placeholder
            let client_path = client_dir.join(*name);
            let nas_path = nas_dir.join(*name);
            if client_path.is_dir() {
                continue;
            }
            // Delete and recreate (same strategy as watcher update_placeholder)
            let _ = fs::remove_file(&client_path);
            let blob = nas_path.to_string_lossy().as_bytes().to_vec();
            let cf_meta = Metadata::file().size(nas_size);
            let placeholder = PlaceholderFile::new(*name)
                .metadata(cf_meta)
                .mark_in_sync()
                .blob(blob);
            match placeholder.create::<PathBuf>(client_dir.to_path_buf()) {
                Ok(_) => {
                    self.cache
                        .record_known_file(&client_path, nas_size, nas_mtime);
                    updated += 1;
                    log::debug!("[sync-reconcile] ~ {} ({})", name, nas_size);
                }
                Err(e) => log::debug!("[sync-reconcile] Update failed {}: {}", name, e),
            }
        }

        // Update folder mtime in DB
        self.cache.update_folder_mtime(nas_dir, current_mtime);

        (added, removed, updated)
    }

    /// Clear all cached (hydrated) data for this mount.
    pub fn clear_cache(&self) -> (u32, u64) {
        self.cache.clear_all()
    }

    /// Get the current sync activity summary for UI display.
    pub fn activity_summary(&self) -> String {
        if let Some(ref wt) = self.write_through {
            wt.activity.lock().unwrap().summary()
        } else {
            "Disabled".to_string()
        }
    }

    /// Look up the NamespaceCLSID that the CF API auto-created for a sync root.
    fn lookup_namespace_clsid(mount_id: &str) -> Option<String> {
        let sid = SecurityId::current_user().ok()?;
        let sync_root_id = SyncRootIdBuilder::new(PROVIDER_NAME)
            .user_security_id(sid)
            .account_name(mount_id)
            .build();
        let id_string = sync_root_id.to_os_string().to_string_lossy().to_string();

        let srm_key = format!(
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager\{}",
            id_string
        );

        let output = std::process::Command::new("reg")
            .args(["query", &srm_key, "/v", "NamespaceCLSID"])
            .creation_flags(0x08000000)
            .output()
            .ok()?;

        let text = String::from_utf8_lossy(&output.stdout);
        text.lines()
            .find(|l| l.contains("NamespaceCLSID"))
            .and_then(|l| l.split_whitespace().last())
            .map(|s| s.to_string())
    }

    /// Redirect the CF-created Explorer nav entry to point at the junction path
    /// instead of the cache path. Patches the TargetFolderPath in the CLSID's
    /// InitPropertyBag so Explorer navigates to C:\Volumes\ufb\{share}.
    fn redirect_sync_root_nav_entry(mount_id: &str, volume_path: &str) {
        let clsid = match Self::lookup_namespace_clsid(mount_id) {
            Some(c) => c,
            None => {
                log::debug!("[sync] No NamespaceCLSID found for '{}', nothing to redirect", mount_id);
                return;
            }
        };

        let init_bag_key = format!(
            r"HKCU\Software\Classes\CLSID\{}\Instance\InitPropertyBag",
            clsid
        );

        let result = std::process::Command::new("reg")
            .args([
                "add", &init_bag_key,
                "/v", "TargetFolderPath",
                "/t", "REG_SZ",
                "/d", volume_path,
                "/f",
            ])
            .creation_flags(0x08000000)
            .output();

        match result {
            Ok(o) if o.status.success() => {
                log::info!(
                    "[sync] Redirected CF nav entry for '{}' → {} (CLSID {})",
                    mount_id, volume_path, clsid
                );
            }
            _ => {
                log::warn!("[sync] Failed to redirect CF nav entry for '{}'", mount_id);
            }
        }
    }

    /// Remove a NamespaceCLSID entry from Desktop\NameSpace by CLSID string.
    fn remove_nav_entry_by_clsid(clsid: &str) {
        let ns_key = format!(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace\{}",
            clsid
        );
        let _ = std::process::Command::new("reg")
            .args(["delete", &ns_key, "/f"])
            .creation_flags(0x08000000)
            .output();

        // Also remove the CLSID class registration itself
        let clsid_key = format!(r"HKCU\Software\Classes\CLSID\{}", clsid);
        let _ = std::process::Command::new("reg")
            .args(["delete", &clsid_key, "/f"])
            .creation_flags(0x08000000)
            .output();

        log::info!("[sync] Removed orphaned CF nav entry (CLSID {})", clsid);
    }

    /// Scan Desktop\NameSpace for entries whose default value looks like a
    /// MediaMount sync root ID (e.g. "MediaMount!S-1-5-...!mount-id") but
    /// whose SyncRootManager key no longer exists. These are orphans left
    /// behind when a sync root was unregistered without cleaning up its CLSID.
    fn remove_orphaned_cf_nav_entries() {
        let output = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace",
                "/s",
            ])
            .creation_flags(0x08000000)
            .output();

        let text = match output {
            Ok(ref o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => return,
        };

        let provider_prefix = format!("{}!", PROVIDER_NAME);
        let mut current_clsid: Option<String> = None;

        for line in text.lines() {
            let trimmed = line.trim();

            // Registry key line — extract the CLSID
            if trimmed.starts_with("HKEY_CURRENT_USER\\") {
                current_clsid = trimmed.rsplit('\\').next()
                    .filter(|s| s.starts_with('{') && s.ends_with('}'))
                    .map(|s| s.to_string());
                continue;
            }

            // Default value line — check if it's a MediaMount sync root ID
            if trimmed.starts_with("(Default)") && trimmed.contains(&provider_prefix) {
                if let Some(ref clsid) = current_clsid {
                    // This CLSID's default value is a MediaMount sync root ID.
                    // Check if the SyncRootManager entry still exists.
                    let sync_id = trimmed
                        .split("REG_SZ")
                        .nth(1)
                        .map(|s| s.trim())
                        .unwrap_or("");

                    if !sync_id.is_empty() {
                        let srm_key = format!(
                            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SyncRootManager\{}",
                            sync_id
                        );
                        let check = std::process::Command::new("reg")
                            .args(["query", &srm_key])
                            .creation_flags(0x08000000)
                            .output();

                        let exists = check.map(|o| o.status.success()).unwrap_or(false);
                        if !exists {
                            log::info!(
                                "[sync] Found orphaned CF nav entry: {} (sync root '{}')",
                                clsid, sync_id
                            );
                            Self::remove_nav_entry_by_clsid(clsid);
                        }
                    }
                }
            }
        }
    }

    /// Icon for the sync root in Explorer.
    /// Uses cloud-sync.ico next to the exe, falls back to embedded exe icon.
    fn find_icon() -> String {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let cloud_ico = dir.join("cloud-sync.ico");
                if cloud_ico.exists() {
                    let path = cloud_ico.to_string_lossy().to_string();
                    log::info!("[sync] Using sync icon: {}", path);
                    return path;
                }
            }
            // Fallback: embedded icon in the exe
            let path = format!("{},0", exe.to_string_lossy());
            log::info!("[sync] Using exe icon: {}", path);
            return path;
        }
        r"%SystemRoot%\system32\shell32.dll,12".to_string()
    }
}

impl Drop for SyncRoot {
    fn drop(&mut self) {
        if self._connection.is_some() || self.write_through.is_some() || self.nas_watcher.is_some() {
            self.stop();
        }
    }
}
