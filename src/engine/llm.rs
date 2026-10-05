use std::path::{Path, PathBuf};

use anyhow::Context;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::{params::LlamaModelParams, AddBos};
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use sysinfo::System;

pub fn apply_voice_commands(raw_text: &str) -> String {
    let commands = [
        (r"(?i)\bnew paragraph\b", "\n\n"),
        (r"(?i)\bnew line\b", "\n"),
        (r"(?i)\bcomma\b", ","),
        (r"(?i)\bperiod\b", "."),
        (r"(?i)\bquestion mark\b", "?"),
        (r"(?i)\bexclamation point\b", "!"),
    ];

    let mut processed = raw_text.to_string();
    for (pattern, replacement) in commands {
        let command = regex::Regex::new(pattern).expect("voice command pattern is valid");
        processed = command.replace_all(&processed, replacement).into_owned();
    }
    let before_punctuation =
        regex::Regex::new(r"[ \t]+([,.?!])").expect("punctuation-spacing pattern is valid");
    processed = before_punctuation
        .replace_all(&processed, "$1")
        .into_owned();
    let around_newlines =
        regex::Regex::new(r"[ \t]*\n[ \t]*").expect("newline-spacing pattern is valid");
    processed = around_newlines.replace_all(&processed, "\n").into_owned();

    let scratch_that =
        regex::Regex::new(r"(?i)\bscratch that\b").expect("scratch-that pattern is valid");
    if let Some(command) = scratch_that.find(&processed) {
        let prefix = processed[..command.start()].trim_end();
        let prefix_without_terminator = prefix.trim_end_matches(['.', '?', '!', '\n', '\r']);
        let retained_end = prefix_without_terminator
            .char_indices()
            .rev()
            .find(|(_, character)| matches!(character, '.' | '?' | '!' | '\n' | '\r'))
            .map(|(index, character)| index + character.len_utf8())
            .unwrap_or(0);
        processed.truncate(retained_end);
    }

    processed.trim().to_string()
}

struct MemoryMonitor {
    sys: System,
    allow_large_models: bool,
}

impl MemoryMonitor {
    fn new(allow_large_models: bool) -> Self {
        Self {
            sys: System::new_all(),
            allow_large_models,
        }
    }

    fn get_optimal_model_path(&mut self, app_dir: &Path) -> anyhow::Result<(PathBuf, u8)> {
        self.sys.refresh_memory();
        let available_gb = self.sys.available_memory() as f64 / 1_073_741_824.0;

        let tier_1 = app_dir.join("Models/qwen2.5-3b-instruct-q4_k_m.gguf");
        let tier_2 = app_dir.join("Models/qwen2.5-1.5b-instruct-q4_k_m.gguf");
        let tier_3 = app_dir.join("Models/qwen2.5-0.5b-instruct-q4_k_m.gguf");

        if self.allow_large_models && available_gb > 8.0 && tier_1.exists() {
            Ok((tier_1, 1))
        } else if self.allow_large_models && available_gb > 4.0 && tier_2.exists() {
            Ok((tier_2, 2))
        } else if tier_3.exists() {
            Ok((tier_3, 3))
        } else {
            anyhow::bail!(
                "No supported LLM model file is available under {}; checked {}, {}, and {}",
                app_dir.join("Models").display(),
                tier_1.display(),
                tier_2.display(),
                tier_3.display()
            );
        }
    }
}

struct CachedLlm {
    model: Option<LlamaModel>,
    backend: LlamaBackend,
    model_path: PathBuf,
    tier: u8,
}

impl CachedLlm {
    fn load(model_path: PathBuf, tier: u8) -> anyhow::Result<Self> {
        let backend = LlamaBackend::init()?;
        let model = load_model_with_backend_fallback(&backend, &model_path)?;

        Ok(Self {
            model: Some(model),
            backend,
            model_path,
            tier,
        })
    }

    fn model(&self) -> anyhow::Result<&LlamaModel> {
        self.model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("The cached LLM model is not loaded"))
    }

    fn downgrade_if_needed(&mut self, model_path: PathBuf, tier: u8) -> anyhow::Result<()> {
        if tier <= self.tier || model_path == self.model_path {
            return Ok(());
        }

        tracing::warn!(
            previous_tier = self.tier,
            new_tier = tier,
            "Memory pressure detected; switching LLM model tier"
        );

        let model = load_model_with_backend_fallback(&self.backend, &model_path)?;
        self.model = Some(model);
        self.model_path = model_path;
        self.tier = tier;
        Ok(())
    }
}

