//! Crash-isolated, persistent transcribe.cpp worker for Parakeet GGUF models.

use anyhow::Context;
use std::io::{BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;
use transcribe_cpp::{Backend, Model, ModelOptions, RunOptions, SessionOptions, Task};

const WORKER_ARGUMENT: &str = "--parakeet-worker";
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUDIO_SAMPLES: usize = 16_000 * 60 * 30;

#[derive(Debug)]
enum WorkerResponse {
    Ready(String),
    Transcript(String),
    Error(String),
    Disconnected(String),
}

/// A warm child process owns transcribe.cpp. Native access violations, illegal
/// instructions, and Vulkan driver failures therefore cannot terminate VeeType.
pub struct ParakeetWorker {
    child: Child,
    requests: SyncSender<Vec<f32>>,
    responses: Receiver<WorkerResponse>,
    architecture: String,
}

impl ParakeetWorker {
    pub fn spawn(model_path: &Path, use_gpu: bool, threads: usize) -> anyhow::Result<Self> {
        let executable = std::env::current_exe().context("Locating the VeeType executable")?;
        let mut child = Command::new(executable)
            .arg(WORKER_ARGUMENT)
            .arg(model_path)
            .arg(if use_gpu { "vulkan" } else { "cpu" })
            .arg(threads.clamp(1, 16).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()
            .context("Starting the isolated Parakeet engine")?;
        let mut stdin = child
            .stdin
            .take()
            .context("Opening Parakeet worker input")?;
        let stdout = child
            .stdout
            .take()
            .context("Opening Parakeet worker output")?;
        let (sender, responses) = mpsc::channel();
        let write_errors = sender.clone();
        let (requests, request_receiver) = mpsc::sync_channel::<Vec<f32>>(1);
        let writer = std::thread::Builder::new()
            .name("parakeet-request-writer".into())
            .spawn(move || {
                while let Ok(samples) = request_receiver.recv() {
                    if let Err(error) = write_audio_request(&mut stdin, &samples) {
                        let _ =
                            write_errors.send(WorkerResponse::Disconnected(format!("{error:#}")));
                        break;
                    }
                }
            });
        if let Err(error) = writer {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Starting Parakeet request writer");
        }
        let reader = std::thread::Builder::new()
            .name("parakeet-response-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let response = match read_response(&mut reader) {
                        Ok(response) => response,
                        Err(error) => {
                            let _ = sender.send(WorkerResponse::Disconnected(format!("{error:#}")));
                            return;
                        }
                    };
                    if sender.send(response).is_err() {
                        return;
                    }
                }
            });
        if let Err(error) = reader {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Starting Parakeet response monitor");
        }

        let mut worker = Self {
            child,
            requests,
            responses,
            architecture: String::new(),
        };
        match worker.responses.recv_timeout(READY_TIMEOUT) {
            Ok(WorkerResponse::Ready(architecture)) => worker.architecture = architecture,
            Ok(WorkerResponse::Error(error) | WorkerResponse::Disconnected(error)) => {
                worker.stop();
                anyhow::bail!("Parakeet worker could not load the model: {error}");
            }
            Ok(WorkerResponse::Transcript(_)) => {
                worker.stop();
                anyhow::bail!("Parakeet worker returned an invalid startup response");
            }
            Err(RecvTimeoutError::Timeout) => {
                worker.stop();
                anyhow::bail!("Parakeet model loading timed out after 180 seconds");
            }
            Err(RecvTimeoutError::Disconnected) => {
                worker.stop();
                anyhow::bail!("Parakeet worker stopped while loading the model");
            }
        }
        Ok(worker)
    }

    pub fn architecture(&self) -> &str {
        &self.architecture
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> anyhow::Result<String> {
        if samples.len() > MAX_AUDIO_SAMPLES {
            anyhow::bail!("Dictation is longer than the 30-minute safety limit");
        }
        // Pipe writes may block if a native worker hangs before reading input.
        // Send on a separate thread so the deadline covers transport as well
        // as inference. Killing the child releases any blocked pipe write.
        self.requests
            .try_send(samples.to_vec())
            .context("Submitting audio to Parakeet")?;

        let audio_seconds = samples.len() as u64 / 16_000;
        let timeout = Duration::from_secs((audio_seconds.saturating_mul(3) + 60).clamp(90, 600));
        match self.responses.recv_timeout(timeout) {
            Ok(WorkerResponse::Transcript(text)) => Ok(text),
            Ok(WorkerResponse::Error(error) | WorkerResponse::Disconnected(error)) => {
                anyhow::bail!("Parakeet worker failed: {error}")
            }
            Ok(WorkerResponse::Ready(_)) => {
                anyhow::bail!("Parakeet worker returned an invalid response")
            }
            Err(RecvTimeoutError::Timeout) => {
                self.stop();
                anyhow::bail!("Parakeet transcription timed out")
            }
            Err(RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Parakeet worker stopped unexpectedly")
            }
        }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ParakeetWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs worker mode before the desktop single-instance guard is acquired.
pub fn maybe_run_from_arguments() -> Option<i32> {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    if arguments.next()?.to_string_lossy() != WORKER_ARGUMENT {
        return None;
    }
    let model_path = match arguments.next() {
        Some(path) => path,
        None => return Some(2),
    };
    let backend = match arguments.next().as_deref().and_then(|value| value.to_str()) {
        Some("vulkan") => Backend::Vulkan,
        Some("cpu") => Backend::Cpu,
        _ => return Some(2),
    };
    let threads = arguments
        .next()
        .and_then(|value| value.to_string_lossy().parse::<i32>().ok())
        .unwrap_or(2)
        .clamp(1, 16);
    Some(match run_worker(Path::new(&model_path), backend, threads) {
        Ok(()) => 0,
        Err(_) => 1,
    })
}

fn run_worker(model_path: &Path, backend: Backend, threads: i32) -> anyhow::Result<()> {
    unsafe {
        windows_sys::Win32::System::Threading::SetPriorityClass(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            windows_sys::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS,
        );
    }
    let mut output = std::io::stdout().lock();
    let result = (|| -> anyhow::Result<(Model, transcribe_cpp::Session)> {
        let model = Model::load_with(
            model_path,
            &ModelOptions {
                backend,
                device: None,
            },
        )
        .with_context(|| format!("Loading Parakeet model {}", model_path.display()))?;
        let session = model
            .session_with(&SessionOptions {
                n_threads: threads,
                ..Default::default()
            })
            .context("Creating Parakeet session")?;
        Ok((model, session))
    })();
    let (model, mut session) = match result {
        Ok(value) => value,
        Err(error) => {
            write_response(&mut output, 2, &format!("{error:#}"))?;
            return Err(error);
        }
    };
    write_response(&mut output, 0, &model.arch())?;

    let mut input = std::io::stdin().lock();
    loop {
        let mut count_bytes = [0_u8; 4];
        if let Err(error) = input.read_exact(&mut count_bytes) {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                return Ok(());
            }
            return Err(error.into());
        }
        let count = u32::from_le_bytes(count_bytes) as usize;
        if count > MAX_AUDIO_SAMPLES {
            write_response(&mut output, 2, "Audio request exceeds the safety limit")?;
            anyhow::bail!("Audio request exceeds the safety limit");
        }
        let byte_count = count
            .checked_mul(std::mem::size_of::<f32>())
            .context("Audio request size overflow")?;
        let mut audio_bytes = vec![0_u8; byte_count];
        input.read_exact(&mut audio_bytes)?;
        let samples: Vec<f32> = audio_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        let options = RunOptions {
            task: Task::Transcribe,
            language: Some("en".to_string()),
            ..Default::default()
        };
        match session.run(&samples, &options) {
            Ok(result) => write_response(&mut output, 1, &result.text)?,
            Err(error) => write_response(&mut output, 2, &error.to_string())?,
        }
    }
}

fn write_audio_request(output: &mut impl Write, samples: &[f32]) -> anyhow::Result<()> {
    anyhow::ensure!(
        samples.len() <= MAX_AUDIO_SAMPLES,
        "Audio request exceeds the safety limit"
    );
    anyhow::ensure!(
        samples.iter().all(|sample| sample.is_finite()),
        "Audio contains non-finite samples"
    );
    output.write_all(&(samples.len() as u32).to_le_bytes())?;
    // Bounded conversion storage avoids a second recording-sized allocation.
    let mut bytes = Vec::with_capacity(4096 * 4);
    for chunk in samples.chunks(4096) {
        bytes.clear();
        for sample in chunk {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        output.write_all(&bytes)?;
    }
    output.flush()?;
    Ok(())
}

fn write_response(output: &mut impl Write, kind: u8, message: &str) -> anyhow::Result<()> {
    let bytes = message.as_bytes();
    let length = u32::try_from(bytes.len()).context("Worker response is too large")?;
    output.write_all(&[kind])?;
    output.write_all(&length.to_le_bytes())?;
    output.write_all(bytes)?;
    output.flush()?;
    Ok(())
}

fn read_response(input: &mut impl Read) -> anyhow::Result<WorkerResponse> {
    let mut kind = [0_u8; 1];
    input.read_exact(&mut kind)?;
    let mut length = [0_u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_RESPONSE_BYTES {
        anyhow::bail!("Parakeet worker response exceeds the safety limit");
    }
    let mut bytes = vec![0_u8; length];
    input.read_exact(&mut bytes)?;
    let message = String::from_utf8(bytes).context("Parakeet worker returned invalid UTF-8")?;
    Ok(match kind[0] {
        0 => WorkerResponse::Ready(message),
        1 => WorkerResponse::Transcript(message),
        2 => WorkerResponse::Error(message),
        other => WorkerResponse::Disconnected(format!("Unknown response type {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_protocol_preserves_samples_across_conversion_chunks() {
        let samples: Vec<f32> = (0..9001).map(|i| (i as f32 * 0.001).sin()).collect();
        let mut bytes = Vec::new();
        write_audio_request(&mut bytes, &samples).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 9001);
        let restored: Vec<f32> = bytes[4..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        assert_eq!(restored, samples);
    }

    #[test]
    fn audio_protocol_rejects_non_finite_samples_before_writing() {
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut bytes = Vec::new();
            assert!(write_audio_request(&mut bytes, &[sample]).is_err());
            assert!(bytes.is_empty());
        }
    }

    #[test]
    fn response_protocol_round_trips_unicode() {
        let mut bytes = Vec::new();
        write_response(&mut bytes, 1, "hello 世界").unwrap();
        match read_response(&mut bytes.as_slice()).unwrap() {
            WorkerResponse::Transcript(text) => assert_eq!(text, "hello 世界"),
            response => panic!("unexpected response: {response:?}"),
        }
    }

    #[test]
    fn response_protocol_rejects_oversized_messages() {
        let mut bytes = vec![1];
        bytes.extend_from_slice(&((MAX_RESPONSE_BYTES as u32) + 1).to_le_bytes());
        assert!(read_response(&mut bytes.as_slice()).is_err());
    }
}
