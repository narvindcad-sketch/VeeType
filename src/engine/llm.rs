use std::path::{Path, PathBuf};

use anyhow::Context;
use enigo::{Enigo, KeyboardControllable};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::token::data_array::LlamaTokenDataArray;
use sysinfo::System;

struct MemoryMonitor {
    sys: System,
}

impl MemoryMonitor {
    fn new() -> Self {
        Self {
            sys: System::new_all(),
        }
    }

    fn get_optimal_model_path(&mut self, app_dir: &Path) -> anyhow::Result<(PathBuf, u8)> {
        self.sys.refresh_memory();
        let available_gb = self.sys.available_memory() as f64 / 1_073_741_824.0;

        let tier_1 = app_dir.join("Models/qwen2.5-3b-instruct-q4_k_m.gguf");
        let tier_2 = app_dir.join("Models/qwen2.5-1.5b-instruct-q4_k_m.gguf");
        let tier_3 = app_dir.join("Models/qwen2.5-0.5b-instruct-q4_k_m.gguf");

        if available_gb > 8.0 && tier_1.exists() {
            Ok((tier_1, 1))
        } else if available_gb > 4.0 && tier_2.exists() {
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
        let model_params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)?;

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

        let model_params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(&self.backend, &model_path, &model_params)?;
        self.model = Some(model);
        self.model_path = model_path;
        self.tier = tier;
        Ok(())
    }
}

pub struct LocalLlm {
    memory_monitor: MemoryMonitor,
    cached_model: CachedLlm,
}

impl LocalLlm {
    pub fn load(app_dir: &Path) -> anyhow::Result<Self> {
        let mut memory_monitor = MemoryMonitor::new();
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
        enigo: &mut Enigo,
    ) -> anyhow::Result<String> {
        let (model_path, target_tier) = self.memory_monitor.get_optimal_model_path(app_dir)?;
        self.cached_model
            .downgrade_if_needed(model_path, target_tier)?;

        let model = self.cached_model.model()?;
        let ctx_params = LlamaContextParams::default();
        let mut ctx = model
            .new_context(&self.cached_model.backend, ctx_params)
            .context("Failed to create inference context")?;
        let prompt = format!(
            "<|im_start|>system\n{}<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            system_prompt, raw_text
        );

        let vocab = model.vocab();
        let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
        let mut batch = LlamaBatch::new(tokens.len(), 1);
        for (i, &token) in tokens.iter().enumerate() {
            batch
                .add(token, i as i32, &[0], i == tokens.len() - 1)
                .context("Failed to add prompt token to inference batch")?;
        }
        ctx.decode(&mut batch)
            .context("Failed to decode the prompt")?;

        let mut generated_text = String::new();
        let mut pending_bytes = Vec::new();
        let mut current_position = tokens.len() as i32;
        for _ in 0..max_tokens {
            let mut candidates =
                LlamaTokenDataArray::from_iter(ctx.candidates_ith(batch.n_tokens() - 1), false);
            let next_token = candidates.sample_token_greedy();
            if vocab.is_eog(next_token) {
                break;
            }

            pending_bytes.extend_from_slice(&vocab.token_to_piece(next_token, false, None));
            match std::str::from_utf8(&pending_bytes) {
                Ok(piece) => {
                    if !piece.is_empty() {
                        generated_text.push_str(piece);
                        enigo.key_sequence(piece);
                    }
                    pending_bytes.clear();
                }
                Err(error) => {
                    let valid_len = error.valid_up_to();
                    if valid_len > 0 {
                        let piece = std::str::from_utf8(&pending_bytes[..valid_len])?;
                        generated_text.push_str(piece);
                        enigo.key_sequence(piece);
                        pending_bytes.drain(..valid_len);
                    }
                    if error.error_len().is_some() {
                        let piece = String::from_utf8_lossy(&pending_bytes);
                        generated_text.push_str(&piece);
                        enigo.key_sequence(piece.as_ref());
                        pending_bytes.clear();
                    }
                }
            }

            batch.clear();
            batch
                .add(next_token, current_position, &[0], true)
                .context("Failed to add generated token to inference batch")?;
            current_position += 1;
            ctx.decode(&mut batch)?;
        }

        if !pending_bytes.is_empty() {
            let piece = String::from_utf8_lossy(&pending_bytes);
            generated_text.push_str(&piece);
            enigo.key_sequence(piece.as_ref());
        }

        Ok(generated_text)
    }
}
