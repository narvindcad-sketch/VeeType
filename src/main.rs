#![windows_subsystem = "windows"]

mod config;
mod engine;
mod ui;
mod utils;

use anyhow::Context;
use config::{load_config, PromptMode};
use cpal::traits::{DeviceTrait, HostTrait};
use device_query::DeviceState;
use enigo::{Enigo, KeyboardControllable};
use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tray_icon::menu::MenuEvent;
use whisper_rs::{WhisperContext, WhisperContextParameters};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetWindowTextW, MessageBoxW, PeekMessageW,
    TranslateMessage, MB_ICONERROR, MB_OK, MSG, PM_REMOVE, WM_QUIT,
};

use engine::audio::{
    apply_vocabulary, transcribe_audio, transcribe_media_file, vocabulary_prompt, AudioCapture,
};
use engine::cloud::CloudLlm;
use engine::llm::LocalLlm;
use ui::{Overlay, OverlayState};
use utils::hotkey::{is_hotkey_pressed, is_valid_hotkey};

fn application_directory() -> anyhow::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let executable_dir = executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("Cannot determine the application directory"))?;

    Ok(executable_dir
        .ancestors()
        .find(|directory| directory.join("Models").join("ggml-base.bin").is_file())
        .map(Path::to_path_buf)
        .unwrap_or(executable_dir))
}

fn configure_auto_start(enabled: bool) -> anyhow::Result<()> {
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE_NAME: &str = "VeeType";
    const LEGACY_VALUE_NAME: &str = "VoiceDictation";

    let key_path = wide(RUN_KEY);
    let value_name = wide(VALUE_NAME);
    let legacy_value_name = wide(LEGACY_VALUE_NAME);
    let command = if enabled {
        let executable = std::env::current_exe()?;
        Some(wide(&format!("\"{}\"", executable.display())))
    } else {
        None
    };
    let mut key = 0;
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        anyhow::bail!("Could not open the Windows startup registry key (error {status})");
    }

    let result = if enabled {
        let command = command.as_ref().expect("command exists when enabled");
        let byte_len = u32::try_from(command.len() * std::mem::size_of::<u16>())?;
        let status = unsafe {
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                byte_len,
            )
        };
        if status != 0 {
            Err(anyhow::anyhow!(
                "Could not register Windows auto-start (error {status})"
            ))
        } else {
            let legacy_status = unsafe { RegDeleteValueW(key, legacy_value_name.as_ptr()) };
            if legacy_status == 0 || legacy_status == 2 {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Could not remove the legacy Windows auto-start entry (error {legacy_status})"
                ))
            }
        }
    } else {
        let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
        if status == 0 || status == 2 {
            let legacy_status = unsafe { RegDeleteValueW(key, legacy_value_name.as_ptr()) };
            if legacy_status == 0 || legacy_status == 2 {
                Ok(())
            } else {
                Err(anyhow::anyhow!(
                    "Could not remove the legacy Windows auto-start entry (error {legacy_status})"
                ))
            }
        } else {
            Err(anyhow::anyhow!(
                "Could not remove the Windows auto-start entry (error {status})"
            ))
        }
    };

    unsafe {
        RegCloseKey(key);
    }
    result
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn pump_windows_messages() -> bool {
    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while PeekMessageW(&mut message, 0, 0, 0, PM_REMOVE) != 0 {
            if message.message == WM_QUIT {
                return true;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    false
}

fn get_active_window_title() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return String::new();
        }

        let mut buffer = [0u16; 512];
        let len = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        if len > 0 {
            OsString::from_wide(&buffer[..len as usize])
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        }
    }
}

