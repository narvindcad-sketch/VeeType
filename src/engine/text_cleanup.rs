//! Fast, model-independent transcript cleanup.
//!
//! Inspired by Handy's MIT-licensed language-aware filler filtering. This
//! conservative English path runs even when no polishing LLM is installed.
use regex::Regex;
use std::sync::LazyLock;

static FILLER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:uh+|uhm+|umm+|hmm+|hm+|mmm+|um|ah|eh)\b[,.]?").unwrap()
});
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[ \t]{2,}").unwrap());
static SPACE_BEFORE_PUNCTUATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+([,.;:!?])").unwrap());

pub fn clean_english_transcript(text: &str) -> String {
    let mut output = FILLER_PATTERN.replace_all(text, "").into_owned();
    output = collapse_stutters(&output);
    output = MULTI_SPACE.replace_all(&output, " ").into_owned();
    output = SPACE_BEFORE_PUNCTUATION.replace_all(&output, "$1").into_owned();
    output = output.trim().to_string();
    if let Some(first) = output.chars().next() {
        if first.is_lowercase() {
            let mut capitalized = first.to_uppercase().collect::<String>();
            capitalized.push_str(&output[first.len_utf8()..]);
            output = capitalized;
        }
    }
    output
}

fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut result = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        let normalized = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
        let mut count = 1;
        while index + count < words.len()
            && words[index + count]
                .trim_matches(|c: char| !c.is_alphanumeric())
                .eq_ignore_ascii_case(&normalized)
        {
            count += 1;
        }
        result.push(word);
        index += if count >= 3 { count } else { 1 };
    }
    result.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_common_fillers_without_an_llm() {
        assert_eq!(
            clean_english_transcript("Uh, hello hmm this is, um, a microphone test."),
            "Hello this is, a microphone test."
        );
    }

    #[test]
    fn collapses_stutters_but_keeps_normal_double_words() {
        assert_eq!(clean_english_transcript("check check check this this"), "Check this this");
    }

    #[test]
    fn does_not_remove_fillers_inside_words() {
        assert_eq!(clean_english_transcript("A humming hummingbird."), "A humming hummingbird.");
    }
}
