#![windows_subsystem = "windows"]

mod config;
mod engine;
mod ui;
mod utils;

use anyhow::Context;
use config::{load_config, PromptMode};
use cpal::traits::{DeviceTrait, HostTrait};
use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tray_icon::menu::MenuEvent;
use whisper_rs::{WhisperContext, WhisperContextParameters};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, EVENT_MODIFY_STATE, INFINITE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetWindowTextW, MessageBoxW,
    MsgWaitForMultipleObjectsEx, PeekMessageW, TranslateMessage, MB_ICONERROR, MB_OK, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_HOTKEY, WM_QUIT,
};

use engine::audio::{
    apply_vocabulary, transcribe_audio, transcribe_media_file, vocabulary_prompt, AudioCapture,
};
use engine::cloud::CloudLlm;
use engine::license::{Entitlements, LicenseManager};
use engine::llm::{apply_voice_commands, LocalLlm};
use engine::{effective_provider, hands_free_enabled};
use ui::{Overlay, OverlayState};
use utils::hotkey::{is_hotkey_pressed, is_valid_hotkey, windows_hotkey_parts, HOTKEY_ID};

static HOTKEY_TRIGGERED: AtomicBool = AtomicBool::new(false);

struct SingleInstanceGuard {
    mutex: HANDLE,
    wake_event: HANDLE,
}

impl SingleInstanceGuard {
    fn acquire() -> anyhow::Result<Option<Self>> {
        const MUTEX_NAME: &str = "Global\\VeeTypeAppMutex";
        const EVENT_NAME: &str = "Global\\VeeTypeAppWake";

        let event_name = wide(EVENT_NAME);
        let wake_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
        if wake_event == 0 {
            anyhow::bail!(
                "Could not create the VeeType wake event (Windows error {})",
                unsafe { GetLastError() }
            );
        }

        let mutex_name = wide(MUTEX_NAME);
        unsafe {
            SetLastError(0);
        }
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
        if mutex == 0 {
            let error = unsafe { GetLastError() };
            unsafe {
                CloseHandle(wake_event);
            }
            anyhow::bail!(
                "Could not create the VeeType single-instance mutex (Windows error {error})"
            );
        }

        if unsafe { GetLastError() } == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(mutex);
            }
            let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr()) };
            if event == 0 {
                let error = unsafe { GetLastError() };
                unsafe {
                    CloseHandle(wake_event);
                }
                anyhow::bail!(
                    "VeeType is already running, but its wake event could not be opened (Windows error {error})"
                );
            }
            let signaled = unsafe { SetEvent(event) } != 0;
            let error = unsafe { GetLastError() };
            unsafe {
                CloseHandle(event);
                CloseHandle(wake_event);
            }
            if !signaled {
                anyhow::bail!(
                    "Could not notify the running VeeType instance (Windows error {error})"
                );
            }
            return Ok(None);
        }

        Ok(Some(Self { mutex, wake_event }))
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.wake_event);
            CloseHandle(self.mutex);
        }
    }
}

struct RegisteredHotkey;

impl RegisteredHotkey {
    fn register(hotkey: &str) -> anyhow::Result<Self> {
        let (modifiers, key) = windows_hotkey_parts(hotkey)?;
        if unsafe { RegisterHotKey(0, HOTKEY_ID, modifiers, key) } == 0 {
            anyhow::bail!(
                "Could not register the global hotkey {hotkey:?} (Windows error {})",
                unsafe { GetLastError() }
            );
        }
        Ok(Self)
    }
}

impl Drop for RegisteredHotkey {
    fn drop(&mut self) {
        if unsafe { UnregisterHotKey(0, HOTKEY_ID) } == 0 {
            tracing::warn!(
                error = unsafe { GetLastError() },
                "Could not unregister the global hotkey"
            );
        }
    }
}

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
            if message.message == WM_HOTKEY && message.wParam == HOTKEY_ID as usize {
                HOTKEY_TRIGGERED.store(true, Ordering::Release);
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    false
}

fn take_hotkey_trigger() -> bool {
    HOTKEY_TRIGGERED.swap(false, Ordering::AcqRel)
}

fn wait_for_windows_activity(wake_event: HANDLE) -> anyhow::Result<bool> {
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(1, &wake_event, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    if result == WAIT_OBJECT_0 {
        return Ok(true);
    }
    if result == WAIT_OBJECT_0 + 1 {
        return Ok(false);
    }
    if result == WAIT_FAILED {
        anyhow::bail!(
            "Waiting for Windows messages failed (Windows error {})",
            unsafe { GetLastError() }
        );
    }
    anyhow::bail!("Windows returned an unexpected message-wait result: {result}");
}

fn open_settings_window() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    Command::new(executable).arg("--settings").spawn()?;
    Ok(())
}

