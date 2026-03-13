use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusOption {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryOption {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultMetadata {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub due_date: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub is_tracked: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortState {
    #[serde(default)]
    pub sort_column: String,
    #[serde(default)]
    pub ascending: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderTypeConfig {
    #[serde(default)]
    pub is_shot: bool,
    #[serde(default)]
    pub is_asset: bool,
    #[serde(default)]
    pub is_posting: bool,
    #[serde(default)]
    pub is_doc: bool,
    #[serde(default)]
    pub add_action: String,
    #[serde(default)]
    pub add_action_template: String,
    #[serde(default)]
    pub add_action_template_file: String,
    #[serde(default)]
    pub status_options: Vec<StatusOption>,
    #[serde(default)]
    pub category_options: Vec<CategoryOption>,
    #[serde(default)]
    pub default_metadata: DefaultMetadata,
    #[serde(default)]
    pub display_metadata: HashMap<String, bool>,
    #[serde(default)]
    pub sort_state: SortState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub username: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    #[serde(default, deserialize_with = "deserialize_version")]
    pub version: String,
    #[serde(default)]
    pub folder_types: HashMap<String, FolderTypeConfig>,
    #[serde(default)]
    pub users: Vec<User>,
    #[serde(default, deserialize_with = "deserialize_priority_options")]
    pub priority_options: Vec<String>,
}

/// Accept version as string or number
fn deserialize_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    use serde::de;
    struct VersionVisitor;
    impl<'de> de::Visitor<'de> for VersionVisitor {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("string or number")
        }
        fn visit_str<E: de::Error>(self, v: &str) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_string<E: de::Error>(self, v: String) -> Result<String, E> { Ok(v) }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<String, E> { Ok(v.to_string()) }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<String, E> { Ok(v.to_string()) }
    }
    deserializer.deserialize_any(VersionVisitor)
}

/// Accept priority options as strings or numbers
fn deserialize_priority_options<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    use serde::de;
    struct PriorityVisitor;
    impl<'de> de::Visitor<'de> for PriorityVisitor {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("array of strings or numbers")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<String>, A::Error> {
            let mut v = Vec::new();
            while let Some(val) = seq.next_element::<serde_json::Value>()? {
                match val {
                    serde_json::Value::String(s) => v.push(s),
                    serde_json::Value::Number(n) => v.push(n.to_string()),
                    _ => v.push(val.to_string()),
                }
            }
            Ok(v)
        }
    }
    deserializer.deserialize_seq(PriorityVisitor)
}

impl ProjectConfig {
    /// Load a project config from a JSON file path.
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read config: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Failed to parse config: {}", e))
    }

    /// Load the global template from the app data directory.
    pub fn load_global_template() -> Result<Self, String> {
        let app_data = crate::utils::get_app_data_dir();
        let template_path = app_data.join("projectTemplate.json");
        if template_path.exists() {
            Self::load_from_file(&template_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Load config for a specific job (looks for .ufb/projectConfig.json in the job directory).
    pub fn load_for_job(job_path: &str) -> Result<Self, String> {
        let ufb_dir = Path::new(job_path).join(".ufb");
        // Primary: projectConfig.json (standard name)
        let primary = ufb_dir.join("projectConfig.json");
        if primary.exists() {
            return Self::load_from_file(&primary);
        }
        // Legacy fallback: config.json
        let legacy = ufb_dir.join("config.json");
        if legacy.exists() {
            return Self::load_from_file(&legacy);
        }
        Self::load_global_template()
    }

    /// Save config to a file.
    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize config: {}", e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
        std::fs::write(path, json).map_err(|e| format!("Failed to write config: {}", e))
    }

    /// Get config for a specific folder type (case-insensitive lookup).
    pub fn get_folder_type_config(&self, folder_type: &str) -> Option<&FolderTypeConfig> {
        let lower = folder_type.to_lowercase();
        self.folder_types.get(&lower).or_else(|| {
            // Fallback: try exact match, then case-insensitive scan
            self.folder_types.get(folder_type).or_else(|| {
                self.folder_types
                    .iter()
                    .find(|(k, _)| k.to_lowercase() == lower)
                    .map(|(_, v)| v)
            })
        })
    }

    /// Get status color by name from a folder type config.
    pub fn get_status_color(&self, folder_type: &str, status_name: &str) -> Option<String> {
        self.get_folder_type_config(folder_type).and_then(|ftc| {
            ftc.status_options
                .iter()
                .find(|s| s.name == status_name)
                .map(|s| s.color.clone())
        })
    }

    /// Get category color by name from a folder type config.
    pub fn get_category_color(&self, folder_type: &str, category_name: &str) -> Option<String> {
        self.get_folder_type_config(folder_type).and_then(|ftc| {
            ftc.category_options
                .iter()
                .find(|c| c.name == category_name)
                .map(|c| c.color.clone())
        })
    }
}
