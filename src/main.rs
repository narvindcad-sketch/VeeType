#![windows_subsystem = "windows"]

mod config;
mod engine;
mod ui;
mod utils;
mod text_input;

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
    CloseHandle, GetLastError, SetLastError, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey,
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
use engine::adaptive::{AdaptiveBackend, ComputeBackend};
use engine::license::Entitlements;
use engine::llm::{apply_voice_commands, LocalLlm};
use engine::{effective_provider, hands_free_enabled};
use ui::{Overlay, OverlayState};
use utils::hotkey::{is_hotkey_pressed, is_valid_hotkey, windows_hotkey_parts, HOTKEY_ID};

static HOTKEY_TRIGGERED: AtomicBool = AtomicBool::new(false);

struct SingleInstanceGuard {
    mutex: HANDLE,
    wake_event: HANDLE,
    config_event: HANDLE,
}

impl SingleInstanceGuard {
    fn acquire() -> anyhow::Result<Option<Self>> {
        const MUTEX_NAME: &str = "Global\\VeeTypeAppMutex";
        const EVENT_NAME: &str = "Global\\VeeTypeAppWake";
        const CONFIG_EVENT_NAME: &str = "Global\\VeeTypeConfigChanged";

        let event_name = wide(EVENT_NAME);
        let wake_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
        if wake_event == 0 {
            anyhow::bail!(
                "Could not create the VeeType wake event (Windows error {})",
                unsafe { GetLastError() }
            );
        }
        let config_event_name = wide(CONFIG_EVENT_NAME);
        let config_event = unsafe { CreateEventW(std::ptr::null(), 0, 0, config_event_name.as_ptr()) };
        if config_event == 0 {
            let error = unsafe { GetLastError() };
            unsafe { CloseHandle(wake_event) };
            anyhow::bail!("Could not create the VeeType configuration event (Windows error {error})");
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
                CloseHandle(config_event);
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
                    CloseHandle(config_event);
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
                CloseHandle(config_event);
            }
            if !signaled {
                anyhow::bail!(
                    "Could not notify the running VeeType instance (Windows error {error})"
                );
            }
            return Ok(None);
        }

        Ok(Some(Self { mutex, wake_event, config_event }))
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.wake_event);
            CloseHandle(self.config_event);
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

fn installed_whisper_model(app_dir: &Path, preferred: &str) -> Option<PathBuf> {
    let models_dir = app_dir.join("Models");
    let selected = models_dir.join(preferred.trim());
    if selected.is_file() {
        return Some(selected);
    }
    let fallback = models_dir.join("ggml-base.bin");
    fallback.is_file().then_some(fallback)
}

fn input_device_config(
    host: &cpal::Host,
    selected_name: Option<&str>,
) -> anyhow::Result<(cpal::Device, cpal::StreamConfig, cpal::SampleFormat, u32, usize)> {
    let device = match selected_name.filter(|name| !name.trim().is_empty()) {
        Some(selected_name) => match host
            .input_devices()?
            .find(|device| device.name().is_ok_and(|name| name == selected_name))
        {
            Some(device) => device,
            None => {
                tracing::warn!(
                    configured_device = selected_name,
                    "Configured microphone is unavailable; using the Windows default input device"
                );
                host.default_input_device().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Configured microphone {:?} is unavailable and Windows has no default microphone",
                        selected_name
                    )
                })?
            }
        },
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No microphone input device is available"))?,
    };
    let supported_config = device
        .default_input_config()
        .context("Could not read microphone format")?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let native_sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;
    Ok((device, stream_config, sample_format, native_sample_rate, channels))
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum WindowsActivity {
    OpenSettings,
    SettingsChanged,
    Message,
    Timeout,
}

