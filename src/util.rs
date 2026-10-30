//! Small text helpers shared by the providers.

use std::sync::LazyLock;

use regex::Regex;

/// Up to `n` candidates similar to `word`, best first (like Python's `difflib.get_close_matches`).
pub fn close_matches<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>, n: usize, cutoff: f64) -> Vec<&'a str> {
    let mut scored: Vec<(f64, &str)> = candidates
        .into_iter()
        .map(|c| (strsim::normalized_levenshtein(word, c).max(strsim::sorensen_dice(word, c)), c))
        .filter(|(score, _)| *score >= cutoff)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(n).map(|(_, c)| c).collect()
}

static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s<>\)\]"'`*]+"#).unwrap());

/// Every distinct link in `texts`, in order, without trailing punctuation.
pub fn links<'a>(texts: impl IntoIterator<Item = Option<&'a str>>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in texts.into_iter().flatten() {
        for m in LINK.find_iter(text) {
            let link = m.as_str().trim_end_matches(['.', ',', ';', ':']).to_string();
            if !out.contains(&link) {
                out.push(link);
            }
        }
    }
    out
}

/// Keep the first occurrence of each item.
pub fn dedupe<T: PartialEq>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Python's `str.capitalize`.
pub fn capitalize(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Python's `str.title` for simple words.
pub fn title(text: &str) -> String {
    text.split(' ').map(capitalize).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_and_links() {
        assert_eq!(close_matches("uniswep", ["uniswap", "aave", "unicorn"], 3, 0.6), vec!["uniswap"]);
        assert_eq!(
            links([Some("see https://a.io/x. and https://a.io/x"), None, Some("(https://b.io)")]),
            vec!["https://a.io/x", "https://b.io"]
        );
        assert_eq!(capitalize("hIGH"), "High");
    }
}
