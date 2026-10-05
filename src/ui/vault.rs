//! Egui-based local dictation history window.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use chrono::Local;
use eframe::egui;
use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VaultEntry {
    pub timestamp: String,
    pub raw: String,
    pub polished: String,
}

pub fn load_entries(path: &Path) -> anyhow::Result<Vec<VaultEntry>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("Reading {}", path.display())),
    };
    serde_json::from_str(&contents).with_context(|| format!("Parsing {}", path.display()))
}

pub fn save_entry(path: &Path, raw: &str, polished: &str) -> anyhow::Result<()> {
    let mut entries = load_entries(path)?;
    entries.push(VaultEntry {
        timestamp: Local::now().format("%Y-%m-%d %I:%M:%S %p").to_string(),
        raw: raw.to_string(),
        polished: polished.to_string(),
    });
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }

    let contents = serde_json::to_vec_pretty(&entries).context("Serializing dictation history")?;
    fs::write(path, contents).with_context(|| format!("Saving {}", path.display()))
}

pub fn clear_entries(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Clearing {}", path.display())),
    }
}

pub fn run(path: PathBuf) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dictation Vault")
            .with_inner_size([720.0, 620.0])
            .with_min_inner_size([480.0, 360.0])
            .with_transparent(true),
        ..Default::default()
    };

    eframe::run_native(
        "Dictation Vault",
        options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(VaultApp::new(path)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Dictation Vault window failed: {error}"))
}

struct VaultApp {
    path: PathBuf,
    entries: Vec<VaultEntry>,
    search_query: String,
    status: Option<String>,
    confirm_clear: bool,
    last_auto_refresh: Instant,
}

impl VaultApp {
    fn new(path: PathBuf) -> Self {
        let (entries, status) = match load_entries(&path) {
            Ok(entries) => (entries, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Could not load history: {error:#}")),
            ),
        };
        Self {
            path,
            entries,
            search_query: String::new(),
            status,
            confirm_clear: false,
            last_auto_refresh: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        match load_entries(&self.path) {
            Ok(entries) => {
                self.entries = entries;
                self.status = None;
            }
            Err(error) => self.status = Some(format!("Could not load history: {error:#}")),
        }
    }

    fn render_entry(ui: &mut egui::Ui, entry: &VaultEntry) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgba_unmultiplied(35, 42, 55, 210))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_rgba_unmultiplied(120, 150, 180, 45),
            ))
            .inner_margin(egui::Margin::same(12.0))
            .rounding(egui::Rounding::same(10.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&entry.timestamp)
                            .small()
                            .color(egui::Color32::from_rgb(115, 205, 255)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Copy polished text").clicked() {
                            ui.ctx().copy_text(entry.polished.clone());
                        }
                    });
                });

                ui.add_space(6.0);
                ui.label(egui::RichText::new("Polished").strong());
                ui.label(&entry.polished);
                ui.collapsing("Raw transcript", |ui| {
                    ui.label(&entry.raw);
                });
            });
    }
}

impl eframe::App for VaultApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_auto_refresh.elapsed() >= Duration::from_secs(1) {
            self.refresh();
            self.last_auto_refresh = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_unmultiplied(19, 24, 34, 232))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        egui::Color32::from_rgba_unmultiplied(160, 195, 230, 90),
                    ))
                    .inner_margin(egui::Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.heading(
                            egui::RichText::new("Dictation Vault")
                                .size(24.0)
                                .color(egui::Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new("Your recent dictations, stored on this device")
                                .color(egui::Color32::LIGHT_GRAY),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Clear history").clicked() {
                            self.confirm_clear = true;
                        }
                        if ui.button("Refresh").clicked() {
                            self.refresh();
                        }
                    });
                });

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search_query)
                            .hint_text("Filter by raw or polished text...")
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.add_space(10.0);

                if let Some(status) = &self.status {
                    ui.colored_label(egui::Color32::LIGHT_RED, status);
                }

                let filter = self.search_query.to_lowercase();
                let filtered: Vec<_> = self
                    .entries
                    .iter()
                    .rev()
                    .filter(|entry| {
                        filter.is_empty()
                            || entry.raw.to_lowercase().contains(&filter)
                            || entry.polished.to_lowercase().contains(&filter)
                    })
                    .collect();
                ui.label(format!(
                    "{} dictation{}",
                    filtered.len(),
                    if filtered.len() == 1 { "" } else { "s" }
                ));
                ui.separator();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if filtered.is_empty() {
                            ui.add_space(24.0);
                            ui.centered_and_justified(|ui| {
                                ui.label(if self.search_query.is_empty() {
                                    "No dictations yet."
                                } else {
                                    "No dictations match your search."
                                });
                            });
                        } else {
                            for entry in filtered {
                                Self::render_entry(ui, entry);
                                ui.add_space(8.0);
                            }
                        }
                    });
            });

        if self.confirm_clear {
            egui::Window::new("Clear dictation history?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("This permanently removes all saved dictations.");
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.confirm_clear = false;
                        }
                        if ui.button("Clear history").clicked() {
                            self.confirm_clear = false;
                            match clear_entries(&self.path) {
                                Ok(()) => {
                                    self.entries.clear();
                                    self.status = Some("Dictation history cleared.".to_string());
                                }
                                Err(error) => {
                                    self.status =
                                        Some(format!("Could not clear history: {error:#}"));
                                }
                            }
                        }
                    });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{clear_entries, load_entries, save_entry};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_vault_path() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("voice-dictation-vault-{id}.json"))
    }

    #[test]
    fn vault_persists_raw_and_polished_text_and_clear_removes_history() {
        let path = temporary_vault_path();
        save_entry(&path, "raw words", "Polished words").expect("save entry");
        let entries = load_entries(&path).expect("load entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].raw, "raw words");
        assert_eq!(entries[0].polished, "Polished words");
        assert!(!entries[0].timestamp.is_empty());

        clear_entries(&path).expect("clear entries");
        assert!(load_entries(&path)
            .expect("load cleared entries")
            .is_empty());
    }

    #[test]
    fn vault_retains_only_the_most_recent_one_hundred_entries() {
        let path = temporary_vault_path();
        for index in 0..105 {
            save_entry(&path, &format!("raw {index}"), &format!("polished {index}"))
                .expect("save entry");
        }
        let entries = load_entries(&path).expect("load entries");
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[0].raw, "raw 5");
        assert_eq!(entries[99].raw, "raw 104");
        clear_entries(&path).expect("remove test vault");
    }
}
