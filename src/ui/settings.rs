use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;

use crate::config::{load_config, save_config, AppConfig, AppProfile};
use crate::engine::downloader::Download;
use crate::engine::{Entitlements, KeyVault, LicenseManager, OtaUpdater};
use crate::utils::hotkey::{is_hotkey_pressed, is_valid_hotkey, pressed_hotkey};

pub fn run(app_dir: PathBuf) -> anyhow::Result<()> {
    let config = load_config(&app_dir)?;
    let logo = image::load_from_memory(include_bytes!("../../icon.ico"))
        .context("Could not decode the VeeType application icon")?
        .into_rgba8();
    let (logo_width, logo_height) = logo.dimensions();
    let logo_pixels = logo.into_raw();
    let viewport = egui::ViewportBuilder::default()
        .with_title("VeeType | Settings")
        .with_icon(egui::IconData {
            rgba: logo_pixels.clone(),
            width: logo_width,
            height: logo_height,
        });
    let viewport = if config.settings.has_completed_onboarding {
        viewport
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([820.0, 560.0])
    } else {
        viewport.with_inner_size([500.0, 700.0]).with_resizable(false)
    };
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "VeeType | Settings",
        options,
        Box::new(move |creation_context| {
            configure_visuals(&creation_context.egui_ctx);
            let logo = creation_context.egui_ctx.load_texture(
                "veetype-app-icon",
                egui::ColorImage::from_rgba_unmultiplied(
                    [logo_width as usize, logo_height as usize],
                    &logo_pixels,
                ),
                egui::TextureOptions::LINEAR,
            );
            Ok(Box::new(SettingsApp::new(app_dir, config, logo)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("VeeType Settings window failed: {error}"))
}

struct SettingsApp {
    app_dir: PathBuf,
    models_dir: PathBuf,
    logo: egui::TextureHandle,
    config: AppConfig,
    provider: String,
    model: String,
    input_devices: Vec<String>,
    capture_hotkey: bool,
    groq_key: String,
    openai_key: String,
    remove_groq_key: bool,
    remove_openai_key: bool,
    anthropic_key: String,
    remove_anthropic_key: bool,
    status: Option<(bool, String)>,
    update_status: String,
    update_rx: Receiver<String>,
    update_tx: Sender<String>,
    update_checking: bool,
    license_entitlements: Entitlements,
    license_signed_in: bool,
    license_email: String,
    license_password: String,
    license_status: String,
    license_rx: Receiver<String>,
    license_tx: Sender<String>,
    license_busy: bool,
    alias_name: String,
    alias_path: String,
    advanced_open: bool,
    tab: Tab,
    onboarding: bool,
    wizard_step: u8,
    mic_status: Option<(bool, String)>,
    hotkey_verified: bool,
    profile_exe: String,
    model_search: String,
    mic_peak: f32,
    feature_idx: usize,
    feature_t: f64,
    resize_pending: bool,
    wizard_scratch: String,
    vocab_from: String,
    vocab_to: String,
    downloads: std::collections::HashMap<&'static str, std::sync::Arc<Download>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Home,
    Modes,
    Vocabulary,
    Shortcuts,
    Library,
    Sound,
    Config,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Modes => "Default Mode",
            Tab::Vocabulary => "Vocabulary",
            Tab::Shortcuts => "Shortcuts",
            Tab::Library => "Models library",
            Tab::Sound => "Sound",
            Tab::Config => "Configuration",
        }
    }

    fn chip_color(self) -> egui::Color32 {
        match self {
            Tab::Home => egui::Color32::from_rgb(234, 138, 74),
            Tab::Modes => egui::Color32::from_rgb(134, 112, 217),
            Tab::Vocabulary => egui::Color32::from_rgb(86, 140, 232),
            Tab::Shortcuts => egui::Color32::from_rgb(222, 108, 150),
            Tab::Config => egui::Color32::from_rgb(120, 113, 140),
            Tab::Sound => egui::Color32::from_rgb(52, 170, 140),
            Tab::Library => egui::Color32::from_rgb(182, 122, 214),
        }
    }
}

impl SettingsApp {
    fn new(app_dir: PathBuf, config: AppConfig, logo: egui::TextureHandle) -> Self {
        let (update_tx, update_rx) = mpsc::channel();
        let (license_tx, license_rx) = mpsc::channel();
        let license_signed_in = match LicenseManager::has_session() {
            Ok(signed_in) => signed_in,
            Err(error) => {
                tracing::warn!(error = %error, "Could not read saved Supabase session");
                false
            }
        };
        let (license_entitlements, license_status) = match LicenseManager::cached_entitlements() {
            Ok(Some(entitlements)) => (
                entitlements,
                LicenseManager::account_status()
                    .unwrap_or_else(|error| format!("License status error: {error:#}")),
            ),
            Ok(None) => (
                Entitlements::fallback(),
                "No active Pro license is stored on this device.".into(),
            ),
            Err(error) => {
                tracing::warn!(error = %error, "Could not verify cached Pro license");
                (
                    Entitlements::fallback(),
                    format!("Cached Pro license is not valid: {error:#}"),
                )
            }
        };
        let models_dir = app_dir.join("Models");
        let input_devices = match list_input_devices() {
            Ok(devices) => devices,
            Err(error) => {
                tracing::warn!(error = %error, "Could not enumerate microphone devices");
                Vec::new()
            }
        };
        let provider = config.provider().to_ascii_lowercase();
        let model = config.api.model.clone().unwrap_or_default();

        let onboarding = !config.settings.has_completed_onboarding;
        Self {
            app_dir,
            models_dir,
            logo,
            config,
            provider,
            model,
            input_devices,
            capture_hotkey: false,
            groq_key: String::new(),
            openai_key: String::new(),
            remove_groq_key: false,
            remove_openai_key: false,
            anthropic_key: String::new(),
            remove_anthropic_key: false,
            status: None,
            update_status: String::new(),
            update_rx,
            update_tx,
            update_checking: false,
            license_entitlements,
            license_signed_in,
            license_email: String::new(),
            license_password: String::new(),
            license_status,
            license_rx,
            license_tx,
            license_busy: false,
            alias_name: String::new(),
            alias_path: String::new(),
            advanced_open: false,
            tab: Tab::Home,
            model_search: String::new(),
            mic_peak: 0.0,
            feature_idx: 0,
            feature_t: 0.0,
            resize_pending: false,
            wizard_scratch: String::new(),
            vocab_from: String::new(),
            vocab_to: String::new(),
            onboarding,
            wizard_step: 0,
            mic_status: None,
            hotkey_verified: false,
            profile_exe: String::new(),
            downloads: Default::default(),
        }
    }

    fn start_license_action(&mut self, create_account: bool) {
        if self.license_email.trim().is_empty() || self.license_password.is_empty() {
            self.license_status = "Enter your account email and password first.".into();
            return;
        }

        let email = self.license_email.trim().to_string();
        let password = std::mem::take(&mut self.license_password);
        let sender = self.license_tx.clone();
        self.license_busy = true;
        self.license_status = if create_account {
            "Creating account...".into()
        } else {
            "Signing in and checking subscription...".into()
        };
        std::thread::spawn(move || {
            let result = LicenseManager::sign_in(&email, &password, create_account);
            let message =
                result.unwrap_or_else(|error| format!("Account/license error: {error:#}"));
            let _ = sender.send(message);
        });
    }

    fn refresh_license(&mut self) {
        let sender = self.license_tx.clone();
        self.license_busy = true;
        self.license_status = "Refreshing Pro license...".into();
        std::thread::spawn(move || {
            let result = LicenseManager::refresh_license();
            let message =
                result.unwrap_or_else(|error| format!("License refresh failed: {error:#}"));
            let _ = sender.send(message);
        });
    }

    fn open_checkout(&mut self) {
        let sender = self.license_tx.clone();
        self.license_busy = true;
        self.license_status = "Opening secure checkout...".into();
        std::thread::spawn(move || {
            let result = LicenseManager::open_checkout();
            let message =
                result.unwrap_or_else(|error| format!("Could not open checkout: {error:#}"));
            let _ = sender.send(message);
        });
    }

    fn save(&mut self) {
        if !is_valid_hotkey(self.config.settings.hotkey.trim()) {
            self.status = Some((false, "The selected hotkey is not supported.".into()));
            return;
        }

        let result = (|| -> anyhow::Result<()> {
            save_credential("GROQ_API_KEY", &self.groq_key, self.remove_groq_key)?;
            save_credential("OPENAI_API_KEY", &self.openai_key, self.remove_openai_key)?;
            save_credential(
                "ANTHROPIC_API_KEY",
                &self.anthropic_key,
                self.remove_anthropic_key,
            )?;

            self.config.api.provider = Some(self.provider.clone());
            self.config.api.model =
                (!self.model.trim().is_empty()).then(|| self.model.trim().to_string());
            crate::engine::startup::set_enabled(self.config.settings.auto_start)?;
            save_config(&self.app_dir, &self.config)?;
            Ok(())
        })();

        self.status = Some(match result {
            Ok(()) => (
                true,
                "Settings saved. Restart VeeType to apply the changes.".into(),
            ),
            Err(error) => (false, format!("Could not save settings: {error:#}")),
        });
    }

    fn import_model(&mut self) {
        let Some(source) = rfd::FileDialog::new()
            .add_filter("GGUF models", &["gguf", "bin"])
            .pick_file()
        else {
            return;
        };

        let result = copy_model(&source, &self.models_dir);
        self.status = Some(match result {
            Ok(destination) => (
                true,
                format!(
                    "Imported {}. Restart VeeType to use model changes.",
                    destination
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ),
            ),
            Err(error) => (false, format!("Could not import model: {error:#}")),
        });
    }

    fn render_library(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.downloads.retain(|_, download| {
            !(download.is_finished() && download.error().is_none())
        });
        let installed = model_files(&self.models_dir).unwrap_or_default();
        for (heading, recommended) in [("RECOMMENDED OFFLINE MODELS", true), ("OTHER MODELS", false)] {
            ui.label(egui::RichText::new(heading).small().color(INK).strong());
            for model in MODEL_CATALOG.iter().filter(|m| m.recommended == recommended) {
                let is_installed = installed.iter().any(|f| f == model.file);
                let download = self.downloads.get(model.file).cloned();
                if model_card(ui, model, is_installed, download.as_deref()) {
                    self.downloads.insert(
                        model.file,
                        Download::start(model.file, &self.models_dir, ctx.clone()),
                    );
                }
            }
        }
    }

    fn render_models(&mut self, ui: &mut egui::Ui) {
        section(ui, "Local models", |ui| {
            ui.label("Whisper and GGUF model files in the application Models folder:");

            match model_files(&self.models_dir) {
                Ok(files) if files.is_empty() => {
                    ui.colored_label(
                        WARN,
                        "No model files found.",
                    );
                }
                Ok(files) => {
                    for file in files {
                        ui.label(file);
                    }
                }
                Err(error) => {
                    ui.colored_label(
                        ERR,
                        format!("Could not list model files: {error:#}"),
                    );
                }
            }

            if ui.button("Import GGUF model...").clicked() {
                self.import_model();
            }
        });
    }

    fn render_voice_commands(&mut self, ui: &mut egui::Ui) {
        toggle_row(
            ui,
            "Voice commands",
            "Say a trigger word plus a name to open apps, folders or links.",
            &mut self.config.voice_commands.enabled,
            true,
        );
        if !self.config.voice_commands.enabled {
            return;
        }
        let commands = &mut self.config.voice_commands;
        let mut triggers = commands.triggers.join(", ");
        row_card(ui, "Trigger words", "The first word you say must be one of these.", |ui| {
            if ui
                .add(egui::TextEdit::singleline(&mut triggers).desired_width(260.0))
                .changed()
            {
                commands.triggers = triggers
                    .split(',')
                    .map(|word| word.trim().to_lowercase())
                    .filter(|word| !word.is_empty() && !word.contains(char::is_whitespace))
                    .collect();
            }
        });
        section(ui, "Shortcuts", |ui| {
            ui.label(
                egui::RichText::new(
                    "Say \"open my projects\" to open a folder, file, app or website you choose.",
                )
                .color(MUTED),
            );
            let mut remove = None;
            for (spoken, path) in &commands.aliases {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(spoken).strong().color(INK));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").clicked() {
                            remove = Some(spoken.clone());
                        }
                        ui.label(egui::RichText::new(path).color(MUTED));
                    });
                });
            }
            if let Some(spoken) = remove {
                commands.aliases.remove(&spoken);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.alias_name)
                    .hint_text("Spoken name, e.g. my projects")
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.alias_path)
                    .hint_text("Folder, file, app or https:// link")
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.button("Browse folder").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.alias_path = path.to_string_lossy().into_owned();
                    }
                }
                if ui.button("Browse file or app").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        self.alias_path = path.to_string_lossy().into_owned();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ready =
                        !self.alias_name.trim().is_empty() && !self.alias_path.trim().is_empty();
                    let add = egui::Button::new(egui::RichText::new("Add shortcut").color(ON_ACCENT))
                        .fill(ACCENT);
                    if ui.add_enabled(ready, add).clicked() {
                        commands.aliases.insert(
                            self.alias_name.trim().to_lowercase(),
                            self.alias_path.trim().to_string(),
                        );
                        self.alias_name.clear();
                        self.alias_path.clear();
                    }
                });
            });
        });
    }
    fn render_app_profiles(&mut self, ui: &mut egui::Ui) {
        section(ui, "Activate for apps", |ui| {
            ui.label(
                egui::RichText::new("Override the writing mode per application (matched by executable name).")
                    .color(MUTED),
            );
            let mut remove = None;
            for (exe, profile) in self.config.app_profiles.iter_mut() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(exe.as_str()).color(INK));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").clicked() {
                            remove = Some(exe.clone());
                        }
                        ui.checkbox(&mut profile.polish, "Polish");
                        let current = profile.mode.clone().unwrap_or_else(|| "default".into());
                        egui::ComboBox::from_id_salt(exe.as_str())
                            .selected_text(&current)
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for mode in ["default", "coding", "professional"] {
                                    ui.selectable_value(&mut profile.mode, Some(mode.to_string()), mode);
                                }
                            });
                    });
                });
            }
            if let Some(exe) = remove {
                self.config.app_profiles.remove(&exe);
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Add app profile").clicked() {
                        let exe = self.profile_exe.trim().to_lowercase();
                        if !exe.is_empty() {
                            self.config.app_profiles.entry(exe).or_insert(AppProfile {
                                mode: Some("default".into()),
                                polish: true,
                            });
                            self.profile_exe.clear();
                        }
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_exe)
                            .hint_text("Executable, e.g. winword.exe")
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        });
    }
    fn render_provider(&mut self, ui: &mut egui::Ui) {
        section(ui, "Text polishing", |ui| {
            egui::ComboBox::from_label("Provider")
                .selected_text(&self.provider)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.provider, "local".to_string(), "local");
                    ui.add_enabled_ui(self.license_entitlements.cloud_providers, |ui| {
                        for provider in ["groq", "openai", "anthropic"] {
                            ui.selectable_value(&mut self.provider, provider.to_string(), provider);
                        }
                    });
                });
            if !self.license_entitlements.cloud_providers {
                ui.label("Cloud providers require an active Pro license.");
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Model override");
                ui.text_edit_singleline(&mut self.model);
            });
            ui.label("Leave the model override blank to use the provider default.");

            egui::CollapsingHeader::new("Cloud API credentials")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        "Credentials are stored in Windows Credential Manager, not config.toml.",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Groq API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.groq_key)
                                .password(true)
                                .hint_text("Enter a new key to replace the stored key"),
                        );
                    });
                    ui.checkbox(&mut self.remove_groq_key, "Remove stored Groq key");
                    ui.horizontal(|ui| {
                        ui.label("OpenAI API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.openai_key)
                                .password(true)
                                .hint_text("Enter a new key to replace the stored key"),
                        );
                    });
                    ui.checkbox(&mut self.remove_openai_key, "Remove stored OpenAI key");
                    ui.horizontal(|ui| {
                        ui.label("Anthropic API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.anthropic_key)
                                .password(true)
                                .hint_text("Enter a new key to replace the stored key"),
                        );
                    });
                    ui.checkbox(&mut self.remove_anthropic_key, "Remove stored Anthropic key");
                });
        });
    }

    fn finish_onboarding(&mut self) {
        self.config.settings.has_completed_onboarding = true;
        self.onboarding = false;
        self.resize_pending = true;
        self.tab = Tab::Home;
        self.status = Some(match save_config(&self.app_dir, &self.config) {
            Ok(()) => (
                true,
                "Setup complete. Restart VeeType to apply the model and hotkey.".into(),
            ),
            Err(error) => (false, format!("Could not save settings: {error:#}")),
        });
    }

    fn render_wizard(&mut self, ui: &mut egui::Ui) {
        const STEPS: u8 = 6;
        let ctx = ui.ctx().clone();
        let installed = model_files(&self.models_dir).unwrap_or_default();
        self.downloads
            .retain(|_, download| !(download.is_finished() && download.error().is_none()));
        let step = self.wizard_step.min(STEPS - 1);

        wizard_steps_labels(ui, step);
        ui.add_space(6.0);
        wizard_progress(ui, (step + 1) as f32 / STEPS as f32);
        ui.add_space(14.0);
        ui.vertical_centered(|ui| ui.image((self.logo.id(), egui::vec2(44.0, 44.0))));
        ui.add_space(18.0);
        let (title, subtitle) = match step {
            0 => ("Welcome to VeeType", "Your voice. Beautifully written."),
            1 => ("Microphone access", "VeeType needs your microphone to hear you."),
            2 => ("Choose your engine", "Pick an offline model. It downloads once and runs on this PC."),
            3 => ("Everything you get", "Powerful tools for a refined workflow."),
            4 => ("Set your hotkey", "Hold it anywhere in Windows to dictate."),
            _ => ("Test your setup", "Check your microphone, then try dictating below."),
        };
        ui.label(egui::RichText::new(title).size(28.0).strong().color(INK));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(subtitle).size(14.0).color(MUTED));
        ui.add_space(18.0);

        let width = ui.available_width();
        let content_height = (ui.available_height() - 168.0).max(120.0);
        let mut primary: (&str, bool) = ("Continue", true);
        let mut secondary: Option<&str> = None;

        ui.allocate_ui_with_layout(
            egui::vec2(width, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_height(content_height);
                match step {
                    0 => {
                        for (icon, text) in [
                            (Icon::Chip, "Speech recognition runs on your own PC"),
                            (Icon::Shield, "Your audio never leaves your device"),
                            (Icon::Key, "Optional cloud polish uses your own key (text only)"),
                        ] {
                            info_row(ui, icon, text);
                        }
                        primary = ("Get started", true);
                    }
                    1 => {
                        let ok = self.mic_status.as_ref().map(|status| status.0);
                        let tone = match ok {
                            Some(true) => OK,
                            Some(false) => ERR,
                            None => ACCENT,
                        };
                        ui.vertical_centered(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(104.0, 104.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect.expand(8.0), 36.0, tone.gamma_multiply(0.12));
                            ui.painter().rect_filled(rect, 30.0, CARD);
                            draw_icon(ui.painter(), rect.center(), 52.0, Icon::Mic, tone, CARD);
                        });
                        ui.add_space(12.0);
                        if let Some((ok, message)) = &self.mic_status {
                            ui.vertical_centered(|ui| {
                                ui.colored_label(if *ok { OK } else { ERR }, message);
                            });
                            if !*ok {
                                ui.label(
                                    egui::RichText::new("Open Windows Settings > Privacy & security > Microphone and allow desktop apps.")
                                        .small()
                                        .color(MUTED),
                                );
                            }
                        }
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("Microphone").small().color(MUTED));
                        let selected_device = self
                            .config
                            .settings
                            .input_device
                            .clone()
                            .unwrap_or_else(|| "System default".to_string());
                        egui::ComboBox::from_id_salt("wizard_mic")
                            .selected_text(selected_device)
                            .width(ui.available_width() - 16.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.config.settings.input_device,
                                    None,
                                    "System default",
                                );
                                for device in &self.input_devices {
                                    ui.selectable_value(
                                        &mut self.config.settings.input_device,
                                        Some(device.clone()),
                                        device,
                                    );
                                }
                            });
                        primary = (
                            if ok == Some(true) { "Continue" } else { "Allow microphone" },
                            true,
                        );
                    }
                    2 => {
                        for model in MODEL_CATALOG.iter().take(3) {
                            let is_installed = installed.iter().any(|f| f == model.file);
                            let download = self.downloads.get(model.file).cloned();
                            let selected = self.config.settings.whisper_model == model.file;
                            let mut action = None;
                            egui::Frame::none()
                                .fill(CARD)
                                .stroke(egui::Stroke::new(
                                    1.5_f32,
                                    if selected { ACCENT } else { egui::Color32::TRANSPARENT },
                                ))
                                .rounding(egui::Rounding::same(16.0))
                                .inner_margin(egui::Margin::symmetric(16.0, 12.0))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(egui::RichText::new(model.name).strong().color(INK));
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{} · {}",
                                                    model.languages, model.size
                                                ))
                                                .small()
                                                .color(MUTED),
                                            );
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if let Some(d) =
                                                    download.as_deref().filter(|d| !d.is_finished())
                                                {
                                                    ui.add(
                                                        egui::ProgressBar::new(d.progress())
                                                            .desired_width(90.0)
                                                            .fill(ACCENT)
                                                            .text(format!("{:.0}%", d.progress() * 100.0)),
                                                    );
                                                } else if is_installed {
                                                    if selected {
                                                        ui.colored_label(OK, "Selected");
                                                    } else if ui.button("Use").clicked() {
                                                        action = Some(1);
                                                    }
                                                } else {
                                                    if ui.button("Download").clicked() {
                                                        action = Some(0);
                                                    }
                                                    if let Some(error) =
                                                        download.as_deref().and_then(Download::error)
                                                    {
                                                        ui.colored_label(ERR, error);
                                                    }
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(8.0);
                            match action {
                                Some(0) => {
                                    self.config.settings.whisper_model = model.file.to_string();
                                    self.downloads.insert(
                                        model.file,
                                        Download::start(model.file, &self.models_dir, ctx.clone()),
                                    );
                                }
                                Some(_) => self.config.settings.whisper_model = model.file.to_string(),
                                None => {}
                            }
                        }
                        let ready = installed
                            .iter()
                            .any(|f| *f == self.config.settings.whisper_model);
                        if !ready {
                            ui.label(
                                egui::RichText::new("Download a model to continue.")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                        primary = ("Continue", ready);
                    }
                    3 => feature_carousel(ui, &mut self.feature_idx, &mut self.feature_t),
                    4 => {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 110.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 24.0, CARD);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if self.capture_hotkey {
                                "Press a key..."
                            } else {
                                self.config.settings.hotkey.as_str()
                            },
                            egui::FontId::proportional(30.0),
                            INK,
                        );
                        ui.add_space(12.0);
                        if !self.capture_hotkey && is_hotkey_pressed(&self.config.settings.hotkey) {
                            self.hotkey_verified = true;
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(30));
                        ui.vertical_centered(|ui| {
                            if self.hotkey_verified {
                                ui.colored_label(OK, "Hotkey detected");
                            } else {
                                ui.label(
                                    egui::RichText::new("Press your hotkey now to verify it.")
                                        .color(MUTED),
                                );
                            }
                        });
                        primary = ("Continue", self.hotkey_verified);
                        secondary = Some(if self.capture_hotkey {
                            "Press a key..."
                        } else {
                            "Change hotkey"
                        });
                    }
                    _ => {
                        ui.add(
                            egui::ProgressBar::new((self.mic_peak * 2.0).min(1.0))
                                .fill(OK)
                                .text(format!("Mic level {:.0}%", (self.mic_peak * 100.0).min(100.0))),
                        );
                        if let Some((ok, message)) = &self.mic_status {
                            ui.colored_label(if *ok { OK } else { ERR }, message);
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Click the box, hold {} and speak. Your words appear here once VeeType is running.",
                                self.config.settings.hotkey
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut self.wizard_scratch)
                                .hint_text("Your dictation appears here")
                                .desired_rows(4)
                                .desired_width(f32::INFINITY),
                        );
                        primary = ("Finish setup", true);
                        secondary = Some("Test microphone");
                    }
                }
            },
        );

        ui.add_space(6.0);
        let primary_clicked = pill(ui, primary.0, true, primary.1);
        ui.add_space(8.0);
        let secondary_clicked = match secondary {
            Some(label) => pill(ui, label, false, true),
            None => {
                ui.add_space(46.0);
                false
            }
        };
        ui.add_space(8.0);
        let (mut back, mut skip) = (false, false);
        if step == 0 {
            ui.vertical_centered(|ui| skip = link_button(ui, "Maybe later"));
        } else {
            ui.columns(2, |columns| {
                columns[0].with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| back = link_button(ui, "Back"),
                );
                columns[1].with_layout(
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| skip = link_button(ui, "Skip setup"),
                );
            });
        }

        if back {
            self.wizard_step = step.saturating_sub(1);
        }
        if skip {
            self.finish_onboarding();
            return;
        }
        if primary_clicked {
            match step {
                1 if self.mic_status.as_ref().map(|s| s.0) != Some(true) => self.run_mic_test(),
                5 => self.finish_onboarding(),
                _ => self.wizard_step = step + 1,
            }
        }
        if secondary_clicked {
            match step {
                4 => {
                    self.capture_hotkey = true;
                    self.hotkey_verified = false;
                }
                5 => self.run_mic_test(),
                _ => {}
            }
        }
    }

    fn run_mic_test(&mut self) {
        let (ok, message, peak) = test_microphone(self.config.settings.input_device.as_deref());
        self.mic_peak = peak;
        self.mic_status = Some((ok, message));
    }

    fn capture_hotkey_if_pressed(&mut self) {
        if !self.capture_hotkey {
            return;
        }
        if let Some(hotkey) = pressed_hotkey() {
            if is_valid_hotkey(&hotkey) {
                self.config.settings.hotkey = hotkey;
                self.capture_hotkey = false;
            }
        }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.resize_pending {
            self.resize_pending = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(820.0, 560.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1000.0, 720.0)));
        }
        self.capture_hotkey_if_pressed();
        if let Ok(message) = self.update_rx.try_recv() {
            self.update_status = message;
            self.update_checking = false;
        }
        if let Ok(message) = self.license_rx.try_recv() {
            self.license_status = message;
            self.license_busy = false;
            match LicenseManager::cached_entitlements() {
                Ok(Some(entitlements)) => self.license_entitlements = entitlements,
                Ok(None) => self.license_entitlements = Entitlements::fallback(),
                Err(error) => {
                    tracing::warn!(error = %error, "Could not verify refreshed Pro license");
                    self.license_entitlements = Entitlements::fallback();
                    self.license_status = format!("License was not accepted: {error:#}");
                }
            }
            self.license_signed_in = match LicenseManager::has_session() {
                Ok(signed_in) => signed_in,
                Err(error) => {
                    tracing::warn!(error = %error, "Could not read updated Supabase session");
                    false
                }
            };
        }
        if self.capture_hotkey {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        if self.license_busy || self.update_checking {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        if !self.onboarding {
        egui::SidePanel::left("nav_sidebar")
            .exact_width(220.0)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    .fill(SIDEBAR)
                    .stroke(egui::Stroke::new(1.0_f32, LINE))
                    .inner_margin(egui::Margin::symmetric(16.0, 16.0)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.image((self.logo.id(), egui::vec2(32.0, 32.0)));
                    ui.heading(egui::RichText::new("VeeType").color(INK).strong());
                });
                ui.add_space(24.0);
                for (group, tabs) in [
                    (
                        0,
                        &[Tab::Home, Tab::Modes, Tab::Vocabulary, Tab::Shortcuts][..],
                    ),
                    (1, &[Tab::Config, Tab::Sound, Tab::Library][..]),
                ] {
                    if group > 0 {
                        ui.add_space(14.0);
                    }
                    for &tab in tabs {
                        if sidebar_item(ui, tab, self.tab == tab).clicked() {
                            self.tab = tab;
                        }
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    let (plan, color) = if self.license_entitlements.is_pro() {
                        ("PRO", ACCENT)
                    } else {
                        ("FREE", MUTED)
                    };
                    ui.add_space(8.0);
                    egui::Frame::none()
                        .fill(CARD)
                        .stroke(egui::Stroke::new(1.0_f32, color))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label("VeeType");
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| ui.colored_label(color, egui::RichText::new(plan).strong()),
                                );
                            });
                        });
                    ui.label(
                        egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                            .small()
                            .color(MUTED),
                    );
                });
            });
        }

        if !self.onboarding {
            egui::TopBottomPanel::bottom("save_bar")
                .exact_height(60.0)
                .frame(
                    egui::Frame::none()
                        .fill(PAPER)
                        .inner_margin(egui::Margin::symmetric(24.0, 10.0)),
                )
                .show(ctx, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save = egui::Button::new(
                            egui::RichText::new("Save settings").color(ON_ACCENT).strong(),
                        )
                        .fill(ACCENT)
                        .rounding(egui::Rounding::same(18.0))
                        .min_size(egui::vec2(140.0, 36.0));
                        if ui.add(save).clicked() {
                            self.save();
                        }
                        if let Some((success, message)) = &self.status {
                            ui.colored_label(if *success { OK } else { ERR }, message);
                        }
                    });
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(PAPER)
                    .inner_margin(if self.onboarding {
                        egui::Margin::symmetric(32.0, 16.0)
                    } else {
                        egui::Margin::symmetric(24.0, 20.0)
                    }),
            )
            .show(ctx, |ui| {
            if self.onboarding {
                self.render_wizard(ui);
                return;
            }
            page_header(ui, self.tab);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                if self.tab == Tab::Home {

                }

                if self.tab == Tab::Config {
                    row_card(ui, "Hotkey", "Hold it anywhere to dictate.", |ui| {
                        if self.capture_hotkey && ui.button("Cancel").clicked() {
                            self.capture_hotkey = false;
                        }
                        let label = if self.capture_hotkey {
                            "Press a key...".to_string()
                        } else {
                            format!("{}  (change)", self.config.settings.hotkey)
                        };
                        if ui.button(label).clicked() {
                            self.capture_hotkey = true;
                        }
                    });
                    row_card(ui, "Recognition language", "Use \"auto\" for detection.", |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.config.settings.language)
                                .desired_width(160.0),
                        );
                    });
                    row_card(ui, "Maximum polishing tokens", "", |ui| {
                        ui.add(egui::Slider::new(&mut self.config.settings.max_tokens, 1..=512));
                    });
                    toggle_row(
                        ui,
                        "Start with Windows",
                        "Launch VeeType when you sign in.",
                        &mut self.config.settings.auto_start,
                        true,
                    );
                    toggle_row(
                        ui,
                        "Translate to English",
                        "Translate recognized speech into English.",
                        &mut self.config.settings.translate_to_english,
                        true,
                    );
                }

                if self.tab == Tab::Sound {
                    row_card(ui, "Microphone", "Input device used for dictation.", |ui| {
                        let selected_device = self
                            .config
                            .settings
                            .input_device
                            .clone()
                            .unwrap_or_else(|| "System default".to_string());
                        egui::ComboBox::from_id_salt("mic")
                            .selected_text(selected_device)
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.config.settings.input_device,
                                    None,
                                    "System default",
                                );
                                for device in &self.input_devices {
                                    ui.selectable_value(
                                        &mut self.config.settings.input_device,
                                        Some(device.clone()),
                                        device,
                                    );
                                }
                            });
                    });
                    toggle_row(
                        ui,
                        "Noise suppression",
                        "Filters fans, clicks and background chatter.",
                        &mut self.config.settings.noise_suppression,
                        true,
                    );
                    let hands_free = self.license_entitlements.hands_free;
                    toggle_row(
                        ui,
                        "Hands-free mode",
                        if hands_free { "Stop recording after silence." } else { "Requires a Pro license." },
                        &mut self.config.settings.hands_free,
                        hands_free,
                    );
                    row_card(ui, "Silence timeout (ms)", "", |ui| {
                        ui.add(egui::Slider::new(
                            &mut self.config.settings.silence_timeout_ms,
                            250..=10_000,
                        ));
                    });
                }
                if self.tab == Tab::Modes {
                    self.render_modes(ui);
                }
                if self.tab == Tab::Vocabulary {
                    self.render_vocabulary(ui);
                }
                if self.tab == Tab::Shortcuts {
                    self.render_voice_commands(ui);
                }
                if self.tab == Tab::Library {
                    self.render_library(ui);
                    self.render_models(ui);
                }

                if self.tab == Tab::Home {
                if !cfg!(feature = "test-bypass") {
                section(ui, "Your VeeType account", |ui| {
                    ui.label(&self.license_status);
                    ui.horizontal(|ui| {
                        ui.label("Email");
                        ui.text_edit_singleline(&mut self.license_email);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Password");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.license_password).password(true),
                        );
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(!self.license_busy, egui::Button::new("Sign in"))
                            .clicked()
                        {
                            self.start_license_action(false);
                        }
                        if ui
                            .add_enabled(!self.license_busy, egui::Button::new("Create account"))
                            .clicked()
                        {
                            self.start_license_action(true);
                        }
                        if ui
                            .add_enabled(!self.license_busy, egui::Button::new("Refresh license"))
                            .clicked()
                        {
                            self.refresh_license();
                        }
                        if ui
                            .add_enabled(
                                !self.license_busy && self.license_signed_in,
                                egui::Button::new("Subscribe / renew"),
                            )
                            .clicked()
                        {
                            self.open_checkout();
                        }
                        if ui
                            .add_enabled(
                                !self.license_busy && self.license_signed_in,
                                egui::Button::new("Sign out"),
                            )
                            .clicked()
                        {
                            match LicenseManager::sign_out() {
                                Ok(()) => {
                                    self.license_entitlements = Entitlements::default();
                                    self.license_status =
                                        "Signed out. Restart VeeType to disable Pro features."
                                            .into();
                                }
                                Err(error) => {
                                    self.license_status =
                                        format!("Could not sign out: {error:#}");
                                }
                            }
                        }
                    });
                    ui.label("Pro unlocks cloud providers, hands-free mode, and larger local models. Basic local dictation stays free.");
                });
                }
                section(ui, "Software updates", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("Current version: v{}", env!("CARGO_PKG_VERSION")));
                        if ui
                            .add_enabled(
                                !self.update_checking && !cfg!(feature = "test-bypass"),
                                egui::Button::new(if self.update_checking {
                                    "Checking for updates..."
                                } else {
                                    "Check for updates"
                                }),
                            )
                            .clicked()
                        {
                            self.update_status = "Checking GitHub for updates...".into();
                            self.update_checking = true;
                            OtaUpdater::check_for_updates_async(
                                self.update_tx.clone(),
                                ctx.clone(),
                            );
                        }
                    });
                    if cfg!(feature = "test-bypass") {
                        ui.small("Updates are disabled in test mode.");
                    }
                    if !self.update_status.is_empty() {
                        ui.label(&self.update_status);
                    }
                });
                }

            });
            });
    }
}