fn show_error_dialog(title: &str, message: &str) {
    let title = wide(title);
    let message = wide(message);
    unsafe {
        MessageBoxW(0, message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

fn initialize_logging() -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("The LOCALAPPDATA environment variable is not set"))?;
    let log_dir = local_app_data.join("VeeType");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("Creating log directory {}", log_dir.display()))?;

    let appender = tracing_appender::rolling::daily(log_dir, "app.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("Initializing application logging: {error}"))?;
    Ok(guard)
}

fn main() {
    let _log_guard = match initialize_logging() {
        Ok(guard) => guard,
        Err(error) => {
            show_error_dialog("VeeType logging could not start", &format!("{error:#}"));
            return;
        }
    };

    std::panic::set_hook(Box::new(|panic_info| {
        tracing::error!(panic = %panic_info, "Application panicked");
        show_error_dialog("VeeType encountered an error", &format!("{panic_info}"));
    }));

    let result = if std::env::args().any(|argument| argument == "--vault") {
        application_directory()
            .map(|app_dir| ui::vault::run(app_dir.join("vault.json")))
            .and_then(|result| result)
    } else {
        run_app()
    };

    if let Err(error) = result {
        tracing::error!(error = %error, "Application could not continue");
        show_error_dialog("VeeType could not continue", &format!("{error:#}"));
    }
}

fn run_app() -> anyhow::Result<()> {
    let app_dir = application_directory()?;
    let config = load_config(&app_dir)?;
    if !is_valid_hotkey(config.settings.hotkey.trim()) {
        anyhow::bail!(
            "Unsupported settings.hotkey value: {:?}",
            config.settings.hotkey
        );
    }
    configure_auto_start(config.settings.auto_start)?;
    if config.settings.max_tokens == 0 {
        anyhow::bail!("settings.max_tokens must be greater than zero");
    }
    if !matches!(
        config.provider().to_ascii_lowercase().as_str(),
        "local" | "groq" | "openai"
    ) {
        anyhow::bail!(
            "Unsupported provider {:?}; expected \"local\", \"groq\", or \"openai\"",
            config.provider()
        );
    }
    if config.settings.groq_model.trim().is_empty() {
        anyhow::bail!("settings.groq_model must not be empty");
    }
    if config.settings.silence_timeout_ms == 0 {
        anyhow::bail!("settings.silence_timeout_ms must be greater than zero");
    }
    if config.settings.silence_timeout_ms > 60_000 {
        anyhow::bail!("settings.silence_timeout_ms must not exceed 60000");
    }
    let max_new_tokens = config.settings.max_tokens;
    tracing::info!(
        hotkey = %config.settings.hotkey,
        max_tokens = max_new_tokens,
        provider = %config.provider(),
        "Configuration loaded"
    );

    let cloud_llm = if config.provider().eq_ignore_ascii_case("groq")
        || config.provider().eq_ignore_ascii_case("openai")
    {
        Some(CloudLlm::from_config(&config)?)
    } else {
        None
    };

    let tray = ui::tray::Tray::new()?;
    let overlay = Overlay::spawn()?;

    tracing::info!("Dictation engine starting");

    let mut local_llm = if config.provider().eq_ignore_ascii_case("local") {
        Some(LocalLlm::load(&app_dir)?)
    } else {
        tracing::info!(provider = %config.provider(), "Using cloud text polishing; Whisper remains local");
        None
    };

    let ctx_params = WhisperContextParameters::default();
    let whisper_model_path = app_dir.join("Models/ggml-base.bin");
    let whisper_ctx = WhisperContext::new_with_params(&whisper_model_path, ctx_params)
        .expect("Failed to load Whisper.");
    let mut whisper_state = whisper_ctx.create_state().expect("Failed to create state");

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("No microphone detected.");
    let supported_config = device.default_input_config()?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let native_sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let device_state = DeviceState::new();
    let mut enigo = Enigo::new();

    let silence_threshold = 0.01_f32;
    let silence_limit = (16_000_u64 * config.settings.silence_timeout_ms / 1_000) as usize;
    let whisper_prompt = vocabulary_prompt(&config.vocabulary);
    let mut prompt_mode = PromptMode::Auto;

    loop {
        let mut quit_requested = false;
        loop {
            if pump_windows_messages() {
                quit_requested = true;
                break;
            }

            if let Ok(event) = MenuEvent::receiver().try_recv() {
                if tray.is_quit_event(&event) {
                    quit_requested = true;
                    break;
                }
                if tray.is_vault_event(&event) {
                    match std::env::current_exe()
                        .and_then(|executable| Command::new(executable).arg("--vault").spawn())
                    {
                        Ok(_) => {}
                        Err(error) => {
                            show_error_dialog("Could not open Dictation Vault", &format!("{error}"))
                        }
                    }
                    continue;
                }
                if tray.is_transcribe_event(&event) {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "Audio and video",
                            &[
                                "wav", "mp3", "m4a", "aac", "flac", "ogg", "opus", "mp4", "mov",
                                "mkv", "webm", "avi",
                            ],
                        )
                        .pick_file()
                    {
                        match transcribe_media_file(
                            &path,
                            &mut whisper_state,
                            &config.settings.language,
                            config.settings.translate_to_english,
                            &whisper_prompt,
                            &config.vocabulary,
                        ) {
                            Ok(transcript_path) => {
                                tracing::info!(path = %transcript_path.display(), "Media transcript saved");
                            }
                            Err(error) => show_error_dialog(
                                "File transcription failed",
                                &format!("{error:#}"),
                            ),
                        }
                    }
                    continue;
                }
                if let Some(mode) = tray.handle_prompt_event(&event) {
                    prompt_mode = mode;
                }
            }

            if is_hotkey_pressed(&config.settings.hotkey, &device_state) {
                overlay.show(OverlayState::Listening)?;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if quit_requested {
            break;
        }

        let mut capture = AudioCapture::start(
            &device,
            &stream_config,
            sample_format,
            native_sample_rate,
            channels,
        )?;
        let listening_started = Instant::now();
        tracing::info!("Audio capture started");

        if config.settings.hands_free {
            while is_hotkey_pressed(&config.settings.hotkey, &device_state) {
                if pump_windows_messages() {
                    quit_requested = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
        if quit_requested {
            drop(capture);
            overlay.hide()?;
            break;
        }

        loop {
            if pump_windows_messages() {
                quit_requested = true;
                break;
            }

            let pressed = is_hotkey_pressed(&config.settings.hotkey, &device_state);
            if config.settings.hands_free && pressed {
                while is_hotkey_pressed(&config.settings.hotkey, &device_state) {
                    if pump_windows_messages() {
                        quit_requested = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(30));
                }
                if quit_requested {
                    break;
                }
                overlay.hide()?;
                break;
            }
            if !config.settings.hands_free && !pressed {
                overlay.hide()?;
                break;
            }

            let (average_energy, chunk_count) = capture.drain_samples(silence_threshold);

            if chunk_count > 0 {
                overlay.set_volume((average_energy / 0.08).clamp(0.0, 1.0))?;
            }

            if config.settings.hands_free && capture.silence_limit_reached(silence_limit) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        overlay.hide()?;
        if quit_requested {
            drop(capture);
            break;
        }

        let audio_samples = capture.finish();

        tracing::info!(
            elapsed_ms = listening_started.elapsed().as_millis(),
            sample_count = audio_samples.len(),
            "Audio capture completed"
        );

        if audio_samples.len() < 8_000 {
            continue;
        }

        let transcription_started = Instant::now();
        let transcription_result = transcribe_audio(
            &mut whisper_state,
            &audio_samples,
            &config.settings.language,
            config.settings.translate_to_english,
            &whisper_prompt,
        );
        tracing::info!(
            elapsed_ms = transcription_started.elapsed().as_millis(),
            "Whisper transcription completed"
        );
        let raw_text = transcription_result?;
        let trimmed_raw = raw_text.trim();
        if !trimmed_raw.is_empty() {
            let window_title = if prompt_mode == PromptMode::Auto {
                get_active_window_title().to_lowercase()
            } else {
                String::new()
            };
            let system_instruction = prompt_mode.resolve(&config.prompts, &window_title);

            let normalized_raw = apply_vocabulary(trimmed_raw, &config.vocabulary);
            let polishing_started = Instant::now();
            let mut generated_text = if let Some(cloud_llm) = cloud_llm.as_ref() {
                let polished = cloud_llm.polish(
                    config.settings.max_tokens,
                    system_instruction,
                    &normalized_raw,
                )?;
                enigo.key_sequence(&polished);
                polished
            } else {
                let local_llm = local_llm
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("The local LLM is not loaded"))?;
                local_llm.polish(
                    &app_dir,
                    config.settings.max_tokens,
                    system_instruction,
                    &normalized_raw,
                    &mut enigo,
                )?
            };
            tracing::info!(
                provider = %config.provider(),
                elapsed_ms = polishing_started.elapsed().as_millis(),
                "Text polishing completed"
            );

            if generated_text.trim().is_empty() {
                enigo.key_sequence(trimmed_raw);
                generated_text = trimmed_raw.to_string();
            }

            if generated_text
                .chars()
                .last()
                .map_or(true, |ch| !ch.is_whitespace())
            {
                enigo.key_sequence(" ");
            }
            if let Err(error) =
                ui::vault::save_entry(&app_dir.join("vault.json"), trimmed_raw, &generated_text)
            {
                show_error_dialog(
                    "Dictation was not added to the Vault",
                    &format!("{error:#}"),
                );
            }
            tracing::info!("Polished dictation inserted and saved");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::{AppConfig, PromptMode, Prompts};
    use crate::engine::audio::{apply_vocabulary, vocabulary_prompt};
    use std::collections::HashMap;

    #[test]
    fn vocabulary_replacements_ignore_ascii_case_and_respect_word_boundaries() {
        let vocabulary = HashMap::from([
            ("api".to_string(), "API".to_string()),
            ("github".to_string(), "GitHub".to_string()),
        ]);

        assert_eq!(
            apply_vocabulary(
                "Use the Api with GitHub; rapid stays unchanged.",
                &vocabulary
            ),
            "Use the API with GitHub; rapid stays unchanged."
        );
    }

    #[test]
    fn vocabulary_replacements_prefer_longer_phrases() {
        let vocabulary = HashMap::from([
            ("machine".to_string(), "Machine".to_string()),
            (
                "machine learning".to_string(),
                "Machine Learning".to_string(),
            ),
        ]);

        assert_eq!(
            apply_vocabulary("machine learning", &vocabulary),
            "Machine Learning"
        );
    }

    #[test]
    fn whisper_vocabulary_prompt_includes_configured_spellings() {
        let vocabulary = HashMap::from([
            ("sketchup".to_string(), "SketchUp".to_string()),
            ("sonos".to_string(), "Sonos".to_string()),
        ]);
        let prompt = vocabulary_prompt(&vocabulary);

        assert!(prompt.contains("sketchup (SketchUp)"));
        assert!(prompt.contains("sonos (Sonos)"));
    }

    #[test]
    fn prompt_modes_select_explicit_or_contextual_prompts() {
        let prompts = Prompts {
            default: "default".into(),
            coding: "coding".into(),
            professional: "professional".into(),
        };

        assert_eq!(
            PromptMode::Auto.resolve(&prompts, "Visual Studio Code"),
            "coding"
        );
        assert_eq!(
            PromptMode::Auto.resolve(&prompts, "Outlook"),
            "professional"
        );
        assert_eq!(PromptMode::Auto.resolve(&prompts, "Browser"), "default");
        assert_eq!(PromptMode::Coding.resolve(&prompts, "Outlook"), "coding");
        assert_eq!(
            PromptMode::Professional.resolve(&prompts, "Visual Studio Code"),
            "professional"
        );
    }

    #[test]
    fn checked_in_config_parses_with_optional_settings() {
        let config: AppConfig = toml::from_str(include_str!("../config.example.toml"))
            .expect("example config should parse");

        assert_eq!(config.settings.hotkey, "RightAlt");
        assert_eq!(config.settings.max_tokens, 64);
        assert_eq!(config.settings.provider, "local");
        assert_eq!(config.settings.language, "auto");
        assert!(config.settings.translate_to_english);
        assert!(!config.settings.auto_start);
        assert!(!config.settings.hands_free);
        assert_eq!(
            config.vocabulary.get("github").map(String::as_str),
            Some("GitHub")
        );
    }

    #[test]
    fn configs_without_provider_and_translation_options_keep_local_defaults() {
        let config: AppConfig = toml::from_str(
            r#"
                [settings]
                hotkey = "RightAlt"
                max_tokens = 64

                [prompts]
                default = "default"
                coding = "coding"
                professional = "professional"
            "#,
        )
        .expect("legacy config should parse");

        assert_eq!(config.settings.provider, "local");
        assert_eq!(config.settings.language, "auto");
        assert!(config.settings.translate_to_english);
        assert!(config.vocabulary.is_empty());
    }

    #[test]
    fn api_provider_and_model_can_override_legacy_settings() {
        let config: AppConfig = toml::from_str(
            r#"
                [settings]
                hotkey = "RightAlt"
                max_tokens = 64
                provider = "local"

                [api]
                provider = "openai"
                model = "gpt-4o-mini"

                [prompts]
                default = "default"
                coding = "coding"
                professional = "professional"
            "#,
        )
        .expect("API config should parse");

        assert_eq!(config.provider(), "openai");
        assert_eq!(config.model(), "gpt-4o-mini");
    }
}
