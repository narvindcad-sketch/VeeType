//! Audio capture, transcription, and text-polishing backend modules.

pub mod audio;
pub mod cloud;
pub mod downloader;
pub mod hardware;
pub mod keychain;
pub mod launcher;
pub mod license;
pub mod llm;
pub mod startup;
pub mod updater;

pub use keychain::KeyVault;
pub use license::{effective_provider, hands_free_enabled, Entitlements, LicenseManager};
pub use updater::OtaUpdater;

pub fn worker_thread_count() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(2)
        .clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use super::worker_thread_count;

    #[test]
    fn worker_thread_count_is_capped_and_nonzero() {
        assert!((1..=4).contains(&worker_thread_count()));
    }
}