fn section(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .rounding(egui::Rounding::same(16.0))
        .inner_margin(egui::Margin::symmetric(18.0, 16.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(egui::RichText::new(title).size(15.0).strong().color(INK));
            contents(ui);
        });
}

const INK: egui::Color32 = egui::Color32::from_rgb(239, 235, 248);
const MUTED: egui::Color32 = egui::Color32::from_rgb(170, 161, 188);
const PAPER: egui::Color32 = egui::Color32::from_rgb(18, 16, 23);
const SIDEBAR: egui::Color32 = egui::Color32::from_rgb(11, 9, 17);
const CARD: egui::Color32 = egui::Color32::from_rgb(32, 30, 43);
const LINE: egui::Color32 = egui::Color32::from_rgb(62, 53, 78);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(134, 112, 217);
const PANEL: egui::Color32 = egui::Color32::from_rgb(40, 36, 53);
const ON_ACCENT: egui::Color32 = egui::Color32::WHITE;
const OK: egui::Color32 = egui::Color32::from_rgb(52, 211, 153);
const WARN: egui::Color32 = egui::Color32::from_rgb(251, 191, 36);
const ERR: egui::Color32 = egui::Color32::from_rgb(248, 113, 113);

/// Dark violet palette from the website night theme; edit the constants above to retheme.
fn configure_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    let rounding = egui::Rounding::same(12.0);
    visuals.panel_fill = PAPER;
    visuals.window_fill = SIDEBAR;
    visuals.extreme_bg_color = PANEL;
    visuals.faint_bg_color = CARD;
    visuals.override_text_color = Some(INK);
    visuals.window_stroke = egui::Stroke::new(1.0_f32, LINE);
    visuals.window_rounding = rounding;
    visuals.menu_rounding = rounding;
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, ON_ACCENT);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.rounding = rounding;
    }
    visuals.widgets.noninteractive.bg_stroke.color = LINE;
    visuals.widgets.inactive.bg_fill = LINE;
    visuals.widgets.inactive.weak_bg_fill = PANEL;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(58, 51, 76);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(58, 51, 76);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 7.0);
    style.spacing.interact_size.y = 30.0;
    ctx.set_style(style);
}

