import { invoke } from "@tauri-apps/api/core";
import type {
  Subscription,
  TrackedItemRecord,
  ColumnDefinition,
  ColumnPreset,
  Bookmark,
  FileEntry,
  ProjectConfig,
  FolderTypeConfig,
  AppSettings,
  MeshSyncStatus,
  PeerInfo,
  BackupInfo,
} from "./types";

// ── Subscriptions ──

export const subscribeToJob = (jobPath: string, jobName: string) =>
  invoke<Subscription>("subscribe_to_job", { jobPath, jobName });

export const unsubscribeFromJob = (jobPath: string) =>
  invoke<void>("unsubscribe_from_job", { jobPath });

export const getSubscriptions = () =>
  invoke<Subscription[]>("get_subscriptions");

// ── Metadata ──

export const getItemMetadata = (itemPath: string) =>
  invoke<string | null>("get_item_metadata", { itemPath });

export const upsertItemMetadata = (
  jobPath: string,
  itemPath: string,
  folderName: string,
  metadataJson: string,
  isTracked: boolean
) =>
  invoke<void>("upsert_item_metadata", {
    jobPath,
    itemPath,
    folderName,
    metadataJson,
    isTracked,
  });

export const getTrackedItems = (jobPath: string) =>
  invoke<TrackedItemRecord[]>("get_tracked_items", { jobPath });

export const getAllTrackedItems = () =>
  invoke<TrackedItemRecord[]>("get_all_tracked_items");

export const getFolderMetadata = (jobPath: string, folderName: string) =>
  invoke<import("./types").ItemMetadataRecord[]>("get_folder_metadata", { jobPath, folderName });

export const flushMetadataWrites = () =>
  invoke<number>("flush_metadata_writes");

// ── Columns ──

export const getColumnDefs = (jobPath: string, folderName: string) =>
  invoke<ColumnDefinition[]>("get_column_defs", { jobPath, folderName });

export const addColumn = (def: ColumnDefinition) =>
  invoke<number>("add_column", { def });

export const updateColumn = (def: ColumnDefinition) =>
  invoke<void>("update_column", { def });

export const deleteColumn = (id: number) =>
  invoke<void>("delete_column", { id });

// ── Column Presets ──

export const getColumnPresets = () =>
  invoke<ColumnPreset[]>("get_column_presets");

export const saveColumnPreset = (presetName: string, column: ColumnDefinition) =>
  invoke<number>("save_column_preset", { presetName, column });

export const deleteColumnPreset = (id: number) =>
  invoke<void>("delete_column_preset", { id });

export const addPresetColumn = (presetId: number, jobPath: string, folderName: string) =>
  invoke<number>("add_preset_column", { presetId, jobPath, folderName });

// ── Bookmarks ──

export const getBookmarks = () => invoke<Bookmark[]>("get_bookmarks");

export const addBookmark = (path: string, displayName: string, isProjectFolder: boolean = false) =>
  invoke<Bookmark>("add_bookmark", { path, displayName, isProjectFolder });

export const removeBookmark = (path: string) =>
  invoke<void>("remove_bookmark", { path });

// ── File Operations ──

export const listDirectory = (path: string) =>
  invoke<FileEntry[]>("list_directory", { path });

export const createDirectory = (path: string) =>
  invoke<void>("create_directory", { path });

export const renamePath = (oldPath: string, newPath: string) =>
  invoke<void>("rename_path", { oldPath, newPath });

export const deleteToTrash = (paths: string[]) =>
  invoke<void>("delete_to_trash", { paths });

export const copyFiles = (sources: string[], dest: string) =>
  invoke<void>("copy_files", { sources, dest });

export const moveFiles = (sources: string[], dest: string) =>
  invoke<void>("move_files", { sources, dest });

export const clipboardCopyPaths = (paths: string[]) =>
  invoke<void>("clipboard_copy_paths", { paths });

export const clipboardPaste = (dest: string) =>
  invoke<string[]>("clipboard_paste", { dest });

export const revealInFileManager = (path: string) =>
  invoke<void>("reveal_in_file_manager", { path });