fn load_model_with_backend_fallback(
    backend: &LlamaBackend,
    model_path: &Path,
) -> anyhow::Result<LlamaModel> {
    let gpu_layers = crate::engine::hardware::initialize_backend_hardware();
    let gpu_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
    match LlamaModel::load_from_file(backend, model_path, &gpu_params) {
        Ok(model) => Ok(model),
        Err(gpu_error) if gpu_layers > 0 => {
            tracing::warn!(
                error = %gpu_error,
                "GPU model loading failed; retrying with CPU inference"
            );
            let cpu_params = LlamaModelParams::default().with_n_gpu_layers(0);
            LlamaModel::load_from_file(backend, model_path, &cpu_params)
                .with_context(|| format!("Loading {} with CPU fallback", model_path.display()))
        }
        Err(error) => Err(error.into()),
    }
}

pub struct LocalLlm {
    memory_monitor: MemoryMonitor,
    cached_model: CachedLlm,
}

impl LocalLlm {
    pub fn load(app_dir: &Path, allow_large_models: bool) -> anyhow::Result<Self> {
        let mut memory_monitor = MemoryMonitor::new(allow_large_models);
        let (model_path, model_tier) = memory_monitor.get_optimal_model_path(app_dir)?;
        tracing::info!(model_tier, "Loading local LLM model");
        let cached_model = CachedLlm::load(model_path, model_tier)?;
        Ok(Self {
            memory_monitor,
            cached_model,
        })
    }

    pub fn polish(
        &mut self,
        app_dir: &Path,
        max_tokens: usize,
        system_prompt: &str,
        raw_text: &str,
    ) -> anyhow::Result<String> {
        let (model_path, target_tier) = self.memory_monitor.get_optimal_model_path(app_dir)?;
        self.cached_model
            .downgrade_if_needed(model_path, target_tier)?;

        let model = self.cached_model.model()?;
        let thread_count = crate::engine::worker_thread_count() as i32;
        let ctx_params = LlamaContextParams::default()
            .with_n_threads(thread_count)
            .with_n_threads_batch(thread_count);
        let mut ctx = model
            .new_context(&self.cached_model.backend, ctx_params)
            .context("Failed to create inference context")?;
        let prompt = format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            system_prompt, raw_text
        );

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .context("Failed to tokenize the prompt")?;
        let mut batch = LlamaBatch::new(tokens.len(), 1);
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == tokens.len() - 1)
                .context("Failed to add prompt token to inference batch")?;
        }
        ctx.decode(&mut batch)
            .context("Failed to decode the prompt")?;

        let mut generated_text = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut current_position = tokens.len() as i32;
        for _ in 0..max_tokens {
            let mut candidates =
                LlamaTokenDataArray::from_iter(ctx.candidates_ith(batch.n_tokens() - 1), false);
            let next_token = candidates.sample_token_greedy();
            if model.is_eog_token(next_token) {
                break;
            }

            generated_text.push_str(
                &model
                    .token_to_piece(next_token, &mut decoder, false, None)
                    .context("Failed to decode generated token")?,
            );

            batch.clear();
            batch
                .add(next_token, current_position, &[0], true)
                .context("Failed to add generated token to inference batch")?;
            current_position += 1;
            ctx.decode(&mut batch)?;
        }

        let _ = decoder.decode_to_string(&[], &mut generated_text, true);

        Ok(generated_text)
    }
}

#[cfg(test)]
mod tests {
    use super::apply_voice_commands;

    #[test]
    fn voice_commands_format_punctuation_case_insensitively() {
        assert_eq!(
            apply_voice_commands("Hello comma new line how are you question mark"),
            "Hello,\nhow are you?"
        );
        assert_eq!(
            apply_voice_commands("First new paragraph Second"),
            "First\n\nSecond"
        );
    }

    #[test]
    fn scratch_that_removes_the_previous_sentence() {
        assert_eq!(
            apply_voice_commands("Keep this. Remove that sentence. Scratch that"),
            "Keep this."
        );
        assert_eq!(apply_voice_commands("Remove everything scratch that"), "");
    }
}
