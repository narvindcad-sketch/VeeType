//! Local Whisper transcription and audio/video file processing.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::{HeapConsumer, HeapRb};
use whisper_rs::{FullParams, SamplingStrategy};

pub struct AudioCapture {
    stream: cpal::Stream,
    consumer: HeapConsumer<f32>,
    samples: Vec<f32>,
    silence_samples: usize,
    speech_detected: bool,
}

impl AudioCapture {
    pub fn start(
        device: &cpal::Device,
        stream_config: &cpal::StreamConfig,
        sample_format: cpal::SampleFormat,
        native_sample_rate: u32,
        channels: usize,
    ) -> anyhow::Result<Self> {
        let ring_buffer = HeapRb::<f32>::new(16_000 * 60);
        let (mut producer, consumer) = ring_buffer.split();

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let mut sample_index = 0;
                let ratio = native_sample_rate as f32 / 16_000.0;
                device.build_input_stream(
                    stream_config,
                    move |data: &[f32], _: &_| {
                        for frame in data.chunks(channels) {
                            let current_idx = (sample_index as f32 / ratio) as usize;
                            let next_idx = ((sample_index + 1) as f32 / ratio) as usize;
                            if current_idx != next_idx {
                                let mono = frame.iter().sum::<f32>() / channels as f32;
                                let _ = producer.push(mono);
                            }
                            sample_index += 1;
                        }
                    },
                    |error| tracing::error!(%error, "Audio input stream error"),
                    None,
                )?
            }
            _ => anyhow::bail!("Unsupported microphone sample format: {sample_format:?}"),
        };
        stream
            .play()
            .context("Could not start microphone capture")?;
        Ok(Self {
            stream,
            consumer,
            samples: Vec::new(),
            silence_samples: 0,
            speech_detected: false,
        })
    }

    pub fn drain_samples(&mut self, silence_threshold: f32) -> (f32, usize) {
        let mut energy_sum = 0.0_f32;
        let mut chunk_count = 0;
        while let Some(sample) = self.consumer.pop() {
            self.samples.push(sample);
            energy_sum += sample.abs();
            chunk_count += 1;
        }

        if chunk_count == 0 {
            return (0.0, 0);
        }

        let average_energy = energy_sum / chunk_count as f32;
        if average_energy >= silence_threshold {
            self.speech_detected = true;
            self.silence_samples = 0;
        } else if self.speech_detected {
            self.silence_samples += chunk_count;
        }
        (average_energy, chunk_count)
    }

    pub fn silence_limit_reached(&self, silence_limit: usize) -> bool {
        self.speech_detected && self.silence_samples >= silence_limit
    }

    pub fn finish(mut self) -> Vec<f32> {
        drop(self.stream);
        while let Some(sample) = self.consumer.pop() {
            self.samples.push(sample);
        }
        self.samples
    }
}

pub fn apply_vocabulary(text: &str, vocabulary: &HashMap<String, String>) -> String {
    let mut replacements: Vec<_> = vocabulary
        .iter()
        .filter(|(source, _)| !source.is_empty() && source.is_ascii())
        .collect();
    replacements.sort_by_key(|(source, _)| std::cmp::Reverse(source.len()));

    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;

    while cursor < text.len() {
        let matched = replacements.iter().find_map(|(source, replacement)| {
            let end = cursor.checked_add(source.len())?;
            let candidate = text.get(cursor..end)?;
            if !candidate.eq_ignore_ascii_case(source) {
                return None;
            }

            let source_starts_word = source.chars().next().is_some_and(char::is_alphanumeric);
            let source_ends_word = source
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let before_is_word = text[..cursor]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
            let after_is_word = text[end..]
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');

            (!(source_starts_word && before_is_word) && !(source_ends_word && after_is_word))
                .then_some((source.len(), replacement.as_str()))
        });

        if let Some((matched_len, replacement)) = matched {
            output.push_str(replacement);
            cursor += matched_len;
        } else {
            let character = text[cursor..].chars().next().expect("cursor is in bounds");
            output.push(character);
            cursor += character.len_utf8();
        }
    }

    output
}

pub fn vocabulary_prompt(vocabulary: &HashMap<String, String>) -> String {
    let mut entries: Vec<_> = vocabulary.iter().collect();
    entries.sort_by(|(left, _), (right, _)| left.to_lowercase().cmp(&right.to_lowercase()));
    let terms = entries
        .into_iter()
        .map(|(spoken, spelling)| format!("{spoken} ({spelling})"))
        .collect::<Vec<_>>()
        .join(", ");
    if terms.is_empty() {
        String::new()
    } else {
        format!("Names and specialized terms: {terms}.")
    }
}

pub fn transcribe_audio(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    language: &str,
    translate_to_english: bool,
    initial_prompt: &str,
) -> anyhow::Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let language = (!language.eq_ignore_ascii_case("auto")).then_some(language);
    params.set_language(language);
    params.set_translate(translate_to_english);
    params.set_initial_prompt(&initial_prompt.replace('\0', " "));
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|error| anyhow::anyhow!("Whisper transcription failed: {error}"))?;
    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&format!("{segment}"));
    }
    Ok(text)
}

pub fn transcribe_media_file(
    path: &Path,
    state: &mut whisper_rs::WhisperState,
    language: &str,
    translate_to_english: bool,
    initial_prompt: &str,
    vocabulary: &HashMap<String, String>,
) -> anyhow::Result<PathBuf> {
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-ac", "1", "-ar", "16000", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            anyhow::anyhow!("Could not run FFmpeg from PATH. Install FFmpeg and try again: {error}")
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "FFmpeg could not decode {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.len() % std::mem::size_of::<f32>() != 0 {
        anyhow::bail!(
            "FFmpeg returned incomplete audio samples for {}",
            path.display()
        );
    }
    let samples: Vec<f32> = output
        .stdout
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    if samples.len() < 8_000 {
        anyhow::bail!("The selected file does not contain enough audio to transcribe");
    }

    let text = transcribe_audio(
        state,
        &samples,
        language,
        translate_to_english,
        initial_prompt,
    )?;
    let text = apply_vocabulary(&text, vocabulary);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("transcript");
    let transcript_path = path.with_file_name(format!("{stem}_transcript_{timestamp}.txt"));
    fs::write(&transcript_path, text)?;
    Command::new("notepad.exe")
        .arg(&transcript_path)
        .spawn()
        .context("Transcript was saved, but Notepad could not be opened")?;
    Ok(transcript_path)
}