export const openFile = (path: string) => invoke<void>("open_file", { path });

// ── Search ──

export const searchFiles = (query: string, scopePath?: string) =>
  invoke<FileEntry[]>("search_files", { query, scopePath });

// ── Config ──

export const loadProjectConfig = (jobPath: string) =>
  invoke<ProjectConfig>("load_project_config", { jobPath });

export const getFolderTypeConfig = (jobPath: string, folderType: string) =>
  invoke<FolderTypeConfig | null>("get_folder_type_config", {
    jobPath,
    folderType,
  });

// ── Settings ──

export const loadSettings = () => invoke<AppSettings>("load_settings");

export const saveSettings = (settings: AppSettings) =>
  invoke<void>("save_settings", { settings });

// ── Mesh Sync ──

export const getMeshStatus = () =>
  invoke<MeshSyncStatus>("get_mesh_status");

export const setMeshEnabled = (enabled: boolean) =>
  invoke<void>("set_mesh_enabled", { enabled });

export const triggerFlushEdits = () => invoke<void>("trigger_flush_edits");

export const triggerSnapshot = () => invoke<void>("trigger_snapshot");

export const getMeshPeers = () => invoke<PeerInfo[]>("get_mesh_peers");

// ── URI / Links ──

export const buildUfbUri = (path: string) =>
  invoke<string>("build_ufb_uri", { path });

export const buildUnionUri = (path: string) =>
  invoke<string>("build_union_uri", { path });

export const resolveUfbUri = (uri: string) =>
  invoke<string>("resolve_ufb_uri", { uri });

// ── Special Paths ──

export const getSpecialPaths = () =>
  invoke<Record<string, string>>("get_special_paths");

export const getDrives = () =>
  invoke<[string, string][]>("get_drives");

// ── Dialogs ──

export const pickFolder = (title?: string) =>
  invoke<string | null>("pick_folder", { title });

// ── Drag ──

/** Start native OS drag. Blocks until user drops or cancels. Returns "copied", "moved", or "cancelled". */
export const startNativeDrag = (paths: string[]) =>
  invoke<string>("start_native_drag", { paths });

// ── Thumbnails ──

/** Get a thumbnail as a data:image/png;base64,... URL. Returns null if not supported. */
export const getThumbnail = (filePath: string) =>
  invoke<string | null>("get_thumbnail", { filePath });

// ── Backup ──

export const listBackups = (jobPath: string) =>
  invoke<BackupInfo[]>("list_backups", { jobPath });

export const restoreBackup = (jobPath: string, filename: string) =>
  invoke<string>("restore_backup", { jobPath, filename });

// ── Item Creation ──

export const getFolderAddMode = (folderName: string) =>
  invoke<string>("get_folder_add_mode", { folderName });

export const detectFolderLayoutMode = (jobPath: string, folderName: string) =>
  invoke<string>("detect_folder_layout_mode", { jobPath, folderName });

export const createItemFromTemplate = (jobPath: string, folderPath: string, itemName: string) =>
  invoke<string>("create_item_from_template", { jobPath, folderPath, itemName });

export const createDatePrefixedItem = (folderPath: string, baseName: string) =>
  invoke<string>("create_date_prefixed_item", { folderPath, baseName });

export const createJobFromTemplate = (parentPath: string, jobNumber: string, jobName: string) =>
  invoke<string>("create_job_from_template", { parentPath, jobNumber, jobName });

// ── Transcode ──

export const transcodeAddJobs = (paths: string[]) =>
  invoke<import("../stores/transcodeStore").TranscodeJob[]>("transcode_add_jobs", { paths });

export const transcodeGetQueue = () =>
  invoke<import("../stores/transcodeStore").TranscodeJob[]>("transcode_get_queue");

export const transcodeCancelJob = (id: string) =>
  invoke<void>("transcode_cancel_job", { id });

export const transcodeRemoveJob = (id: string) =>
  invoke<void>("transcode_remove_job", { id });

export const transcodeClearCompleted = () =>
  invoke<void>("transcode_clear_completed");

// ── App lifecycle ──

export const relaunchApp = () => invoke<void>("relaunch_app");
