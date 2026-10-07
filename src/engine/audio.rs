//! Local Whisper transcription and audio/video file processing.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::{HeapConsumer, HeapRb};
use nnnoiseless::DenoiseState;
use webrtc_vad::{SampleRate, Vad, VadMode};
use whisper_rs::{FullParams, SamplingStrategy};

const VAD_FRAME_SAMPLES: usize = 480;
const VAD_HANGOVER_FRAMES: usize = 15;
const VAD_PRE_ROLL_FRAMES: usize = 10;

pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    consumer: HeapConsumer<f32>,
    samples: Vec<f32>,
    vad: Vad,
    pending_frame: Vec<f32>,
    pre_roll: VecDeque<Vec<f32>>,
    voice_hangover_frames: usize,
    silence_samples: usize,
    speech_detected: bool,
    denoiser: Option<Box<DenoiseState<'static>>>,
}

/// RNNoise runs at 48 kHz on 480-sample frames; our 16 kHz frames are upsampled 3x,
/// denoised in 10 ms chunks, then averaged back down.
fn denoise_frame(denoiser: &mut DenoiseState<'static>, frame: &mut [f32]) {
    const SCALE: f32 = 32768.0;
    let mut input = [0.0_f32; DenoiseState::FRAME_SIZE];
    let mut output = [0.0_f32; DenoiseState::FRAME_SIZE];
    for chunk in frame.chunks_exact_mut(DenoiseState::FRAME_SIZE / 3) {
        for (i, slot) in input.iter_mut().enumerate() {
            let position = i as f32 / 3.0;
            let low = position as usize;
            let high = (low + 1).min(chunk.len() - 1);
            let fraction = position - low as f32;
            *slot = (chunk[low] * (1.0 - fraction) + chunk[high] * fraction) * SCALE;
        }
        denoiser.process_frame(&mut output, &input);
        for (i, sample) in chunk.iter_mut().enumerate() {
            let triple = &output[i * 3..i * 3 + 3];
            *sample = (triple.iter().sum::<f32>() / 3.0 / SCALE).clamp(-1.0, 1.0);
        }
    }
}

impl AudioCapture {
    pub fn start(
        device: &cpal::Device,
        stream_config: &cpal::StreamConfig,
        sample_format: cpal::SampleFormat,
        native_sample_rate: u32,
        channels: usize,
        noise_suppression: bool,
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
            stream: Some(stream),
            consumer,
            samples: Vec::new(),
            vad: Vad::new_with_rate_and_mode(SampleRate::Rate16kHz, VadMode::Aggressive),
            pending_frame: Vec::with_capacity(VAD_FRAME_SAMPLES),
            pre_roll: VecDeque::with_capacity(VAD_PRE_ROLL_FRAMES),
            voice_hangover_frames: 0,
            silence_samples: 0,
            speech_detected: false,
            denoiser: noise_suppression.then(DenoiseState::new),
        })
    }

    fn process_pending_samples(&mut self) -> anyhow::Result<(f32, usize)> {
        let mut voiced_energy = 0.0_f32;
        let mut voiced_frames = 0;
        let mut processed_frames = 0;

        while let Some(sample) = self.consumer.pop() {
            self.pending_frame.push(sample);
            if self.pending_frame.len() != VAD_FRAME_SAMPLES {
                continue;
            }

            let mut frame = std::mem::replace(
                &mut self.pending_frame,
                Vec::with_capacity(VAD_FRAME_SAMPLES),
            );
            if let Some(denoiser) = self.denoiser.as_mut() {
                denoise_frame(denoiser, &mut frame);
            }
            let pcm_frame: Vec<i16> = frame
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect();
            let is_voice = self
                .vad
                .is_voice_segment(&pcm_frame)
                .map_err(|()| anyhow::anyhow!("WebRTC VAD rejected a 30 ms audio frame"))?;
            let frame_energy = (frame.iter().map(|sample| sample * sample).sum::<f32>()
                / VAD_FRAME_SAMPLES as f32)
                .sqrt();
            processed_frames += 1;

            if is_voice {
                self.speech_detected = true;
                self.voice_hangover_frames = VAD_HANGOVER_FRAMES;
                self.silence_samples = 0;
                self.samples.extend(self.pre_roll.drain(..).flatten());
                self.samples.extend_from_slice(&frame);
                voiced_energy += frame_energy;
                voiced_frames += 1;
            } else if self.voice_hangover_frames > 0 {
                self.voice_hangover_frames -= 1;
                self.silence_samples += VAD_FRAME_SAMPLES;
                self.samples.extend_from_slice(&frame);
                voiced_energy += frame_energy;
                voiced_frames += 1;
            } else {
                if self.speech_detected {
                    self.silence_samples += VAD_FRAME_SAMPLES;
                }
                if self.pre_roll.len() == VAD_PRE_ROLL_FRAMES {
                    self.pre_roll.pop_front();
                }
                self.pre_roll.push_back(frame);
            }
        }

        let average_energy = if voiced_frames == 0 {
            0.0
        } else {
            voiced_energy / voiced_frames as f32
        };
        Ok((average_energy, processed_frames))
    }

    pub fn drain_samples(&mut self) -> anyhow::Result<(f32, usize)> {
        self.process_pending_samples()
    }

    pub fn silence_limit_reached(&self, silence_limit: usize) -> bool {
        self.speech_detected && self.silence_samples >= silence_limit
    }

    pub fn finish(mut self) -> anyhow::Result<Vec<f32>> {
        drop(self.stream.take());
        self.process_pending_samples()?;
        Ok(self.samples)
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
    params.set_n_threads(crate::engine::worker_thread_count() as i32);
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

#[cfg(test)]
mod tests {
    #[test]
    fn denoiser_preserves_frame_length_and_range() {
        let mut denoiser = nnnoiseless::DenoiseState::new();
        let mut frame: Vec<f32> = (0..480).map(|i| ((i as f32) * 0.1).sin() * 0.3).collect();
        super::denoise_frame(&mut denoiser, &mut frame);
        assert_eq!(frame.len(), 480);
        assert!(frame.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}
