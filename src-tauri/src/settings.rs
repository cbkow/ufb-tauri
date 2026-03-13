use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    #[serde(default = "default_neg_one")]
    pub x: i32,
    #[serde(default = "default_neg_one")]
    pub y: i32,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelVisibility {
    #[serde(default = "default_true", alias = "show_subscriptions")]
    pub show_subscriptions: bool,
    #[serde(default = "default_true", alias = "show_browser1")]
    pub show_browser1: bool,
    #[serde(default = "default_true", alias = "show_browser2")]
    pub show_browser2: bool,
    #[serde(default, alias = "show_transcode_queue")]
    pub show_transcode_queue: bool,
    #[serde(default = "default_true", alias = "use_windows_accent")]
    pub use_windows_accent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSettings {
    #[serde(default = "default_true", alias = "use_windows_accent_color")]
    pub use_windows_accent_color: bool,
    #[serde(default = "default_neg_one_i32", alias = "custom_accent_color_index")]
    pub custom_accent_color_index: i32,
    #[serde(default = "default_half", alias = "custom_picker_color_r")]
    pub custom_picker_color_r: f32,
    #[serde(default = "default_half", alias = "custom_picker_color_g")]
    pub custom_picker_color_g: f32,
    #[serde(default = "default_half", alias = "custom_picker_color_b")]
    pub custom_picker_color_b: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSettings {
    #[serde(default = "default_one_f32", alias = "font_scale")]
    pub font_scale: f32,
    #[serde(default = "default_panel_ratios", alias = "browser_panel_ratios")]
    pub browser_panel_ratios: Vec<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshSyncSettings {
    #[serde(default, alias = "node_id")]
    pub node_id: String,
    #[serde(default, alias = "farm_path")]
    pub farm_path: String,
    #[serde(default = "default_http_port", alias = "http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub tags: String,
    #[serde(default, alias = "api_secret")]
    pub api_secret: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleDriveSettings {
    #[serde(default, alias = "script_url")]
    pub script_url: String,
    #[serde(default, alias = "parent_folder_id")]
    pub parent_folder_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSettings {
    #[serde(default)]
    pub enabled: bool,
}

/// A path mapping rule for cross-OS URI translation.
/// Each triplet maps equivalent paths across Windows, macOS, and Linux.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathMapping {
    #[serde(default)]
    pub win: String,
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub lin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobViewState {
    #[serde(alias = "job_path")]
    pub job_path: String,
    #[serde(alias = "job_name")]
    pub job_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub window: WindowState,
    #[serde(default)]
    pub panels: PanelVisibility,
    #[serde(default)]
    pub appearance: AppearanceSettings,
    #[serde(default)]
    pub ui: UiSettings,
    #[serde(default)]
    pub sync: SyncSettings,
    #[serde(default, alias = "mesh_sync")]
    pub mesh_sync: MeshSyncSettings,
    #[serde(default, alias = "google_drive")]
    pub google_drive: GoogleDriveSettings,
    #[serde(default, alias = "path_mappings")]
    pub path_mappings: Vec<PathMapping>,
    #[serde(default, alias = "job_views")]
    pub job_views: Vec<JobViewState>,
    #[serde(default, alias = "aggregated_tracker_open")]
    pub aggregated_tracker_open: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: -1,
            y: -1,
            width: 1914,
            height: 1060,
            maximized: false,
        }
    }
}

impl Default for PanelVisibility {
    fn default() -> Self {
        Self {
            show_subscriptions: true,
            show_browser1: true,
            show_browser2: true,
            show_transcode_queue: false,
            use_windows_accent: true,
        }
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            use_windows_accent_color: true,
            custom_accent_color_index: -1,
            custom_picker_color_r: 0.5,
            custom_picker_color_g: 0.5,
            custom_picker_color_b: 0.5,
        }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            browser_panel_ratios: vec![0.20, 0.40, 0.40],
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            window: WindowState::default(),
            panels: PanelVisibility::default(),
            appearance: AppearanceSettings::default(),
            ui: UiSettings::default(),
            sync: SyncSettings::default(),
            mesh_sync: MeshSyncSettings::default(),
            google_drive: GoogleDriveSettings::default(),
            path_mappings: vec![],
            job_views: vec![],
            aggregated_tracker_open: false,
        }
    }
}

impl AppSettings {
    /// Load settings from disk, falling back to defaults.
    pub fn load() -> Self {
        let path = crate::utils::get_settings_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        }
    }

    /// Save settings to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = crate::utils::get_settings_path();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize settings: {}", e))?;
        std::fs::write(&path, json).map_err(|e| format!("Failed to write settings: {}", e))
    }
}

// Default value helpers for serde
fn default_neg_one() -> i32 {
    -1
}
fn default_neg_one_i32() -> i32 {
    -1
}
fn default_width() -> u32 {
    1914
}
fn default_height() -> u32 {
    1060
}
fn default_true() -> bool {
    true
}
fn default_half() -> f32 {
    0.5
}
fn default_one_f32() -> f32 {
    1.0
}
fn default_panel_ratios() -> Vec<f32> {
    vec![0.20, 0.40, 0.40]
}
fn default_http_port() -> u16 {
    49200
}
