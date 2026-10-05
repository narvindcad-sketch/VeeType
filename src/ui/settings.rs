use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait};
use eframe::egui;

use crate::config::{load_config, save_config, AppConfig};
use crate::engine::{Entitlements, KeyVault, LicenseManager, OtaUpdater};
use crate::utils::hotkey::{is_valid_hotkey, pressed_hotkey};

pub fn run(app_dir: PathBuf) -> anyhow::Result<()> {
    let config = load_config(&app_dir)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("VeeType Settings")
            .with_inner_size([700.0, 720.0])
            .with_min_inner_size([520.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "VeeType Settings",
        options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(SettingsApp::new(app_dir, config)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("VeeType Settings window failed: {error}"))
}

struct SettingsApp {
    app_dir: PathBuf,
    models_dir: PathBuf,
    config: AppConfig,
    provider: String,
    model: String,
    input_devices: Vec<String>,
    capture_hotkey: bool,
    groq_key: String,
    openai_key: String,
    remove_groq_key: bool,
    remove_openai_key: bool,
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
}

impl SettingsApp {
    fn new(app_dir: PathBuf, config: AppConfig) -> Self {
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
                Entitlements::default(),
                "No active Pro license is stored on this device.".into(),
            ),
            Err(error) => {
                tracing::warn!(error = %error, "Could not verify cached Pro license");
                (
                    Entitlements::default(),
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

        Self {
            app_dir,
            models_dir,
            config,
            provider,
            model,
            input_devices,
            capture_hotkey: false,
            groq_key: String::new(),
            openai_key: String::new(),
            remove_groq_key: false,
            remove_openai_key: false,
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

    fn render_models(&mut self, ui: &mut egui::Ui) {
        ui.heading("Local models");
        ui.label("Whisper and GGUF model files in the application Models folder:");

        match model_files(&self.models_dir) {
            Ok(files) if files.is_empty() => {
                ui.colored_label(egui::Color32::YELLOW, "No model files found.");
            }
            Ok(files) => {
                for file in files {
                    ui.label(file);
                }
            }
            Err(error) => {
                ui.colored_label(
                    egui::Color32::LIGHT_RED,
                    format!("Could not list model files: {error:#}"),
                );
            }
        }

        if ui.button("Import GGUF model...").clicked() {
            self.import_model();
        }
    }

    fn render_provider(&mut self, ui: &mut egui::Ui) {
        ui.heading("Text polishing provider");
        egui::ComboBox::from_label("Provider")
            .selected_text(&self.provider)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.provider, "local".to_string(), "local");
                ui.add_enabled_ui(self.license_entitlements.cloud_providers, |ui| {
                    for provider in ["groq", "openai"] {
                        ui.selectable_value(&mut self.provider, provider.to_string(), provider);
                    }
                });
            });
        if !self.license_entitlements.cloud_providers {
            ui.label("Cloud providers require an active Pro license.");
        }
        ui.horizontal(|ui| {
            ui.label("Model override");
            ui.text_edit_singleline(&mut self.model);
        });
        ui.label("Leave the model override blank to use the provider default.");

        egui::CollapsingHeader::new("Cloud API credentials")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Credentials are stored in Windows Credential Manager, not config.toml.");
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
            });
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
                Ok(None) => self.license_entitlements = Entitlements::default(),
                Err(error) => {
                    tracing::warn!(error = %error, "Could not verify refreshed Pro license");
                    self.license_entitlements = Entitlements::default();
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

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("VeeType Settings");
            ui.label("Changes are saved locally and apply after restarting VeeType.");
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("VeeType Pro account");
                ui.label(&self.license_status);
                ui.horizontal(|ui| {
                    ui.label("Email");
                    ui.text_edit_singleline(&mut self.license_email);
                });
                ui.horizontal(|ui| {
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.license_password)
                            .password(true),
                    );
                });
                ui.horizontal(|ui| {
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
                                    "Signed out. Restart VeeType to disable Pro features.".into();
                            }
                            Err(error) => {
                                self.license_status = format!("Could not sign out: {error:#}");
                            }
                        }
                    }
                });
                ui.label("An active Pro subscription unlocks cloud providers, hands-free mode, and larger local models. Local basic dictation remains free.");
                ui.separator();

                ui.heading("Dictation");
                ui.horizontal(|ui| {
                    ui.label(format!("Hotkey: {}", self.config.settings.hotkey));
                    if ui
                        .button(if self.capture_hotkey {
                            "Press a key..."
                        } else {
                            "Bind hotkey"
                        })
                        .clicked()
                    {
                        self.capture_hotkey = true;
                    }
                    if self.capture_hotkey && ui.button("Cancel").clicked() {
                        self.capture_hotkey = false;
                    }
                });

                let selected_device = self
                    .config
                    .settings
                    .input_device
                    .clone()
                    .unwrap_or_else(|| "System default".to_string());
                egui::ComboBox::from_label("Microphone")
                    .selected_text(selected_device)
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

                ui.add(
                    egui::Slider::new(&mut self.config.settings.max_tokens, 1..=512)
                        .text("Maximum polishing tokens"),
                );
                ui.add_enabled_ui(self.license_entitlements.hands_free, |ui| {
                    ui.checkbox(
                        &mut self.config.settings.hands_free,
                        "Hands-free mode (stop after silence)",
                    );
                });
                if !self.license_entitlements.hands_free {
                    ui.label("Hands-free mode requires an active Pro license.");
                }
                ui.checkbox(
                    &mut self.config.settings.auto_start,
                    "Start VeeType automatically when I sign in",
                );
                ui.checkbox(
                    &mut self.config.settings.translate_to_english,
                    "Translate recognized speech into English",
                );
                ui.horizontal(|ui| {
                    ui.label("Recognition language");
                    ui.text_edit_singleline(&mut self.config.settings.language);
                    ui.label("(use “auto” for detection)");
                });
                ui.add(
                    egui::Slider::new(&mut self.config.settings.silence_timeout_ms, 250..=10_000)
                        .text("Silence timeout (ms)"),
                );

                ui.separator();
                self.render_provider(ui);
                ui.separator();
                self.render_models(ui);

                ui.separator();
                ui.heading("Software updates");
                ui.horizontal(|ui| {
                    ui.label(format!("Current version: v{}", env!("CARGO_PKG_VERSION")));
                    if ui
                        .add_enabled(
                            !self.update_checking,
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
                        OtaUpdater::check_for_updates_async(self.update_tx.clone(), ctx.clone());
                    }
                });
                if !self.update_status.is_empty() {
                    ui.label(&self.update_status);
                }

                ui.separator();
                if let Some((success, message)) = &self.status {
                    ui.colored_label(
                        if *success {
                            egui::Color32::LIGHT_GREEN
                        } else {
                            egui::Color32::LIGHT_RED
                        },
                        message,
                    );
                }
                if ui.button("Save settings").clicked() {
                    self.save();
                }
            });
        });
    }
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
