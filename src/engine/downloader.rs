//! Background model downloader that streams files to disk without blocking the UI.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use sha2::{Digest, Sha256};

pub const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
const PARAKEET_FILE: &str = "parakeet-unified-en-0.6b-Q8_0.gguf";
const PARAKEET_URL: &str = "https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/7e948f21b7bdbac698d3318db9d350f1096f3b6c/parakeet-unified-en-0.6b-Q8_0.gguf";
const PARAKEET_SHA256: &str = "4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795";

fn model_url(file_name: &str) -> String {
    match file_name {
        PARAKEET_FILE => PARAKEET_URL.to_string(),
        _ => format!("{MODEL_BASE_URL}{file_name}"),
    }
}

fn expected_sha256(file_name: &str) -> Option<&'static str> {
    (file_name == PARAKEET_FILE).then_some(PARAKEET_SHA256)
}

pub struct Download {
    downloaded: AtomicU64,
    total: AtomicU64,
    finished: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Download {
    /// Fraction complete in 0.0..=1.0 (0.0 while the size is still unknown).
    pub fn progress(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.downloaded.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }

    /// Starts downloading `file_name` from the model repository into `models_dir`.
    pub fn start(file_name: &str, models_dir: &Path, ctx: eframe::egui::Context) -> Arc<Self> {
        let download = Arc::new(Self {
            downloaded: AtomicU64::new(0),
            total: AtomicU64::new(0),
            finished: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        let url = model_url(file_name);
        let expected_sha256 = expected_sha256(file_name);
        let destination = models_dir.join(file_name);
        let worker = Arc::clone(&download);
        std::thread::spawn(move || {
            if let Err(error) = worker.run(&url, &destination, expected_sha256, &ctx) {
                tracing::warn!(error = %error, url = %url, "Model download failed");
                if let Ok(mut slot) = worker.error.lock() {
                    *slot = Some(format!("{error:#}"));
                }
            }
            worker.finished.store(true, Ordering::Release);
            ctx.request_repaint();
        });
        download
    }

    fn run(
        &self,
        url: &str,
        destination: &PathBuf,
        expected_sha256: Option<&str>,
        ctx: &eframe::egui::Context,
    ) -> anyhow::Result<()> {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| format!("Creating {}", parent.display()))?;
        }
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Some(Duration::from_secs(3 * 60 * 60)))
            .build()?;
        let mut response = client.get(url).send()?.error_for_status()?;
        self.total
            .store(response.content_length().unwrap_or(0), Ordering::Relaxed);

        let partial = destination.with_extension("bin.part");
        let result = (|| -> anyhow::Result<()> {
            let mut file = File::create(&partial)
                .with_context(|| format!("Creating {}", partial.display()))?;
            let mut buffer = vec![0_u8; 256 * 1024];
            let mut digest = Sha256::new();
            let mut last_repaint = std::time::Instant::now();
            loop {
                let read = response.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                file.write_all(&buffer[..read])?;
                digest.update(&buffer[..read]);
                self.downloaded.fetch_add(read as u64, Ordering::Relaxed);
                if last_repaint.elapsed() > Duration::from_millis(100) {
                    ctx.request_repaint();
                    last_repaint = std::time::Instant::now();
                }
            }
            file.flush()?;
            let expected = self.total.load(Ordering::Relaxed);
            let actual = self.downloaded.load(Ordering::Relaxed);
            if expected != 0 && expected != actual {
                anyhow::bail!("Download ended early ({actual} of {expected} bytes)");
            }
            if let Some(expected_hash) = expected_sha256 {
                let actual_hash = format!("{:x}", digest.finalize());
                if !actual_hash.eq_ignore_ascii_case(expected_hash) {
                    anyhow::bail!(
                        "Downloaded model failed its integrity check (expected {expected_hash}, got {actual_hash})"
                    );
                }
            }
            Ok(())
        })();

        match result {
            Ok(()) => fs::rename(&partial, destination)
                .with_context(|| format!("Finalizing {}", destination.display())),
            Err(error) => {
                let _ = fs::remove_file(&partial);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_models_to_their_official_repositories() {
        assert!(model_url("ggml-base.en.bin").starts_with(MODEL_BASE_URL));
        assert_eq!(model_url(PARAKEET_FILE), PARAKEET_URL);
        assert_eq!(expected_sha256(PARAKEET_FILE), Some(PARAKEET_SHA256));
    }
}
