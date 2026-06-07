use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_window_size() -> usize {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
    
    #[serde(default = "default_window_size")]
    pub context_window_size: usize,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("DEEPSEEK_API_KEY").unwrap_or_default(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-v4-flash".to_string(),
            language: "Tiếng Việt".to_string(),
            context_window_size: 1,
        }
    }
}

impl AppConfig {
    /// Get the path to the config file: ~/.config/my_reader/config.json
    pub fn config_path() -> Option<PathBuf> {
        let base_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .ok()
            })?;
        Some(base_dir.join("my_reader").join("config.json"))
    }

    /// Load the configuration from ~/.config/my_reader/config.json or fall back to defaults
    pub fn load() -> Self {
        if let Some(path) = Self::config_path() {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(config) = serde_json::from_str::<Self>(&content) {
                        return config;
                    }
                }
            }
        }
        Self::default()
    }

    /// Save the configuration to disk
    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path()
            .ok_or_else(|| "Không xác định được thư mục Home để lưu cấu hình.".to_string())?;

        // Ensure parent directories exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Không thể tạo thư mục cấu hình: {}", e))?;
        }

        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Lỗi cấu hình serialization: {}", e))?;

        std::fs::write(path, json)
            .map_err(|e| format!("Lỗi ghi file cấu hình: {}", e))?;

        Ok(())
    }
}
