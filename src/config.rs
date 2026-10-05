use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub settings: Settings,
    #[serde(default)]
    pub api: ApiConfig,
    pub prompts: Prompts,
    #[serde(default)]
    pub vocabulary: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ApiConfig {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

impl AppConfig {
    pub fn provider(&self) -> &str {
        self.api
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|provider| !provider.is_empty())
            .unwrap_or(self.settings.provider.trim())
    }

    pub fn model(&self) -> &str {
        self.api
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(self.settings.groq_model.trim())
    }
}

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub hotkey: String,
    pub max_tokens: usize,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_groq_model")]
    pub groq_model: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_translate_to_english")]
    pub translate_to_english: bool,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub hands_free: bool,
    #[serde(default = "default_silence_timeout_ms")]
    pub silence_timeout_ms: u64,
}

fn default_provider() -> String {
    "local".to_string()
}

fn default_groq_model() -> String {
    "llama-3.3-70b-versatile".to_string()
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_translate_to_english() -> bool {
    true
}

fn default_silence_timeout_ms() -> u64 {
    1500
}

#[derive(Debug, Deserialize)]
pub struct Prompts {
    pub default: String,
    pub coding: String,
    pub professional: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            settings: Settings {
                hotkey: "RightAlt".to_string(),
                max_tokens: 64,
                provider: default_provider(),
                groq_model: default_groq_model(),
                language: default_language(),
                translate_to_english: default_translate_to_english(),
                auto_start: false,
                hands_free: false,
                silence_timeout_ms: default_silence_timeout_ms(),
            },
            api: ApiConfig::default(),
            prompts: Prompts {
                default: "You are a professional text-cleaning assistant. Clean up the user's raw voice transcript, remove filler words (like 'um', 'uh', 'ah'), fix punctuation, and output ONLY the polished text.".to_string(),
                coding: "You are a technical dictation assistant. Format the user's speech into clean technical notes, documentation, or code comments. Remove filler words and fix punctuation.".to_string(),
                professional: "You are a professional writing assistant. Format the user's speech into polished, formal, well-punctuated business prose. Remove all filler words.".to_string(),
            },
            vocabulary: HashMap::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Auto,
    Coding,
    Professional,
}

impl PromptMode {
    pub fn resolve<'a>(self, prompts: &'a Prompts, window_title: &str) -> &'a str {
        let window_title = window_title.to_lowercase();
        match self {
            Self::Coding => &prompts.coding,
            Self::Professional => &prompts.professional,
            Self::Auto
                if window_title.contains("visual studio code") || window_title.contains("code") =>
            {
                &prompts.coding
            }
            Self::Auto
                if window_title.contains("word")
                    || window_title.contains("outlook")
                    || window_title.contains("gmail") =>
            {
                &prompts.professional
            }
            Self::Auto => &prompts.default,
        }
    }
}

pub fn load_config(app_dir: &Path) -> anyhow::Result<AppConfig> {
    let config_path = app_dir.join("config.toml");
    let raw = match fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!("No config.toml found; using built-in defaults");
            return Ok(AppConfig::default());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Reading {}", config_path.display()));
        }
    };

    toml::from_str::<AppConfig>(&raw).with_context(|| format!("Parsing {}", config_path.display()))
}