fn list_input_devices() -> anyhow::Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .context("Could not enumerate microphone input devices")?;
    devices
        .map(|device| {
            device
                .name()
                .context("Could not read microphone device name")
        })
        .collect()
}

fn model_files(models_dir: &Path) -> anyhow::Result<Vec<String>> {
    if !models_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = fs::read_dir(models_dir)
        .with_context(|| format!("Reading {}", models_dir.display()))?
        .map(|entry| {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                Ok(Some(entry.file_name().to_string_lossy().into_owned()))
            } else {
                Ok(None)
            }
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn copy_model(source: &Path, models_dir: &Path) -> anyhow::Result<PathBuf> {
    let extension = source
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case("gguf") && !extension.eq_ignore_ascii_case("bin") {
        anyhow::bail!("Only .gguf and .bin model files can be imported");
    }
    let file_name = source
        .file_name()
        .context("Selected model path has no file name")?;
    fs::create_dir_all(models_dir).with_context(|| format!("Creating {}", models_dir.display()))?;
    let destination = models_dir.join(file_name);
    if destination.exists() {
        anyhow::bail!(
            "A model named {} already exists; remove or rename it before importing",
            destination.display()
        );
    }
    fs::copy(source, &destination).with_context(|| {
        format!(
            "Copying model {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(destination)
}

fn save_credential(account: &str, value: &str, remove: bool) -> anyhow::Result<()> {
    if remove {
        KeyVault::delete_key(account)
    } else if !value.is_empty() {
        KeyVault::save_key(account, value)
    } else {
        Ok(())
    }
}

struct CatalogModel {
    name: &'static str,
    description: &'static str,
    file: &'static str,
    size: &'static str,
    languages: &'static str,
    speed: f32,
    accuracy: f32,
    recommended: bool,
}

static MODEL_CATALOG: &[CatalogModel] = &[
    CatalogModel {
        name: "Whisper Large v3 Turbo",
        description: "Best offline accuracy, optimized for speed.",
        file: "ggml-large-v3-turbo.bin",
        size: "1.6 GB",
        languages: "Multilingual",
        speed: 0.60,
        accuracy: 0.98,
        recommended: true,
    },
    CatalogModel {
        name: "Whisper Small (English)",
        description: "Balanced choice for everyday dictation.",
        file: "ggml-small.en.bin",
        size: "466 MB",
        languages: "English",
        speed: 0.80,
        accuracy: 0.85,
        recommended: true,
    },
    CatalogModel {
        name: "Whisper Base (English)",
        description: "Fast and light for chat and quick notes.",
        file: "ggml-base.en.bin",
        size: "142 MB",
        languages: "English",
        speed: 0.92,
        accuracy: 0.72,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny (English)",
        description: "Smallest and fastest; lowest accuracy.",
        file: "ggml-tiny.en.bin",
        size: "75 MB",
        languages: "English",
        speed: 0.98,
        accuracy: 0.60,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium (English)",
        description: "High accuracy with moderate resource use.",
        file: "ggml-medium.en.bin",
        size: "1.5 GB",
        languages: "English",
        speed: 0.45,
        accuracy: 0.92,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v3",
        description: "Maximum accuracy; needs a capable GPU.",
        file: "ggml-large-v3.bin",
        size: "3.1 GB",
        languages: "Multilingual",
        speed: 0.30,
        accuracy: 0.99,
        recommended: false,
    },

    CatalogModel {
        name: "Whisper Large v3 Turbo (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-large-v3-turbo-q5_0.bin",
        size: "547 MB",
        languages: "Multilingual",
        speed: 0.68,
        accuracy: 0.94,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v3 Turbo (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-large-v3-turbo-q8_0.bin",
        size: "834 MB",
        languages: "Multilingual",
        speed: 0.64,
        accuracy: 0.97,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v3 (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-large-v3-q5_0.bin",
        size: "1.0 GB",
        languages: "Multilingual",
        speed: 0.38,
        accuracy: 0.95,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v2",
        description: "Full-precision Whisper model.",
        file: "ggml-large-v2.bin",
        size: "2.9 GB",
        languages: "Multilingual",
        speed: 0.30,
        accuracy: 0.98,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v2 (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-large-v2-q5_0.bin",
        size: "1.0 GB",
        languages: "Multilingual",
        speed: 0.38,
        accuracy: 0.94,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v2 (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-large-v2-q8_0.bin",
        size: "1.5 GB",
        languages: "Multilingual",
        speed: 0.34,
        accuracy: 0.97,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Large v1",
        description: "Full-precision Whisper model.",
        file: "ggml-large-v1.bin",
        size: "2.9 GB",
        languages: "Multilingual",
        speed: 0.30,
        accuracy: 0.96,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium (English) (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-medium.en-q5_0.bin",
        size: "514 MB",
        languages: "English",
        speed: 0.53,
        accuracy: 0.88,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium (English) (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-medium.en-q8_0.bin",
        size: "785 MB",
        languages: "English",
        speed: 0.49,
        accuracy: 0.91,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium",
        description: "Full-precision Whisper model.",
        file: "ggml-medium.bin",
        size: "1.4 GB",
        languages: "Multilingual",
        speed: 0.45,
        accuracy: 0.92,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-medium-q5_0.bin",
        size: "514 MB",
        languages: "Multilingual",
        speed: 0.53,
        accuracy: 0.88,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Medium (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-medium-q8_0.bin",
        size: "785 MB",
        languages: "Multilingual",
        speed: 0.49,
        accuracy: 0.91,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Small (English) (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-small.en-q5_1.bin",
        size: "181 MB",
        languages: "English",
        speed: 0.88,
        accuracy: 0.81,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Small (English) (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-small.en-q8_0.bin",
        size: "252 MB",
        languages: "English",
        speed: 0.84,
        accuracy: 0.84,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Small",
        description: "Full-precision Whisper model.",
        file: "ggml-small.bin",
        size: "465 MB",
        languages: "Multilingual",
        speed: 0.80,
        accuracy: 0.85,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Small (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-small-q5_1.bin",
        size: "181 MB",
        languages: "Multilingual",
        speed: 0.88,
        accuracy: 0.81,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Small (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-small-q8_0.bin",
        size: "252 MB",
        languages: "Multilingual",
        speed: 0.84,
        accuracy: 0.84,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Base (English) (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-base.en-q5_1.bin",
        size: "57 MB",
        languages: "English",
        speed: 0.99,
        accuracy: 0.68,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Base (English) (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-base.en-q8_0.bin",
        size: "78 MB",
        languages: "English",
        speed: 0.96,
        accuracy: 0.71,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Base",
        description: "Full-precision Whisper model.",
        file: "ggml-base.bin",
        size: "141 MB",
        languages: "Multilingual",
        speed: 0.92,
        accuracy: 0.72,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Base (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-base-q5_1.bin",
        size: "57 MB",
        languages: "Multilingual",
        speed: 0.99,
        accuracy: 0.68,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Base (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-base-q8_0.bin",
        size: "78 MB",
        languages: "Multilingual",
        speed: 0.96,
        accuracy: 0.71,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny (English) (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-tiny.en-q5_1.bin",
        size: "31 MB",
        languages: "English",
        speed: 0.99,
        accuracy: 0.56,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny (English) (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-tiny.en-q8_0.bin",
        size: "42 MB",
        languages: "English",
        speed: 0.99,
        accuracy: 0.59,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny",
        description: "Full-precision Whisper model.",
        file: "ggml-tiny.bin",
        size: "74 MB",
        languages: "Multilingual",
        speed: 0.98,
        accuracy: 0.60,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny (Q5)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-tiny-q5_1.bin",
        size: "31 MB",
        languages: "Multilingual",
        speed: 0.99,
        accuracy: 0.56,
        recommended: false,
    },
    CatalogModel {
        name: "Whisper Tiny (Q8)",
        description: "Compressed build: smaller download and faster, with a small accuracy trade-off.",
        file: "ggml-tiny-q8_0.bin",
        size: "42 MB",
        languages: "Multilingual",
        speed: 0.99,
        accuracy: 0.59,
        recommended: false,
    },
];

fn metric_color(value: f32) -> egui::Color32 {
    if value >= 0.8 {
        OK
    } else if value >= 0.5 {
        ACCENT
    } else {
        WARN
    }
}

fn metric_bar(ui: &mut egui::Ui, label: &str, value: f32) {
    ui.horizontal(|ui| {
        ui.add_sized([70.0, 14.0], egui::Label::new(label));
        ui.add_sized(
            [200.0, 14.0],
            egui::ProgressBar::new(value)
                .fill(metric_color(value))
                .text(format!("{:.0}%", value * 100.0)),
        );
    });
}

/// Returns true when the user clicked Download.
fn model_card(
    ui: &mut egui::Ui,
    model: &CatalogModel,
    installed: bool,
    download: Option<&Download>,
) -> bool {
    let mut start = false;
    egui::Frame::none()
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0_f32, LINE))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(15.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.heading(model.name);
                    ui.label(model.description);
                    ui.small(format!("[ {} ] • [ {} ]", model.languages, model.size));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if installed {
                        ui.colored_label(OK, "✔ Installed");
                    } else if let Some(download) = download.filter(|d| !d.is_finished()) {
                        ui.add_sized(
                            [160.0, 18.0],
                            egui::ProgressBar::new(download.progress())
                                .text(format!("{:.0}%", download.progress() * 100.0)),
                        );
                    } else {
                        if ui
                            .button(format!("Download ({})", model.size))
                            .clicked()
                        {
                            start = true;
                        }
                        if let Some(error) = download.and_then(Download::error) {
                            ui.colored_label(ERR, error);
                        }
                    }
                });
            });
            ui.add_space(10.0);
            metric_bar(ui, "Speed", model.speed);
            metric_bar(ui, "Accuracy", model.accuracy);
        });
    ui.add_space(10.0);
    start
}

/// Opens the microphone briefly; this triggers Windows' privacy prompt when access is undecided.
fn test_microphone(device_name: Option<&str>) -> (bool, String, f32) {
    use cpal::traits::StreamTrait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let result = (|| -> anyhow::Result<f32> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .input_devices()?
                .find(|device| device.name().is_ok_and(|n| n == name))
                .context("The selected microphone is not available")?,
            None => host
                .default_input_device()
                .context("No microphone input device is available")?,
        };
        let supported = device.default_input_config()?;
        if supported.sample_format() != cpal::SampleFormat::F32 {
            anyhow::bail!("Unsupported microphone sample format");
        }
        let peak = Arc::new(AtomicU32::new(0));
        let peak_writer = Arc::clone(&peak);
        let stream = device.build_input_stream(
            &supported.into(),
            move |data: &[f32], _: &_| {
                let level = data.iter().fold(0.0_f32, |max, s| max.max(s.abs()));
                if level > f32::from_bits(peak_writer.load(Ordering::Relaxed)) {
                    peak_writer.store(level.to_bits(), Ordering::Relaxed);
                }
            },
            |error| tracing::warn!(%error, "Microphone test stream error"),
            None,
        )?;
        stream.play()?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        Ok(f32::from_bits(peak.load(Ordering::Relaxed)))
    })();

    match result {
        Ok(level) if level > 0.0 => (true, "Microphone access works.".into(), level),
        Ok(_) => (
            false,
            "The microphone opened but captured no audio; access may be blocked.".into(),
            0.0,
        ),
        Err(error) => (false, format!("Microphone unavailable: {error:#}"), 0.0),
    }
}

const LANGUAGES: [(&str, &str); 12] = [
    ("auto", "Auto-detect"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
    ("ru", "Russian"),
];

/// Full-width row with a chevron; returns true when clicked.
fn advanced_header(ui: &mut egui::Ui, open: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, 16.0, CARD.gamma_multiply(0.6));
    }
    let c = egui::pos2(rect.left() + 28.0, rect.center().y);
    let stroke = egui::Stroke::new(1.8_f32, MUTED);
    let points = if open {
        [c + egui::vec2(-4.0, -2.0), c + egui::vec2(0.0, 2.5), c + egui::vec2(4.0, -2.0)]
    } else {
        [c + egui::vec2(-2.0, -4.0), c + egui::vec2(2.5, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    painter.line_segment([points[0], points[1]], stroke);
    painter.line_segment([points[1], points[2]], stroke);
    painter.text(
        egui::pos2(rect.left() + 46.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Advanced settings",
        egui::FontId::proportional(15.0),
        INK,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

fn page_header(ui: &mut egui::Ui, tab: Tab) {
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(tab.title()).size(17.0).strong().color(INK));
    });
    ui.add_space(6.0);
    let line = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover()).0;
    ui.painter().rect_filled(line, 0.0, LINE.gamma_multiply(0.6));
    ui.add_space(16.0);
}

/// Label on the left, control on the right, inside a rounded card.
fn row_card(ui: &mut egui::Ui, label: &str, hint: &str, control: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .rounding(egui::Rounding::same(16.0))
        .inner_margin(egui::Margin::symmetric(18.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let width = ui.available_width();
            ui.allocate_ui_with_layout(
                egui::vec2(width, 32.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(egui::RichText::new(label).strong().size(15.0).color(INK));
                    if !hint.is_empty() {
                        ui.label(egui::RichText::new("(?)").color(MUTED))
                            .on_hover_text(hint);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
                },
            );
        });
}

fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(38.0, 22.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    let fill = PANEL.lerp_to_gamma(ACCENT, t);
    ui.painter().rect_filled(rect, 11.0, fill);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), 8.0, egui::Color32::WHITE);
    response
}

fn toggle_row(ui: &mut egui::Ui, label: &str, hint: &str, on: &mut bool, enabled: bool) {
    row_card(ui, label, hint, |ui| {
        ui.add_enabled_ui(enabled, |ui| toggle(ui, on));
    });
}

fn sidebar_item(ui: &mut egui::Ui, tab: Tab, active: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 36.0),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, 10.0, PANEL);
    } else if response.hovered() {
        painter.rect_filled(rect, 10.0, CARD);
    }
    let chip = egui::Rect::from_min_size(
        rect.left_center() + egui::vec2(8.0, -12.0),
        egui::vec2(24.0, 24.0),
    );
    painter.rect_filled(chip, 7.0, tab.chip_color());
    draw_glyph(painter, chip, tab);
    painter.text(
        egui::pos2(chip.right() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        match tab {
            Tab::Modes => "Modes",
            Tab::Library => "Models library",
            other => other.title(),
        },
        egui::FontId::proportional(15.0),
        if active { INK } else { MUTED },
    );
    response
}

impl SettingsApp {
    fn render_modes(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.downloads
            .retain(|_, download| !(download.is_finished() && download.error().is_none()));
        let installed = model_files(&self.models_dir).unwrap_or_default();

        row_card(ui, "Language", "Language Whisper listens for.", |ui| {
            let current = LANGUAGES
                .iter()
                .find(|(code, _)| *code == self.config.settings.language)
                .map_or(self.config.settings.language.as_str(), |(_, name)| *name)
                .to_string();
            egui::ComboBox::from_id_salt("language")
                .selected_text(current)
                .width(220.0)
                .show_ui(ui, |ui| {
                    for (code, name) in LANGUAGES {
                        ui.selectable_value(
                            &mut self.config.settings.language,
                            code.to_string(),
                            name,
                        );
                    }
                });
        });

        let selected = self.config.settings.whisper_model.clone();
        let selected_name = MODEL_CATALOG
            .iter()
            .find(|m| m.file == selected)
            .map_or(selected.clone(), |m| m.name.to_string());
        let mut choose: Option<&'static str> = None;
        row_card(ui, "Voice Model", "Local Whisper model used for dictation.", |ui| {
            egui::ComboBox::from_id_salt("voice_model")
                .selected_text(selected_name)
                .width(300.0)
                .show_ui(ui, |ui| {
                    ui.set_width(284.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add(
                        egui::TextEdit::singleline(&mut self.model_search)
                            .hint_text("Search models")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("POPULAR").small().color(MUTED));
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let query = self.model_search.to_lowercase();
                    for model in MODEL_CATALOG
                        .iter()
                        .filter(|m| query.is_empty() || m.name.to_lowercase().contains(&query))
                    {
                        let is_installed = installed.iter().any(|f| f == model.file);
                        let busy = self
                            .downloads
                            .get(model.file)
                            .is_some_and(|d| !d.is_finished());
                        let row = ui
                            .horizontal(|ui| {
                                let response = ui.selectable_label(
                                    selected == model.file,
                                    egui::RichText::new(model.name),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if is_installed {
                                            ui.colored_label(OK, "Installed");
                                        } else if busy {
                                            ui.colored_label(MUTED, "Downloading...");
                                        } else {
                                            ui.colored_label(MUTED, model.size.to_string());
                                        }
                                    },
                                );
                                response
                            })
                            .inner;
                        let row = row.on_hover_ui(|ui| {
                            ui.set_max_width(320.0);
                            ui.label(egui::RichText::new(model.name).strong());
                            ui.label(egui::RichText::new(model.description).color(MUTED));
                            ui.add_space(4.0);
                            metric_bar(ui, "Speed", model.speed);
                            metric_bar(ui, "Accuracy", model.accuracy);
                            ui.label(format!("Size  {}", model.size));
                        });
                        if row.clicked() {
                            choose = Some(model.file);
                        }
                    }
                    });
                });
        });
        if let Some(file) = choose {
            self.config.settings.whisper_model = file.to_string();
            let present = model_files(&self.models_dir)
                .unwrap_or_default()
                .iter()
                .any(|f| f == file);
            if !present && !self.downloads.contains_key(file) {
                self.downloads
                    .insert(file, Download::start(file, &self.models_dir, ctx.clone()));
            }
        }
        let progress: Vec<(&'static str, f32)> = self
            .downloads
            .iter()
            .filter(|(_, d)| !d.is_finished())
            .map(|(file, d)| (*file, d.progress()))
            .collect();
        for (file, value) in progress {
            ui.add(
                egui::ProgressBar::new(value)
                    .fill(ACCENT)
                    .text(format!("Downloading {file}: {:.0}%", value * 100.0)),
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        row_card(ui, "Keyboard shortcut", "Global push-to-talk key.", |ui| {
            if ui
                .button(if self.capture_hotkey { "Press a key..." } else { "Record shortcut" })
                .clicked()
            {
                self.capture_hotkey = true;
            }
            ui.label(egui::RichText::new(&self.config.settings.hotkey).color(MUTED));
        });

        self.render_app_profiles(ui);

        if advanced_header(ui, self.advanced_open) {
            self.advanced_open = !self.advanced_open;
        }
        if self.advanced_open {
            let hands_free_ok = self.license_entitlements.hands_free;
            toggle_row(
                ui,
                "Noise suppression",
                "Filters fans, clicks and background chatter.",
                &mut self.config.settings.noise_suppression,
                true,
            );
            toggle_row(
                ui,
                "Hands-free (Pro)",
                "Stop recording after silence. Requires Pro.",
                &mut self.config.settings.hands_free,
                hands_free_ok,
            );
            toggle_row(
                ui,
                "Translate to English",
                "Translate recognized speech into English.",
                &mut self.config.settings.translate_to_english,
                true,
            );
            self.render_provider(ui);
        }
    }

    fn render_vocabulary(&mut self, ui: &mut egui::Ui) {
        section(ui, "Custom vocabulary", |ui| {
            ui.label(
                egui::RichText::new(
                    "Teach VeeType names and terms. Whenever the first phrase is heard, it is replaced with the second.",
                )
                .color(MUTED),
            );
            ui.add_space(6.0);
            let mut entries: Vec<(String, String)> = self
                .config
                .vocabulary
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            entries.sort();
            let mut remove = None;
            for (from, to) in &entries {
                ui.horizontal(|ui| {
                    ui.label(format!("{from}  ->  {to}"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Remove").clicked() {
                            remove = Some(from.clone());
                        }
                    });
                });
            }
            if let Some(key) = remove {
                self.config.vocabulary.remove(&key);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.vocab_from)
                        .hint_text("Heard as")
                        .desired_width(150.0),
                );
                ui.label("->");
                ui.add(
                    egui::TextEdit::singleline(&mut self.vocab_to)
                        .hint_text("Replace with")
                        .desired_width(150.0),
                );
                if ui.button("Add").clicked() {
                    let from = self.vocab_from.trim().to_string();
                    let to = self.vocab_to.trim().to_string();
                    if !from.is_empty() && !to.is_empty() {
                        self.config.vocabulary.insert(from, to);
                        self.vocab_from.clear();
                        self.vocab_to.clear();
                    }
                }
            });
            ui.label(
                egui::RichText::new("Only ASCII phrases can be matched. Click Save settings to apply.")
                    .small()
                    .color(MUTED),
            );
        });
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Home,
    Bolt,
    Book,
    Gear,
    Waves,
    Layers,
    Mic,
    Apps,
    Chip,
    Key,
    Cloud,
    Clock,
    Globe,
    Shield,
}

impl Tab {
    fn icon(self) -> Icon {
        match self {
            Tab::Home => Icon::Home,
            Tab::Modes => Icon::Bolt,
            Tab::Vocabulary => Icon::Book,
            Tab::Shortcuts => Icon::Apps,
            Tab::Config => Icon::Gear,
            Tab::Sound => Icon::Waves,
            Tab::Library => Icon::Layers,
        }
    }
}

fn draw_glyph(painter: &egui::Painter, r: egui::Rect, tab: Tab) {
    draw_icon(
        painter,
        r.center(),
        r.width() * 0.62,
        tab.icon(),
        egui::Color32::WHITE,
        tab.chip_color(),
    );
}

/// Solid line-art icons drawn with the painter; `cut` is the colour behind the icon.
fn draw_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    icon: Icon,
    color: egui::Color32,
    cut: egui::Color32,
) {
    use egui::{Rect, Shape, Stroke};
    let s = size / 2.0;
    let u = |x: f32, y: f32| center + egui::vec2(x * s, y * s);
    let sw = (size * 0.09).max(1.5);
    let stroke = Stroke::new(sw, color);
    let thick = Stroke::new(sw * 1.7, color);
    let rect = |a: (f32, f32), b: (f32, f32)| Rect::from_min_max(u(a.0, a.1), u(b.0, b.1));
    let none = Stroke::NONE;
    match icon {
        Icon::Home => {
            painter.add(Shape::convex_polygon(
                vec![u(-1.0, -0.05), u(0.0, -0.95), u(1.0, -0.05)],
                color,
                none,
            ));
            painter.rect_filled(rect((-0.72, -0.1), (0.72, 0.9)), 0.1 * s, color);
            painter.rect_filled(rect((-0.2, 0.35), (0.2, 0.9)), 0.05 * s, cut);
        }
        Icon::Bolt => {
            painter.add(Shape::convex_polygon(
                vec![u(0.25, -1.0), u(-0.45, 0.15), u(0.1, 0.15)],
                color,
                none,
            ));
            painter.add(Shape::convex_polygon(
                vec![u(-0.1, -0.15), u(0.45, -0.15), u(-0.25, 1.0)],
                color,
                none,
            ));
        }
        Icon::Book => {
            painter.rect_filled(rect((-0.95, -0.7), (-0.05, 0.8)), 0.12 * s, color);
            painter.rect_filled(rect((0.05, -0.7), (0.95, 0.8)), 0.12 * s, color);
            for y in [-0.3_f32, 0.05, 0.4] {
                let line = Stroke::new(sw * 0.8, cut);
                painter.line_segment([u(-0.72, y), u(-0.3, y)], line);
                painter.line_segment([u(0.3, y), u(0.72, y)], line);
            }
        }
        Icon::Gear => {
            painter.circle_stroke(center, 0.5 * s, thick);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = egui::vec2(a.cos(), a.sin());
                painter.line_segment([center + d * 0.72 * s, center + d * 0.98 * s], thick);
            }
            painter.circle_filled(center, 0.16 * s, color);
        }
        Icon::Waves => {
            for (x, h) in [(-0.8_f32, 0.4_f32), (-0.4, 0.85), (0.0, 1.0), (0.4, 0.7), (0.8, 0.45)] {
                painter.rect_filled(rect((x - 0.11, -h), (x + 0.11, h)), 0.11 * s, color);
            }
        }
        Icon::Layers => {
            painter.add(Shape::convex_polygon(
                vec![u(0.0, -0.9), u(1.0, -0.45), u(0.0, 0.0), u(-1.0, -0.45)],
                color,
                none,
            ));
            painter.add(Shape::line(vec![u(-1.0, -0.05), u(0.0, 0.4), u(1.0, -0.05)], thick));
            painter.add(Shape::line(vec![u(-1.0, 0.42), u(0.0, 0.87), u(1.0, 0.42)], thick));
        }
        Icon::Mic => {
            painter.rect_filled(rect((-0.3, -0.95), (0.3, 0.15)), 0.3 * s, color);
            let arc: Vec<egui::Pos2> = (0..=16)
                .map(|i| {
                    let t = std::f32::consts::PI * i as f32 / 16.0;
                    u(t.cos() * 0.66, 0.0 + t.sin() * 0.66)
                })
                .collect();
            painter.add(Shape::line(arc, stroke));
            painter.line_segment([u(0.0, 0.66), u(0.0, 0.95)], stroke);
            painter.line_segment([u(-0.35, 0.95), u(0.35, 0.95)], stroke);
        }
        Icon::Apps => {
            for (x, y) in [(-0.9_f32, -0.9_f32), (0.1, -0.9), (-0.9, 0.1), (0.1, 0.1)] {
                painter.rect_filled(rect((x, y), (x + 0.8, y + 0.8)), 0.2 * s, color);
            }
        }
        Icon::Chip => {
            for k in [-0.5_f32, 0.0, 0.5] {
                painter.line_segment([u(k, -1.0), u(k, -0.6)], stroke);
                painter.line_segment([u(k, 0.6), u(k, 1.0)], stroke);
                painter.line_segment([u(-1.0, k), u(-0.6, k)], stroke);
                painter.line_segment([u(0.6, k), u(1.0, k)], stroke);
            }
            painter.rect_filled(rect((-0.65, -0.65), (0.65, 0.65)), 0.15 * s, color);
            painter.rect_filled(rect((-0.3, -0.3), (0.3, 0.3)), 0.08 * s, cut);
        }
        Icon::Key => {
            painter.circle_stroke(u(-0.45, -0.45), 0.36 * s, thick);
            painter.line_segment([u(-0.2, -0.2), u(0.9, 0.9)], thick);
            painter.line_segment([u(0.5, 0.5), u(0.8, 0.2)], thick);
            painter.line_segment([u(0.25, 0.25), u(0.5, 0.0)], thick);
        }
        Icon::Cloud => {
            painter.circle_filled(u(-0.4, 0.15), 0.4 * s, color);
            painter.circle_filled(u(0.05, -0.2), 0.5 * s, color);
            painter.circle_filled(u(0.5, 0.15), 0.35 * s, color);
            painter.rect_filled(rect((-0.4, 0.1), (0.5, 0.55)), 0.0, color);
        }
        Icon::Clock => {
            painter.circle_stroke(center, 0.9 * s, thick);
            painter.line_segment([u(0.0, 0.0), u(0.0, -0.55)], thick);
            painter.line_segment([u(0.0, 0.0), u(0.4, 0.2)], thick);
        }
        Icon::Globe => {
            painter.circle_stroke(center, 0.9 * s, stroke);
            let ellipse: Vec<egui::Pos2> = (0..32)
                .map(|i| {
                    let t = std::f32::consts::TAU * i as f32 / 32.0;
                    u(t.cos() * 0.4, t.sin() * 0.9)
                })
                .collect();
            painter.add(Shape::closed_line(ellipse, stroke));
            painter.line_segment([u(-0.9, 0.0), u(0.9, 0.0)], stroke);
            painter.line_segment([u(-0.78, -0.45), u(0.78, -0.45)], stroke);
            painter.line_segment([u(-0.78, 0.45), u(0.78, 0.45)], stroke);
        }
        Icon::Shield => {
            painter.add(Shape::convex_polygon(
                vec![u(-0.8, -0.65), u(0.0, -0.95), u(0.8, -0.65), u(0.7, 0.15), u(0.0, 0.95), u(-0.7, 0.15)],
                color,
                none,
            ));
            let tick = Stroke::new(sw, cut);
            painter.line_segment([u(-0.3, 0.0), u(-0.05, 0.3)], tick);
            painter.line_segment([u(-0.05, 0.3), u(0.35, -0.25)], tick);
        }
    }
}

struct Feature {
    icon: Icon,
    label: &'static str,
    text: &'static str,
}

const FEATURES: [Feature; 11] = [
    Feature { icon: Icon::Mic, label: "Dictate anywhere", text: "Hold your hotkey and speak. Text is typed into whichever app has focus." },
    Feature { icon: Icon::Apps, label: "Voice shortcuts", text: "Say \"Open my projects\" to jump to any folder, file, app or website you have set up." },
    Feature { icon: Icon::Apps, label: "Open apps", text: "Say \"Open Microsoft Word\" or \"Launch Photoshop\" and VeeType starts it instead of typing." },
    Feature { icon: Icon::Book, label: "Vocabulary", text: "Teach it names and terms so they are always spelled your way." },
    Feature { icon: Icon::Waves, label: "Noise filter", text: "Studio noise suppression removes fans, clicks and background chatter." },
    Feature { icon: Icon::Layers, label: "App profiles", text: "Choose a writing mode for each application you dictate into." },
    Feature { icon: Icon::Chip, label: "Local models", text: "Whisper models run on your own PC, with no audio uploaded." },
    Feature { icon: Icon::Globe, label: "Languages", text: "Dictate in many languages, or let VeeType detect them automatically." },
    Feature { icon: Icon::Cloud, label: "Cloud polish", text: "Pro: tidy transcripts with a cloud model using your own key." },
    Feature { icon: Icon::Key, label: "Your own keys", text: "Pro: bring your own Groq or OpenAI key. Only text is ever sent." },
    Feature { icon: Icon::Clock, label: "Hands-free", text: "Pro: stop recording automatically after you pause speaking." },
];

fn wizard_steps_labels(ui: &mut egui::Ui, current: u8) {
    const NAMES: [&str; 6] = ["Welcome", "Permissions", "Engine", "Features", "Hotkey", "Test"];
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    let slot = rect.width() / NAMES.len() as f32;
    for (i, name) in NAMES.iter().enumerate() {
        let color = if i as u8 == current {
            INK
        } else if (i as u8) < current {
            ACCENT
        } else {
            MUTED.gamma_multiply(0.6)
        };
        ui.painter().text(
            egui::pos2(rect.left() + slot * (i as f32 + 0.5), rect.center().y),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(12.0),
            color,
        );
    }
}

fn wizard_progress(ui: &mut egui::Ui, fraction: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 6.0),
        egui::Sense::hover(),
    );
    let value = ui
        .ctx()
        .animate_value_with_time(egui::Id::new("wizard_progress"), fraction, 0.4);
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, CARD);
    let fill = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * value, rect.height()));
    painter.rect_filled(fill.expand(5.0), 8.0, ACCENT.gamma_multiply(0.10));
    painter.rect_filled(fill.expand(2.5), 6.0, ACCENT.gamma_multiply(0.22));
    painter.rect_filled(fill, 3.0, ACCENT);
}

fn pill(ui: &mut egui::Ui, text: &str, primary: bool, enabled: bool) -> bool {
    let (fill, color) = if primary { (ACCENT, ON_ACCENT) } else { (PANEL, INK) };
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 46.0), sense);
    let alpha = if !enabled { 0.45 } else if response.hovered() { 0.9 } else { 1.0 };
    let painter = ui.painter();
    painter.rect_filled(rect, 23.0, fill.gamma_multiply(alpha));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(15.0),
        if enabled { color } else { color.gamma_multiply(0.6) },
    );
    if enabled {
        response.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    response.clicked()
}

