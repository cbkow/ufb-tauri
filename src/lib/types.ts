// ── Shared TypeScript types matching Rust structs ──

// Subscriptions
export interface Subscription {
  id: number;
  jobPath: string;
  jobName: string;
  isActive: boolean;
  subscribedTime: number;
  lastSyncTime: number | null;
  syncStatus: "Pending" | "Syncing" | "Synced" | "Stale" | "Error";
  shotCount: number;
}

export interface TrackedItemRecord {
  itemPath: string;
  jobPath: string;
  jobName: string;
  folderName: string;
  metadataJson: string;
  modifiedTime: number | null;
}

export interface ItemMetadataRecord {
  itemPath: string;
  folderName: string;
  metadataJson: string;
  isTracked: boolean;
}

// Item metadata (parsed from metadataJson)
export interface ItemMetadata {
  status?: string;
  category?: string;
  priority?: number;
  dueDate?: string;
  artist?: string;
  note?: string;
  links?: string[];
  isTracked?: boolean;
  [key: string]: unknown; // dynamic column values
}

// Column definitions
export interface ColumnOption {
  id?: number;
  name: string;
  color?: string;
}

export interface ColumnDefinition {
  id?: number;
  jobPath: string;
  folderName: string;
  columnName: string;
  columnType: "text" | "dropdown" | "date" | "number" | "priority" | "checkbox" | "links" | "note";
  columnOrder: number;
  columnWidth: number;
  isVisible: boolean;
  defaultValue?: string;
  options: ColumnOption[];
}

// Column presets
export interface ColumnPreset {
  id: number;
  presetName: string;
  columnsJson: string;
  createdTime: number;
  modifiedTime: number;
}

// Bookmarks
export interface Bookmark {
  id: number;
  path: string;
  displayName: string;
  createdTime: number;
  isProjectFolder: boolean;
}

// File entries
export interface FileEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  modified: number | null;
  extension: string;
}

// Project config
export interface StatusOption {
  name: string;
  color: string;
}

export interface CategoryOption {
  name: string;
  color: string;
}

export interface DefaultMetadata {
  status: string;
  category: string;
  priority: number;
  dueDate: string;
  artist: string;
  note: string;
  links: string[];
  isTracked: boolean;
}

export interface SortState {
  sortColumn: string;
  ascending: boolean;
}

export interface FolderTypeConfig {
  isShot: boolean;
  isAsset: boolean;
  isPosting: boolean;
  isDoc: boolean;
  addAction: string;
  addActionTemplate: string;
  addActionTemplateFile: string;
  statusOptions: StatusOption[];
  categoryOptions: CategoryOption[];
  defaultMetadata: DefaultMetadata;
  displayMetadata: Record<string, boolean>;
  sortState: SortState;
}

export interface User {
  username: string;
  displayName: string;
}

export interface ProjectConfig {
  version: string;
  folderTypes: Record<string, FolderTypeConfig>;
  users: User[];
  priorityOptions: (string | number)[];
}

// Settings
export interface WindowState {
  x: number;
  y: number;
  width: number;
  height: number;
  maximized: boolean;
}

export interface PanelVisibility {
  showSubscriptions: boolean;
  showBrowser1: boolean;
  showBrowser2: boolean;
  showTranscodeQueue: boolean;
  useWindowsAccent: boolean;
}

export interface AppearanceSettings {
  useWindowsAccentColor: boolean;
  customAccentColorIndex: number;
  customPickerColorR: number;
  customPickerColorG: number;
  customPickerColorB: number;
}

export interface UiSettings {
  fontScale: number;
  browserPanelRatios: number[];
}

export interface MeshSyncSettings {
  nodeId: string;
  farmPath: string;
  httpPort: number;
  tags: string;
  apiSecret: string;
}

export interface GoogleDriveSettings {
  scriptUrl: string;
  parentFolderId: string;
}

export interface JobViewState {
  jobPath: string;
  jobName: string;
}

export interface PathMapping {
  win: string;
  mac: string;
  lin: string;
}

export interface AppSettings {
  window: WindowState;
  panels: PanelVisibility;
  appearance: AppearanceSettings;
  ui: UiSettings;
  sync: { enabled: boolean };
  meshSync: MeshSyncSettings;
  googleDrive: GoogleDriveSettings;
  pathMappings: PathMapping[];
  jobViews: JobViewState[];
  aggregatedTrackerOpen: boolean;
}

// Mesh sync peers
export interface PeerEndpoint {
  node_id: string;
  ip: string;
  port: number;
  timestamp_ms: number;
  tags: string[];
}

export interface PeerInfo {
  nodeId: string;
  tags: string[];
  endpoint: PeerEndpoint;
  isAlive: boolean;
  isLeader: boolean;
  failedPolls: number;
  lastSeenMs: number;
  hasUdpContact: boolean;
  lastUdpContactMs: number;
}

// Mesh sync
export interface MeshSyncStatus {
  isLeader: boolean;
  leaderId: string;
  peerCount: number;
  lastSnapshotTime: number | null;
  pendingEditsCount: number;
  statusMessage: string;
  isEnabled: boolean;
  isConfigured: boolean;
}

// Mount (MediaMount Agent)
export interface MountStateUpdate {
  mountId: string;
  state: string;
  stateDetail: string;
  cacheUsedBytes: number;
  cacheMaxBytes: number;
  dirtyFiles: number;
  lastFallbackTime: number | null;
  isRcloneActive: boolean;
  isSmbActive: boolean;
}

export interface MountConfig {
  id: string;
  enabled: boolean;
  displayName: string;
  nasSharePath: string;
  credentialKey: string;
  rcloneDriveLetter: string;
  smbDriveLetter?: string;
  junctionPath: string;
  cacheDirPath: string;
  cacheMaxSize: string;
  cacheMaxAge: string;
  vfsWriteBack: string;
  vfsReadChunkSize: string;
  vfsReadChunkStreams: number;
  vfsReadAhead: string;
  bufferSize: string;
  probeIntervalSecs: number;
  probeTimeoutMs: number;
  fallbackThreshold: number;
  recoveryThreshold: number;
  maxRcloneStartAttempts: number;
  healthcheckFileName: string;
  extraRcloneFlags: string[];
}

export interface MountsConfig {
  version: number;
  mounts: MountConfig[];
}

// Backup
export interface BackupInfo {
  timestamp: number;
  filename: string;
  createdBy: string;
  shotCount: number;
  checksum: string;
  uncompressedSize: number;
  date: string;
}
