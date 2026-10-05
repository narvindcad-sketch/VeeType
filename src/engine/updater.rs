use self_update::cargo_crate_version;
use std::sync::mpsc::Sender;
use std::thread;

const REPOSITORY_OWNER: &str = "narvindcad-sketch";
const REPOSITORY_NAME: &str = "VeeType";

pub struct OtaUpdater;

impl OtaUpdater {
    fn perform_update() -> anyhow::Result<String> {
        let status = self_update::backends::github::Update::configure()
            .repo_owner(REPOSITORY_OWNER)
            .repo_name(REPOSITORY_NAME)
            .bin_name("VeeType")
            .show_download_progress(false)
            .show_output(false)
            .no_confirm(true)
            .current_version(cargo_crate_version!())
            .build()?
            .update()?;

        if status.updated() {
            Ok(format!(
                "Updated to {}. Close and restart VeeType to use the new version.",
                status.version()
            ))
        } else {
            Ok(format!(
                "VeeType is already up to date ({}).",
                status.version()
            ))
        }
    }

    pub fn check_for_updates_async(status_sender: Sender<String>, ctx: eframe::egui::Context) {
        thread::spawn(move || {
            let message =
                Self::perform_update().unwrap_or_else(|error| format!("Update failed: {error:#}"));
            let _ = status_sender.send(message);
            ctx.request_repaint();
        });
    }
}
