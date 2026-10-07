use anyhow::Context;
use reqwest::blocking::Client;

use crate::config::AppConfig;
use crate::engine::keychain::KeyVault;

pub struct CloudLlm {
    client: Client,
    endpoint: &'static str,
    api_key: String,
    model: String,
    provider: &'static str,
}

impl CloudLlm {
    pub fn from_config(config: &AppConfig) -> anyhow::Result<Self> {
        let (provider, endpoint, key_name, default_model) =
            if config.provider().eq_ignore_ascii_case("groq") {
                (
                    "groq",
                    "https://api.groq.com/openai/v1/chat/completions",
                    "GROQ_API_KEY",
                    "llama-3.3-70b-versatile",
                )
            } else if config.provider().eq_ignore_ascii_case("anthropic") {
                (
                    "anthropic",
                    "https://api.anthropic.com/v1/messages",
                    "ANTHROPIC_API_KEY",
                    "claude-3-5-haiku-20241022",
                )
            } else if config.provider().eq_ignore_ascii_case("openai") {
                (
                    "openai",
                    "https://api.openai.com/v1/chat/completions",
                    "OPENAI_API_KEY",
                    "gpt-4o-mini",
                )
            } else {
                anyhow::bail!(
                    "Cloud LLM provider must be \"groq\", \"openai\" or \"anthropic\", got {:?}",
                    config.provider()
                );
            };
        let api_key = match KeyVault::get_key(key_name)? {
            Some(key) => key,
            None => std::env::var(key_name).with_context(|| {
                format!(
                    "{key_name} must be configured in Windows Credential Manager or the environment"
                )
            })?,
        };
        if api_key.trim().is_empty() {
            anyhow::bail!("{key_name} is empty");
        }
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .context("Could not initialize the cloud LLM HTTP client")?;
        let configured_model = if config.api.model.is_some() {
            config.model()
        } else if provider == "groq" {
            config.model()
        } else {
            default_model
        };

        Ok(Self {
            client,
            endpoint,
            api_key,
            model: configured_model.to_string(),
            provider,
        })
    }

    pub fn polish(
        &self,
        max_tokens: usize,
        system_prompt: &str,
        raw_text: &str,
    ) -> anyhow::Result<String> {
        let request = if self.provider == "anthropic" {
            self.client
                .post(self.endpoint)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&serde_json::json!({
                    "model": self.model,
                    "system": system_prompt,
                    "messages": [{"role": "user", "content": raw_text}],
                    "temperature": 0.2,
                    "max_tokens": max_tokens
                }))
        } else {
            self.client
                .post(self.endpoint)
                .bearer_auth(&self.api_key)
                .json(&serde_json::json!({
                    "model": self.model,
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": raw_text}
                    ],
                    "temperature": 0.2,
                    "max_tokens": max_tokens
                }))
        };
        let response = request
            .send()
            .with_context(|| format!("Could not connect to {}", self.provider))?;
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("Could not read {}'s response", self.provider))?;
        if !status.is_success() {
            anyhow::bail!("{} returned HTTP {status}: {body}", self.provider);
        }
        let response: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("{} returned invalid JSON", self.provider))?;
        let content = if self.provider == "anthropic" {
            &response["content"][0]["text"]
        } else {
            &response["choices"][0]["message"]["content"]
        };
        content
            .as_str()
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("{} returned no polished text", self.provider))
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{AppConfig, Prompts, Settings};
    use std::collections::HashMap;

    #[test]
    fn cloud_provider_and_model_override_legacy_settings() {
        let config = AppConfig {
            settings: Settings {
                hotkey: "RightAlt".into(),
                max_tokens: 64,
                provider: "local".into(),
                groq_model: "legacy-model".into(),
                language: "auto".into(),
                translate_to_english: true,
                auto_start: false,
                hands_free: false,
                silence_timeout_ms: 1500,
                noise_suppression: false,
                has_completed_onboarding: true,
                whisper_model: "ggml-base.bin".into(),
                input_device: None,
            },
            api: crate::config::ApiConfig {
                provider: Some("openai".into()),
                model: Some("custom-model".into()),
            },
            prompts: Prompts {
                default: String::new(),
                coding: String::new(),
                professional: String::new(),
            },
            vocabulary: HashMap::new(),
            voice_commands: Default::default(),
            app_profiles: Default::default(),
        };

        assert_eq!(config.provider(), "openai");
        assert_eq!(config.model(), "custom-model");
        assert_ne!(config.provider(), "local");
    }
}