fn send_unicode_text(text: &str) -> anyhow::Result<()> {
    const MAX_UNITS_PER_BATCH: usize = 5_000;

    for utf16_batch in text
        .encode_utf16()
        .collect::<Vec<_>>()
        .chunks(MAX_UNITS_PER_BATCH)
    {
        let mut inputs = Vec::with_capacity(utf16_batch.len() * 2);
        for &unit in utf16_batch {
            for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }

        let expected = inputs.len() as u32;
        let sent = unsafe {
            SendInput(
                expected,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent != expected {
            anyhow::bail!(
                "Windows accepted {sent} of {expected} Unicode keyboard events (error {})",
                unsafe { GetLastError() }
            );
        }
    }
    Ok(())
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

fn get_active_application_exe() -> Option<String> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return None;
        }
        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        path.rsplit('\\').next().map(str::to_lowercase)
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

    let appender = tracing_appender::rolling::daily(log_dir, "veetype.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .map_err(|error| anyhow::anyhow!("Initializing application logging: {error}"))?;
    Ok(guard)
}

fn main() {
    let is_auxiliary_window =
        std::env::args().any(|argument| argument == "--settings" || argument == "--vault");
    let single_instance = if is_auxiliary_window {
        None
    } else {
        match SingleInstanceGuard::acquire() {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => return,
            Err(error) => {
                show_error_dialog(
                    "VeeType single-instance check failed",
                    &format!("{error:#}"),
                );
                return;
            }
        }
    };

    let priority_set =
        unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) != 0 };

    let _log_guard = match initialize_logging() {
        Ok(guard) => guard,
        Err(error) => {
            show_error_dialog("VeeType logging could not start", &format!("{error:#}"));
            return;
        }
    };

    if priority_set {
        tracing::info!("Process priority set to below normal");
    } else {
        tracing::warn!(
            error = %std::io::Error::last_os_error(),
            "Could not lower process priority; continuing with default priority"
        );
    }

    std::panic::set_hook(Box::new(|panic_info| {
        tracing::error!(panic = %panic_info, "Application panicked");
        show_error_dialog("VeeType encountered an error", &format!("{panic_info}"));
    }));

    let result = if std::env::args().any(|argument| argument == "--vault") {
        application_directory()
            .map(|app_dir| ui::vault::run(app_dir.join("vault.json")))
            .and_then(|result| result)
    } else if std::env::args().any(|argument| argument == "--settings") {
        application_directory().and_then(ui::settings::run)
    } else {
        match single_instance.as_ref() {
            Some(guard) => run_app(guard.wake_event),
            None => Err(anyhow::anyhow!(
                "The main application instance guard was not initialized"
            )),
        }
    };

    if let Err(error) = result {
        tracing::error!(error = %error, "Application could not continue");
        show_error_dialog("VeeType could not continue", &format!("{error:#}"));
    }
}

