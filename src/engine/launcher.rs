//! Voice-driven application launching backed by a Start Menu shortcut index.

use crate::config::VoiceCommandConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use strsim::jaro_winkler;
use walkdir::WalkDir;

const MATCH_THRESHOLD: f64 = 0.85;

static APP_INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
static INDEXING_STARTED: OnceLock<()> = OnceLock::new();

/// Builds the Start Menu index on a background thread.
pub fn start_indexing() {
    if INDEXING_STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        let _ = APP_INDEX.set(index_windows_apps());
    });
}

pub fn index_windows_apps() -> HashMap<String, PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("ProgramData") {
        roots.push(PathBuf::from(dir).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Some(dir) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(dir).join(r"Microsoft\Windows\Start Menu\Programs"));
    }

    let mut index = HashMap::new();
    for root in roots {
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let is_lnk = path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"));
            if !is_lnk {
                continue;
            }
            if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                index.insert(name.to_lowercase(), path.to_path_buf());
            }
        }
    }
    index
}

/// Returns the app name if `transcription` is a launch command (trigger word first, 2-4 words).
fn parse_command(transcription: &str, triggers: &[String]) -> Option<String> {
    let cleaned: String = transcription
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '+' || c == '#' { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.len() < 2 || words.len() > 4 || !triggers.iter().any(|t| t.trim().eq_ignore_ascii_case(words[0])) {
        return None;
    }
    Some(words[1..].join(" "))
}

fn best_match<'a>(target: &str, index: &'a HashMap<String, PathBuf>) -> Option<&'a PathBuf> {
    if let Some(path) = index.get(target) {
        return Some(path);
    }
    let mut best = None;
    let mut best_score = MATCH_THRESHOLD;
    for (name, path) in index {
        let score = jaro_winkler(target, name);
        if score > best_score {
            best_score = score;
            best = Some(path);
        }
    }
    best
}

/// Launches the matching app and returns true when the utterance was consumed as a command.
pub fn evaluate_and_launch(transcription: &str, config: &VoiceCommandConfig) -> bool {
    if !config.enabled {
        return false;
    }
    let Some(target) = parse_command(transcription, &config.triggers) else {
        return false;
    };
    let wanted = normalize_name(&target);
    if let Some(alias_target) = config
        .aliases
        .iter()
        .find(|(spoken, _)| normalize_name(spoken) == wanted)
        .map(|(_, path)| path.trim())
    {
        return spawn(alias_target, &target);
    }
    let Some(index) = APP_INDEX.get() else {
        return false;
    };
    let Some(path) = best_match(&target, index) else {
        return false;
    };
    spawn(&path.to_string_lossy(), &target)
}

/// Drops filler words so "open my projects" matches an alias saved as "projects".
fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .split_whitespace()
        .filter(|word| !matches!(*word, "my" | "the"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_direct_executable(path: &str) -> bool {
    path.to_lowercase().ends_with(".exe") && std::path::Path::new(path).is_file()
}

fn spawn(path: &str, spoken: &str) -> bool {
    let result = if is_direct_executable(path) {
        Command::new(path).spawn()
    } else {
        // Folders, documents, shortcuts and links are opened by the shell.
        Command::new("explorer.exe").arg(path).spawn()
    };
    match result {
        Ok(_) => {
            tracing::info!(app = %spoken, path = %path, "Voice command opened target");
            true
        }
        Err(error) => {
            tracing::warn!(error = %error, "Voice launch failed");
            false
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anchored_short_commands() {
        let t = VoiceCommandConfig::default().triggers;
        assert_eq!(parse_command("Open MS Word.", &t), Some("ms word".into()));
        assert_eq!(parse_command("open", &t), None);
        assert_eq!(parse_command("please open word", &t), None);
        assert_eq!(parse_command("open the door and then leave", &t), None);
    }

    #[test]
    fn alias_names_ignore_filler_words() {
        assert_eq!(normalize_name("My Projects"), normalize_name("projects"));
        assert!(!is_direct_executable("D:\\Projects"));
    }

    #[test]
    fn fuzzy_matches_index() {
        let mut index = HashMap::new();
        index.insert("word".to_string(), PathBuf::from("Word.lnk"));
        index.insert("excel".to_string(), PathBuf::from("Excel.lnk"));
        assert_eq!(best_match("word", &index), Some(&PathBuf::from("Word.lnk")));
        assert_eq!(best_match("xyz", &index), None);
    }
}
