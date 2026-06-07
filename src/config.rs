use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub language: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("DEEPSEEK_API_KEY").unwrap_or_default(),
            base_url: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-v4-flash".to_string(),
            language: "Tiếng Việt".to_string(),
        }
    }
}