fn run_app(wake_event: HANDLE) -> anyhow::Result<()> {
    let app_dir = application_directory()?;
    let config = load_config(&app_dir)?;
    let entitlements = match LicenseManager::cached_entitlements() {
        Ok(Some(entitlements)) => entitlements,
        Ok(None) => Entitlements::fallback(),
        Err(error) => {
            tracing::warn!(error = %error, "Cached Pro license is unavailable; using free features");
            Entitlements::fallback()
        }
    };
    if !is_valid_hotkey(config.settings.hotkey.trim()) {
        anyhow::bail!(
            "Unsupported settings.hotkey value: {:?}",
            config.settings.hotkey
        );
    }
    engine::startup::set_enabled(config.settings.auto_start)?;
    let _registered_hotkey = RegisteredHotkey::register(&config.settings.hotkey)?;
    if config.settings.max_tokens == 0 {
        anyhow::bail!("settings.max_tokens must be greater than zero");
    }
    if !matches!(
        config.provider().to_ascii_lowercase().as_str(),
        "local" | "groq" | "openai" | "anthropic"
    ) {
        anyhow::bail!(
            "Unsupported provider {:?}; expected \"local\", \"groq\", \"openai\", or \"anthropic\"",
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
    let selected_provider = config.provider();
    let effective_provider = effective_provider(selected_provider, &entitlements);
    if effective_provider != selected_provider {
        tracing::warn!(
            provider = selected_provider,
            "Cloud provider requires an active Pro license; using local polishing"
        );
    }
    let hands_free_enabled = hands_free_enabled(config.settings.hands_free, &entitlements);
    if config.settings.hands_free && !hands_free_enabled {
        tracing::warn!("Hands-free mode requires an active Pro license; using push-to-talk");
    }
    tracing::info!(
        hotkey = %config.settings.hotkey,
        max_tokens = max_new_tokens,
        provider = effective_provider,
        "Configuration loaded"
    );

    let cloud_llm = if effective_provider.eq_ignore_ascii_case("groq")
        || effective_provider.eq_ignore_ascii_case("openai")
        || effective_provider.eq_ignore_ascii_case("anthropic")
    {
        Some(CloudLlm::from_config(&config)?)
    } else {
        None
    };

    if !config.settings.has_completed_onboarding {
        if let Err(error) = open_settings_window() {
            tracing::warn!(error = %error, "Could not open the first-run setup wizard");
        }
    }

    let tray = ui::tray::Tray::new()?;
    let overlay = Overlay::spawn()?;

    tracing::info!("Dictation engine starting");

    let mut local_llm = if effective_provider.eq_ignore_ascii_case("local") {
        match LocalLlm::load(&app_dir, entitlements.large_models) {
            Ok(llm) => Some(llm),
            Err(error) => {
                tracing::warn!(error = %error, "Local polishing model unavailable; typing raw transcripts");
                None
            }
        }
    } else {
        tracing::info!(
            provider = effective_provider,
            "Using cloud text polishing; Whisper remains local"
        );
        None
    };

    let configured_model = app_dir.join("Models").join(config.settings.whisper_model.trim());
    let whisper_model_path = if configured_model.is_file() {
        configured_model
    } else {
        tracing::warn!(
            model = %configured_model.display(),
            "Configured Whisper model is missing; falling back to ggml-base.bin"
        );
        app_dir.join("Models/ggml-base.bin")
    };
    // The installer ships no models; wait for the setup wizard to download one.
    if !whisper_model_path.is_file() {
        tracing::warn!(
            model = %whisper_model_path.display(),
            "No Whisper model installed yet; waiting for a download from VeeType Settings"
        );
        while !whisper_model_path.is_file() {
            if pump_windows_messages() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        // Let the finished download settle before loading it.
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let ctx_params = WhisperContextParameters::default();
    let whisper_ctx = match WhisperContext::new_with_params(&whisper_model_path, ctx_params) {
        Ok(context) => {
            if cfg!(feature = "vulkan") {
                tracing::info!("Whisper model loaded with Vulkan support enabled");
            }
            context
        }
        Err(gpu_error) if cfg!(feature = "vulkan") => {
            tracing::warn!(
                error = %gpu_error,
                "Whisper GPU initialization failed; retrying with CPU inference"
            );
            let mut cpu_params = WhisperContextParameters::default();
            cpu_params.use_gpu(false);
            WhisperContext::new_with_params(&whisper_model_path, cpu_params).map_err(
                |cpu_error| {
                    anyhow::anyhow!(
                        "Failed to load Whisper model {} with GPU ({gpu_error}) and CPU fallback ({cpu_error})",
                        whisper_model_path.display()
                    )
                },
            )?
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to load Whisper model {}: {error}",
                whisper_model_path.display()
            ));
        }
    };
    let mut whisper_state = whisper_ctx
        .create_state()
        .context("Failed to initialize Whisper transcription state")?;

    let host = cpal::default_host();
    let device = match config
        .settings
        .input_device
        .as_deref()
        .filter(|name| !name.trim().is_empty())
    {
        Some(selected_name) => host
            .input_devices()?
            .find(|device| device.name().is_ok_and(|name| name == selected_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Configured microphone device {:?} is not available; select another device in VeeType Settings",
                    selected_name
                )
            })?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No microphone input device is available"))?,
    };
    let supported_config = device.default_input_config()?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let native_sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;

    let silence_limit = (16_000_u64 * config.settings.silence_timeout_ms / 1_000) as usize;
    let whisper_prompt = vocabulary_prompt(&config.vocabulary);
    let mut prompt_mode = PromptMode::Auto;
    engine::launcher::start_indexing();

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
                if tray.is_settings_event(&event) {
                    match std::env::current_exe()
                        .and_then(|executable| Command::new(executable).arg("--settings").spawn())
                    {
                        Ok(_) => {}
                        Err(error) => show_error_dialog(
                            "Could not open VeeType Settings",
                            &format!("{error}"),
                        ),
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

            if take_hotkey_trigger() {
                overlay.show(OverlayState::Listening)?;
                break;
            }
            if wait_for_windows_activity(wake_event)? {
                if let Err(error) = open_settings_window() {
                    tracing::error!(error = %error, "Could not open Settings for the running VeeType instance");
                    show_error_dialog("Could not open VeeType Settings", &format!("{error:#}"));
                }
            }
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
            config.settings.noise_suppression,
        )?;
        let active_exe = get_active_application_exe();
        let listening_started = Instant::now();
        tracing::info!("Audio capture started");

        if hands_free_enabled {
            while is_hotkey_pressed(&config.settings.hotkey) {
                if pump_windows_messages() {
                    quit_requested = true;
                    break;
                }
                let _ = take_hotkey_trigger();
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
            let _ = take_hotkey_trigger();

            let pressed = is_hotkey_pressed(&config.settings.hotkey);
            if hands_free_enabled && pressed {
                while is_hotkey_pressed(&config.settings.hotkey) {
                    if pump_windows_messages() {
                        quit_requested = true;
                        break;
                    }
                    let _ = take_hotkey_trigger();
                    std::thread::sleep(Duration::from_millis(30));
                }
                if quit_requested {
                    break;
                }
                overlay.hide()?;
                break;
            }
            if !hands_free_enabled && !pressed {
                overlay.hide()?;
                break;
            }

            let (average_energy, frame_count) = capture.drain_samples()?;

            if frame_count > 0 {
                overlay.set_volume((average_energy / 0.08).clamp(0.0, 1.0))?;
            }

            if hands_free_enabled && capture.silence_limit_reached(silence_limit) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        overlay.hide()?;
        if quit_requested {
            drop(capture);
            break;
        }

        let audio_samples = capture.finish()?;

        tracing::info!(
            elapsed_ms = listening_started.elapsed().as_millis(),
            sample_count = audio_samples.len(),
            "Audio capture completed"
        );

        if audio_samples.len() < 8_000 {
            continue;
        }

        overlay.show(OverlayState::Processing)?;
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
        if !trimmed_raw.is_empty() && engine::launcher::evaluate_and_launch(trimmed_raw, &config.voice_commands) {
            tracing::info!("Voice launch command handled; skipping text insertion");
        } else if !trimmed_raw.is_empty() {
            let profile = active_exe
                .as_deref()
                .and_then(|exe| config.app_profiles.get(exe));
            let effective_mode = profile
                .and_then(|profile| profile.mode.as_deref())
                .and_then(PromptMode::from_name)
                .filter(|mode| *mode != PromptMode::Auto)
                .unwrap_or(prompt_mode);
            let window_title = if effective_mode == PromptMode::Auto {
                get_active_window_title().to_lowercase()
            } else {
                String::new()
            };
            let system_instruction = effective_mode.resolve(&config.prompts, &window_title);

            let normalized_raw = apply_vocabulary(trimmed_raw, &config.vocabulary);
            let polishing_started = Instant::now();
            let mut generated_text = if profile.is_some_and(|profile| !profile.polish) {
                normalized_raw.clone()
            } else if let Some(cloud_llm) = cloud_llm.as_ref() {
                cloud_llm
                    .polish(
                        config.settings.max_tokens,
                        system_instruction,
                        &normalized_raw,
                    )
                    .unwrap_or_else(|error| {
                        tracing::warn!(error = %error, "Cloud polishing failed; typing the raw transcript");
                        normalized_raw.clone()
                    })
            } else if let Some(local_llm) = local_llm.as_mut() {
                local_llm
                    .polish(
                        &app_dir,
                        config.settings.max_tokens,
                        system_instruction,
                        &normalized_raw,
                    )
                    .unwrap_or_else(|error| {
                        tracing::warn!(error = %error, "Local polishing failed; typing the raw transcript");
                        normalized_raw.clone()
                    })
            } else {
                normalized_raw.clone()
            };
            tracing::info!(
                provider = effective_provider,
                elapsed_ms = polishing_started.elapsed().as_millis(),
                "Text polishing completed"
            );

            if generated_text.trim().is_empty() {
                generated_text = trimmed_raw.to_string();
            }
            generated_text = apply_voice_commands(&generated_text);

            if !generated_text.is_empty() {
                let mut insertion_text = generated_text.clone();
                if insertion_text
                    .chars()
                    .last()
                    .map_or(true, |ch| !ch.is_whitespace())
                {
                    insertion_text.push(' ');
                }
                send_unicode_text(&insertion_text)?;
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
        overlay.hide()?;
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
