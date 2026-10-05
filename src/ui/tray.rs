use anyhow::Context;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, Submenu},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::config::PromptMode;

pub struct Tray {
    _icon: TrayIcon,
    vault_menu_id: tray_icon::menu::MenuId,
    transcribe_menu_id: tray_icon::menu::MenuId,
    quit_menu_id: tray_icon::menu::MenuId,
    auto_prompt_item: CheckMenuItem,
    coding_prompt_item: CheckMenuItem,
    professional_prompt_item: CheckMenuItem,
    auto_prompt_id: tray_icon::menu::MenuId,
    coding_prompt_id: tray_icon::menu::MenuId,
    professional_prompt_id: tray_icon::menu::MenuId,
}

impl Tray {
    pub fn new() -> anyhow::Result<Self> {
        let tray_menu = Menu::new();
        let vault_item = MenuItem::new("View Dictation Vault...", true, None);
        let vault_menu_id = vault_item.id().clone();
        let transcribe_item = MenuItem::new("Transcribe Audio/Video File...", true, None);
        let transcribe_menu_id = transcribe_item.id().clone();
        let prompt_menu = Submenu::new("Active Prompt", true);
        let auto_prompt_item = CheckMenuItem::new("Auto Mode", true, true, None);
        let auto_prompt_id = auto_prompt_item.id().clone();
        let coding_prompt_item = CheckMenuItem::new("Coding Mode", true, false, None);
        let coding_prompt_id = coding_prompt_item.id().clone();
        let professional_prompt_item = CheckMenuItem::new("Professional Mode", true, false, None);
        let professional_prompt_id = professional_prompt_item.id().clone();
        prompt_menu
            .append_items(&[
                &auto_prompt_item,
                &coding_prompt_item,
                &professional_prompt_item,
            ])
            .context("Could not create the Active Prompt tray menu")?;
        let quit_item = MenuItem::new("Quit VeeType", true, None);
        let quit_menu_id = quit_item.id().clone();
        tray_menu
            .append_items(&[&vault_item, &transcribe_item, &prompt_menu, &quit_item])
            .context("Could not create the VeeType tray menu")?;

        let icon_rgba = (0..(16 * 16))
            .flat_map(|_| [0u8, 180u8, 255u8, 255u8])
            .collect::<Vec<_>>();
        let icon = Icon::from_rgba(icon_rgba, 16, 16).context("Could not create tray icon")?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_tooltip("VeeType (Local transcription)")
            .with_icon(icon)
            .build()
            .context("Could not build VeeType tray icon")?;

        Ok(Self {
            _icon: icon,
            vault_menu_id,
            transcribe_menu_id,
            quit_menu_id,
            auto_prompt_item,
            coding_prompt_item,
            professional_prompt_item,
            auto_prompt_id,
            coding_prompt_id,
            professional_prompt_id,
        })
    }

    pub fn is_quit_event(&self, event: &MenuEvent) -> bool {
        event.id == self.quit_menu_id
    }

    pub fn is_vault_event(&self, event: &MenuEvent) -> bool {
        event.id == self.vault_menu_id
    }

    pub fn is_transcribe_event(&self, event: &MenuEvent) -> bool {
        event.id == self.transcribe_menu_id
    }

    pub fn handle_prompt_event(&self, event: &MenuEvent) -> Option<PromptMode> {
        let mode = if event.id == self.auto_prompt_id {
            PromptMode::Auto
        } else if event.id == self.coding_prompt_id {
            PromptMode::Coding
        } else if event.id == self.professional_prompt_id {
            PromptMode::Professional
        } else {
            return None;
        };

        self.auto_prompt_item.set_checked(mode == PromptMode::Auto);
        self.coding_prompt_item
            .set_checked(mode == PromptMode::Coding);
        self.professional_prompt_item
            .set_checked(mode == PromptMode::Professional);
        Some(mode)
    }
}