fn link_button(ui: &mut egui::Ui, text: &str) -> bool {
    ui.add(egui::Label::new(egui::RichText::new(text).color(MUTED)).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

fn info_row(ui: &mut egui::Ui, icon: Icon, text: &str) {
    egui::Frame::none()
        .fill(CARD)
        .rounding(egui::Rounding::same(16.0))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                draw_icon(ui.painter(), rect.center(), 24.0, icon, ACCENT, CARD);
                ui.label(egui::RichText::new(text).size(14.0).color(INK));
            });
        });
    ui.add_space(8.0);
}

/// Sliding strip of feature cards; the focused card is highlighted and explained below.
fn feature_carousel(ui: &mut egui::Ui, index: &mut usize, last_change: &mut f64) {
    const PITCH: f32 = 136.0;
    let now = ui.input(|input| input.time);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 160.0),
        egui::Sense::hover(),
    );
    let screen = ui.ctx().screen_rect();
    let clip = egui::Rect::from_x_y_ranges(screen.x_range(), rect.y_range());
    if now - *last_change > 3.2 && *index + 1 < FEATURES.len() {
        *index += 1;
        *last_change = now;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
    let position = ui
        .ctx()
        .animate_value_with_time(egui::Id::new("feature_pos"), *index as f32, 0.5);
    let painter = ui.painter().with_clip_rect(clip);
    let dim = egui::Color32::from_rgb(92, 86, 108);
    for (i, feature) in FEATURES.iter().enumerate() {
        let delta = i as f32 - position;
        let cx = rect.center().x + delta * PITCH;
        if (cx - rect.center().x).abs() > screen.width() / 2.0 + 90.0 {
            continue;
        }
        let focus = (1.0 - delta.abs()).clamp(0.0, 1.0);
        let card = egui::Rect::from_center_size(
            egui::pos2(cx, rect.center().y),
            egui::vec2(120.0, 150.0),
        );
        let fill = CARD.lerp_to_gamma(PANEL, focus);
        painter.rect_filled(card, 22.0, fill);
        draw_icon(
            &painter,
            card.center() - egui::vec2(0.0, 14.0),
            54.0,
            feature.icon,
            dim.lerp_to_gamma(INK, focus),
            fill,
        );
        painter.text(
            card.center() + egui::vec2(0.0, 48.0),
            egui::Align2::CENTER_CENTER,
            feature.label,
            egui::FontId::proportional(13.0),
            MUTED.lerp_to_gamma(INK, focus),
        );
        let response = ui
            .interact(card.intersect(clip), egui::Id::new(("feature_card", i)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            *index = i;
            *last_change = now;
        }
    }

    ui.add_space(14.0);
    let current = &FEATURES[(*index).min(FEATURES.len() - 1)];
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(current.label).size(18.0).strong().color(INK));
        ui.add_space(2.0);
        ui.label(egui::RichText::new(current.text).size(13.0).color(MUTED));
    });
    ui.add_space(10.0);

    let (dots, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 14.0),
        egui::Sense::hover(),
    );
    let count = FEATURES.len();
    let left = dots.center().x - (count as f32 - 1.0) * 8.0;
    for i in 0..count {
        let center = egui::pos2(left + i as f32 * 16.0, dots.center().y);
        let active = i == *index;
        ui.painter().circle_filled(center, if active { 4.0 } else { 3.0 }, if active { ACCENT } else { LINE });
        let response = ui.interact(
            egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0)),
            egui::Id::new(("feature_dot", i)),
            egui::Sense::click(),
        );
        if response.clicked() {
            *index = i;
            *last_change = now;
        }
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn catalog_files_are_unique_ggml_names() {
        let mut seen = std::collections::HashSet::new();
        for model in MODEL_CATALOG {
            assert!(model.file.starts_with("ggml-") && model.file.ends_with(".bin"));
            assert!(seen.insert(model.file), "duplicate {}", model.file);
        }
        assert!(MODEL_CATALOG.len() >= 30);
    }
}