fn wait_for_windows_activity(
    wake_event: HANDLE,
    config_event: HANDLE,
    timeout_ms: u32,
) -> anyhow::Result<WindowsActivity> {
    let handles = [wake_event, config_event];
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(2, handles.as_ptr(), timeout_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    if result == WAIT_OBJECT_0 {
        return Ok(WindowsActivity::OpenSettings);
    }
    if result == WAIT_OBJECT_0 + 1 {
        return Ok(WindowsActivity::SettingsChanged);
    }
    if result == WAIT_TIMEOUT {
        return Ok(WindowsActivity::Timeout);
    }
    if result == WAIT_OBJECT_0 + 2 {
        return Ok(WindowsActivity::Message);
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

enum SpeechEngine {
    Whisper {
        _context: WhisperContext,
        state: whisper_rs::WhisperState,
    },
    Universal {
        _model: transcribe_cpp::Model,
        session: transcribe_cpp::Session,
        architecture: String,
    },
}

struct LoadedWhisper {
    engine: SpeechEngine,
    backend: ComputeBackend,
}

fn load_whisper_engine(
    whisper_model_path: &Path,
    requested_backend: ComputeBackend,
) -> anyhow::Result<LoadedWhisper> {
    if whisper_model_path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("gguf")) {
        let requested = match requested_backend {
            ComputeBackend::Cpu => transcribe_cpp::Backend::Cpu,
            ComputeBackend::Gpu => transcribe_cpp::Backend::Vulkan,
        };
        let options = transcribe_cpp::ModelOptions { backend: requested, device: None };
        let model = match transcribe_cpp::Model::load_with(whisper_model_path, &options) {
            Ok(model) => model,
            Err(gpu_error) if requested_backend == ComputeBackend::Gpu => {
                tracing::warn!(error = %gpu_error, "Universal speech-model GPU initialization failed; retrying on CPU");
                return load_whisper_engine(whisper_model_path, ComputeBackend::Cpu);
            }
            Err(error) => return Err(error).with_context(|| format!("Loading universal speech model {}", whisper_model_path.display())),
        };
        let architecture = model.arch();
        let session = model.session_with(&transcribe_cpp::SessionOptions {
            n_threads: engine::worker_thread_count() as i32,
            ..Default::default()
        }).context("Creating universal speech-model session")?;
        tracing::info!(backend = ?requested_backend, architecture, "Universal speech model loaded");
        return Ok(LoadedWhisper {
            engine: SpeechEngine::Universal { _model: model, session, architecture },
            backend: requested_backend,
        });
    }
    let mut ctx_params = WhisperContextParameters::default();
    ctx_params.use_gpu(requested_backend == ComputeBackend::Gpu);
    let mut actual_backend = requested_backend;
    let whisper_ctx = match WhisperContext::new_with_params(whisper_model_path, ctx_params) {
        Ok(context) => {
            context
        }
        Err(gpu_error) if requested_backend == ComputeBackend::Gpu => {
            tracing::warn!(
                error = %gpu_error,
                "Whisper GPU initialization failed; retrying with CPU inference"
            );
            let mut cpu_params = WhisperContextParameters::default();
            cpu_params.use_gpu(false);
            actual_backend = ComputeBackend::Cpu;
            WhisperContext::new_with_params(whisper_model_path, cpu_params).map_err(
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
    let whisper_state = match whisper_ctx.create_state() {
        Ok(state) => state,
        Err(error) if actual_backend == ComputeBackend::Gpu => {
            tracing::warn!(%error, "Whisper GPU state allocation failed; retrying on CPU");
            drop(whisper_ctx);
            return load_whisper_engine(whisper_model_path, ComputeBackend::Cpu);
        }
        Err(error) => return Err(error).context("Failed to initialize Whisper transcription state"),
    };
    tracing::info!(backend = ?actual_backend, "Whisper model loaded");
    Ok(LoadedWhisper {
        engine: SpeechEngine::Whisper { _context: whisper_ctx, state: whisper_state },
        backend: actual_backend,
    })
}

fn transcribe_with_engine(
    loaded: &mut LoadedWhisper,
    samples: &[f32],
    language: &str,
    translate_to_english: bool,
    prompt: &str,
    n_threads: usize,
) -> anyhow::Result<String> {
    match &mut loaded.engine {
        SpeechEngine::Whisper { state, .. } => transcribe_audio(
            state, samples, language, translate_to_english, prompt, n_threads,
        ),
        SpeechEngine::Universal { session, architecture, .. } => {
            // The currently shipped universal model is Parakeet. It is an
            // English transcription model and has no translation head. The
            // global translation preference belongs to Whisper, so forwarding
            // it here makes the runtime reject dictation as an unsupported
            // task (the preference defaults to `true` in older installations).
            let options = transcribe_cpp::RunOptions {
                task: universal_transcription_task(translate_to_english),
                language: (!language.eq_ignore_ascii_case("auto")).then(|| language.to_string()),
                ..Default::default()
            };
            session.run(samples, &options)
                .map(|result| result.text)
                .map_err(|error| anyhow::anyhow!("{architecture} transcription failed: {error}"))
        }
    }
}

fn universal_transcription_task(_translate_to_english: bool) -> transcribe_cpp::Task {
    // Universal models advertise their capabilities at runtime. Until a
    // translation-capable universal model is added, transcription is the one
    // supported and reliable operation for this engine family.
    transcribe_cpp::Task::Transcribe
}

fn ensure_whisper_engine(
    engine: &mut Option<LoadedWhisper>, path: &Path, scheduler: &mut AdaptiveBackend,
) -> anyhow::Result<()> {
    let resident_gpu = engine.as_ref().is_some_and(|engine| engine.backend == ComputeBackend::Gpu);
    let requested = scheduler.select(fs::metadata(path)?.len(), resident_gpu);
    if engine.as_ref().is_some_and(|engine| engine.backend == requested) { return Ok(()); }
    // Release the previous backend before allocating its replacement.
    *engine = None;
    let loaded = load_whisper_engine(path, requested)?;
    if requested == ComputeBackend::Gpu && loaded.backend == ComputeBackend::Cpu {
        scheduler.gpu_failed();
    } else { scheduler.activate(loaded.backend); }
    *engine = Some(loaded);
    Ok(())
}

fn send_unicode_text(text: &str) -> anyhow::Result<()> {
    text_input::send_text(text)
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
    let is_background = std::env::args().any(|argument| argument == "--background");
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
            Some(guard) => run_app(guard.wake_event, guard.config_event, !is_background),
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

fn run_app(wake_event: HANDLE, config_event: HANDLE, show_control_center: bool) -> anyhow::Result<()> {
    let app_dir = application_directory()?;
    let mut config = load_config(&app_dir)?;
    let whisper_model_available =
        installed_whisper_model(&app_dir, &config.settings.whisper_model).is_some();
    if !config.settings.has_completed_onboarding {
        open_settings_window().context("Opening the first-run setup wizard")?;
    } else if show_control_center && whisper_model_available {
        // A user launch behaves like a regular desktop app. Windows auto-start
        // passes --background, keeping the tray service unobtrusive.
        open_settings_window().context("Opening the VeeType control window")?;
    }
    // Migrate existing installations to the quiet sign-in command. A policy
    // can deny this optional registry write, but that must never stop
    // dictation from starting.
    if config.settings.auto_start {
        if let Err(error) = engine::startup::set_enabled(true) {
            tracing::warn!(error = %error, "Could not refresh the Windows auto-start command");
        }
    }
    // VeeType is a single, fully included edition. Cloud providers still use
    // the account owner's API key, but no VeeType license is required.
    let entitlements = Entitlements::unlocked();
    if !is_valid_hotkey(config.settings.hotkey.trim()) {
        anyhow::bail!(
            "Unsupported settings.hotkey value: {:?}",
            config.settings.hotkey
        );
    }
    // Auto-start is changed when settings are saved.  Do not touch the Run
    // registry key on every launch: managed Windows installations can deny
    // registry writes, which previously prevented VeeType from starting even
    // when auto-start was disabled.
    let mut registered_hotkey = RegisteredHotkey::register(&config.settings.hotkey)?;
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
    let selected_provider = config.provider();
    let mut effective_provider = effective_provider(selected_provider, &entitlements).to_string();
    let mut hands_free_enabled = hands_free_enabled(config.settings.hands_free, &entitlements);
    tracing::info!(
        hotkey = %config.settings.hotkey,
        max_tokens = config.settings.max_tokens,
        provider = effective_provider,
        language = %config.settings.language,
        translate_to_english = config.settings.translate_to_english,
        "Configuration loaded"
    );

    let mut cloud_llm = if effective_provider.eq_ignore_ascii_case("groq")
        || effective_provider.eq_ignore_ascii_case("openai")
        || effective_provider.eq_ignore_ascii_case("anthropic")
    {
        Some(CloudLlm::from_config(&config)?)
    } else {
        None
    };

    let tray = ui::tray::Tray::new()?;
    let overlay = Overlay::spawn()?;

    tracing::info!("Dictation engine starting");

    if !effective_provider.eq_ignore_ascii_case("local") {
        tracing::info!(
            provider = effective_provider,
            "Using cloud text polishing; Whisper remains local"
        );
    }
    // Both on-device models are loaded only after speech is captured. This
    // leaves the always-on tray process with a tiny idle memory footprint.
    let mut local_llm: Option<LocalLlm> = None;
    let mut local_llm_unavailable = false;

    // The installer ships no models. Keep Settings accessible while the user downloads one.
    let mut whisper_model_path = if let Some(path) =
        installed_whisper_model(&app_dir, &config.settings.whisper_model)
    {
        path
    } else {
        tracing::warn!(
            "No Whisper model installed yet; waiting for a download from VeeType Settings"
        );
        if config.settings.has_completed_onboarding {
            open_settings_window().context("Opening Settings to install a Whisper model")?;
        }
        let path = loop {
            // The wizard can select a different model; read its saved choice each time.
            if app_dir.join("config.toml").is_file() {
                if let Ok(current_config) = load_config(&app_dir) {
                    if let Some(path) =
                        installed_whisper_model(&app_dir, &current_config.settings.whisper_model)
                    {
                        break path;
                    }
                }
            }
            if pump_windows_messages() {
                return Ok(());
            }
            // Relaunching the app and the tray menu must still open Settings while waiting.
            if unsafe { WaitForSingleObject(wake_event, 0) } == WAIT_OBJECT_0 {
                if let Err(error) = open_settings_window() {
                    tracing::warn!(error = %error, "Could not open Settings");
                }
            }
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                if tray.is_quit_event(&event) {
                    return Ok(());
                }
                if tray.is_settings_event(&event) {
                    if let Err(error) = open_settings_window() {
                        show_error_dialog("Could not open VeeType Settings", &format!("{error}"));
                    }
                } else if tray.is_vault_event(&event) {
                    if let Err(error) = std::env::current_exe()
                        .and_then(|exe| Command::new(exe).arg("--vault").spawn())
                    {
                        show_error_dialog("Could not open Dictation Vault", &format!("{error}"));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        // Let the finished download settle before loading it.
        std::thread::sleep(std::time::Duration::from_secs(1));
        path
    };
    let mut whisper_engine: Option<LoadedWhisper> = None;
    let mut whisper_scheduler = AdaptiveBackend::new();
    let mut last_model_use: Option<Instant> = None;

    let host = cpal::default_host();
    let mut silence_limit = (16_000_u64 * config.settings.silence_timeout_ms / 1_000) as usize;
    let mut whisper_prompt = vocabulary_prompt(&config.vocabulary);
    let mut config_modified = fs::metadata(app_dir.join("config.toml"))
        .and_then(|metadata| metadata.modified())
        .ok();
    let mut prompt_mode = PromptMode::Auto;

    loop {
        let mut quit_requested = false;
        loop {
            if last_model_use.is_some_and(|used_at| {
                used_at.elapsed() >= Duration::from_secs(900)
            }) {
                // Keep the model warm across a normal dictation session, then
                // return the large model allocations to the OS and GPU.
                whisper_engine = None;
                local_llm = None;
                last_model_use = None;
                tracing::info!("Released inactive dictation models");
            }
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
                        ensure_whisper_engine(&mut whisper_engine, &whisper_model_path, &mut whisper_scheduler)?;
                        last_model_use = Some(Instant::now());
                        let result = match &mut whisper_engine
                            .as_mut()
                            .expect("Speech engine was just initialized").engine
                        {
                            SpeechEngine::Whisper { state, .. } => transcribe_media_file(
                                &path, state, &config.settings.language,
                                config.settings.translate_to_english, &whisper_prompt,
                                &config.vocabulary, whisper_scheduler.thread_count(),
                            ),
                            SpeechEngine::Universal { .. } => Err(anyhow::anyhow!(
                                "Media-file transcription currently requires a Whisper model"
                            )),
                        };
                        match result {
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
                break;
            }
            match wait_for_windows_activity(wake_event, config_event, 30_000)? {
                WindowsActivity::SettingsChanged => {
                let updated_modified = fs::metadata(app_dir.join("config.toml"))
                    .and_then(|metadata| metadata.modified())
                    .ok();
                if updated_modified != config_modified {
                    config_modified = updated_modified;
                    match load_config(&app_dir) {
                        Ok(updated_config) if is_valid_hotkey(updated_config.settings.hotkey.trim()) => {
                            if updated_config.settings.hotkey != config.settings.hotkey {
                                drop(registered_hotkey);
                                registered_hotkey = match RegisteredHotkey::register(&updated_config.settings.hotkey) {
                                    Ok(hotkey) => hotkey,
                                    Err(error) => {
                                        show_error_dialog("Could not apply VeeType hotkey", &format!("{error:#}"));
                                        RegisteredHotkey::register(&config.settings.hotkey)?
                                    }
                                };
                            }
                            let new_model = installed_whisper_model(
                                &app_dir,
                                &updated_config.settings.whisper_model,
                            );
                            if new_model.as_ref().is_some_and(|path| path != &whisper_model_path) {
                                whisper_model_path = new_model.expect("model path was checked");
                                whisper_engine = None;
                                last_model_use = None;
                            }
                            hands_free_enabled = engine::hands_free_enabled(
                                updated_config.settings.hands_free,
                                &entitlements,
                            );
                            silence_limit = (16_000_u64
                                * updated_config.settings.silence_timeout_ms.clamp(1, 60_000)
                                / 1_000) as usize;
                            whisper_prompt = vocabulary_prompt(&updated_config.vocabulary);
                            effective_provider = engine::effective_provider(
                                updated_config.provider(),
                                &entitlements,
                            )
                            .to_string();
                            cloud_llm = if effective_provider.eq_ignore_ascii_case("local") {
                                None
                            } else {
                                match CloudLlm::from_config(&updated_config) {
                                    Ok(client) => Some(client),
                                    Err(error) => {
                                        tracing::warn!(error = %error, "Updated cloud provider could not be initialized");
                                        None
                                    }
                                }
                            };
                            local_llm = None;
                            local_llm_unavailable = false;
                            config = updated_config;
                            tracing::info!("Applied saved VeeType settings without restart");
                        }
                        Ok(_) => show_error_dialog(
                            "Could not apply VeeType settings",
                            "The selected hotkey is not supported.",
                        ),
                        Err(error) => show_error_dialog(
                            "Could not reload VeeType settings",
                            &format!("{error:#}"),
                        ),
                    }
                }
                }
                WindowsActivity::OpenSettings => {
                    if let Err(error) = open_settings_window() {
                    tracing::error!(error = %error, "Could not open Settings for the running VeeType instance");
                    show_error_dialog("Could not open VeeType Settings", &format!("{error:#}"));
                    }
                }
                WindowsActivity::Message | WindowsActivity::Timeout => {}
            }
        }
        if quit_requested {
            break;
        }

        let (device, stream_config, sample_format, native_sample_rate, channels) =
            match input_device_config(&host, config.settings.input_device.as_deref()) {
                Ok(config) => config,
                Err(error) => {
                    tracing::error!(error = %error, "No usable microphone is available");
                    show_error_dialog(
                        "VeeType could not find a microphone",
                        &format!("{error:#}\n\nConnect or enable a microphone, then try again. VeeType will remain running."),
                    );
                    continue;
                }
            };
        let mut capture = match AudioCapture::start(
            &device,
            &stream_config,
            sample_format,
            native_sample_rate,
            channels,
            config.settings.noise_suppression,
        ) {
            Ok(capture) => capture,
            Err(error) => {
                tracing::error!(error = %error, "Could not open microphone input stream");
                show_error_dialog(
                    "VeeType could not start the microphone",
                    &format!("{error:#}\n\nCheck Windows microphone privacy permissions and the selected input device. VeeType will remain running."),
                );
                continue;
            }
        };
        let active_exe = get_active_application_exe();
        let listening_started = Instant::now();
        tracing::info!("Audio capture started");
        // Show readiness only after the microphone stream has started.
        overlay.show(OverlayState::Listening)?;
        engine::launcher::start_indexing();

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
        if quit_requested {
            drop(capture);
            overlay.hide()?;
            break;
        }

        // Drain a small trailing window so releasing the key doesn't cut the
        // last syllable or an audio-driver buffer still in flight.
        let tail_deadline = Instant::now() + Duration::from_millis(200);
        while Instant::now() < tail_deadline {
            if pump_windows_messages() {
                quit_requested = true;
                break;
            }
            capture.drain_samples()?;
            std::thread::sleep(Duration::from_millis(10));
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
        if let Err(error) =
            ensure_whisper_engine(&mut whisper_engine, &whisper_model_path, &mut whisper_scheduler)
        {
            tracing::error!(error = %error, "Speech engine could not be initialized");
            overlay.hide()?;
            show_error_dialog(
                "VeeType could not start speech recognition",
                &format!("{error:#}\n\nOpen Settings to select or download a compatible speech model."),
            );
            continue;
        }
        last_model_use = Some(Instant::now());
        let mut actual_backend = whisper_engine.as_ref().expect("Whisper engine loaded").backend;
        let mut transcription_started = Instant::now();
        let mut transcription_result = transcribe_with_engine(
            whisper_engine.as_mut().expect("Speech engine loaded"),
            &audio_samples, &config.settings.language,
            config.settings.translate_to_english, &whisper_prompt,
            whisper_scheduler.thread_count(),
        );
        if transcription_result.is_err() && actual_backend == ComputeBackend::Gpu {
            tracing::warn!(error = ?transcription_result.as_ref().err(), "GPU transcription failed; retrying retained audio on CPU");
            whisper_engine = None;
            whisper_scheduler.gpu_failed();
            ensure_whisper_engine(&mut whisper_engine, &whisper_model_path, &mut whisper_scheduler)?;
            actual_backend = ComputeBackend::Cpu;
            transcription_started = Instant::now();
            transcription_result = transcribe_with_engine(
                whisper_engine.as_mut().expect("CPU fallback loaded"),
                &audio_samples, &config.settings.language,
                config.settings.translate_to_english, &whisper_prompt,
                whisper_scheduler.thread_count(),
            );
        }
        if transcription_result.is_ok() {
            whisper_scheduler.record_performance(actual_backend, audio_samples.len() as f64 / 16_000.0, transcription_started.elapsed());
        }
        tracing::info!(
            elapsed_ms = transcription_started.elapsed().as_millis(),
            backend = ?actual_backend,
            "Whisper transcription completed"
        );
        let raw_text = match transcription_result {
            Ok(text) => text,
            Err(error) => {
                tracing::error!(error = %error, "Speech recognition failed; VeeType will remain available");
                overlay.hide()?;
                show_error_dialog(
                    "VeeType could not recognize that dictation",
                    &format!("{error:#}\n\nVeeType is still running. Try again, or check the microphone and model in Settings."),
                );
                continue;
            }
        };
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

            let filler_cleaned = if config.settings.language.eq_ignore_ascii_case("en") {
                engine::text_cleanup::clean_english_transcript(trimmed_raw)
            } else {
                trimmed_raw.to_string()
            };
            let normalized_raw = apply_vocabulary(&filler_cleaned, &config.vocabulary);
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
            } else {
                if local_llm.is_none() && !local_llm_unavailable {
                    match LocalLlm::load(&app_dir, entitlements.large_models) {
                        Ok(llm) => local_llm = Some(llm),
                        Err(error) => {
                            tracing::warn!(error = %error, "Local polishing model unavailable; typing raw transcripts");
                            local_llm_unavailable = true;
                        }
                    }
                }
                local_llm
                    .as_mut()
                    .map(|local_llm| {
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
                    })
                    .unwrap_or_else(|| normalized_raw.clone())
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
    use super::{installed_whisper_model, universal_transcription_task};
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

    #[test]
    fn detects_a_newly_selected_whisper_model_without_the_base_model() {
        let app_dir = std::env::temp_dir().join(format!(
            "veetype-model-test-{}",
            std::process::id()
        ));
        let models_dir = app_dir.join("Models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let selected = models_dir.join("ggml-small.en.bin");
        assert!(installed_whisper_model(&app_dir, "ggml-small.en.bin").is_none());
        std::fs::write(&selected, []).unwrap();
        assert_eq!(
            installed_whisper_model(&app_dir, "ggml-small.en.bin"),
            Some(selected)
        );
        std::fs::remove_dir_all(app_dir).unwrap();
    }

    #[test]
    fn universal_models_transcribe_when_legacy_translation_is_enabled() {
        assert_eq!(
            universal_transcription_task(true),
            transcribe_cpp::Task::Transcribe
        );
    }
}
