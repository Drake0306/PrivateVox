//! Personal dictionary: names and jargon fed to Whisper as its prompt, with a fuzzy safety net for near-misses (design §18.8).

/// Whisper's prompt window is small; more than this many words stops helping.
const MAX_PROMPT_WORDS: usize = 150;
/// Tokens shorter than this are never rewritten: too many real words are within one edit of each other.
const MIN_TOKEN_LEN: usize = 4;

/// The prompt Whisper sees before the audio: the words as a plain list, which biases their spelling.
pub fn initial_prompt(words: &[String]) -> Option<String> {
    let clean: Vec<&str> = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).take(MAX_PROMPT_WORDS).collect();
    if clean.is_empty() {
        None
    } else {
        Some(clean.join(", "))
    }
}

struct Token<'a> {
    prefix: &'a str,
    core: &'a str,
    suffix: &'a str,
}

fn split_token(t: &str) -> Token<'_> {
    let start = t.find(|c: char| c.is_alphanumeric()).unwrap_or(t.len());
    let end = t.rfind(|c: char| c.is_alphanumeric()).map(|i| i + t[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1)).unwrap_or(start);
    if start >= end {
        return Token { prefix: t, core: "", suffix: "" };
    }
    Token { prefix: &t[..start], core: &t[start..end], suffix: &t[end..] }
}

/// Carry the token's case pattern onto the dictionary spelling: ALL CAPS stays caps, Capitalised stays capitalised,
/// anything else takes the dictionary's own spelling.
fn with_case_of(original: &str, word: &str) -> String {
    let letters: Vec<char> = original.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() > 1 && letters.iter().all(|c| c.is_uppercase()) {
        word.to_uppercase()
    } else if letters.first().map(|c| c.is_uppercase()).unwrap_or(false) && letters.iter().skip(1).all(|c| c.is_lowercase()) {
        let mut c = word.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        }
    } else {
        word.to_string()
    }
}

fn close_enough(candidate: &str, word: &str, strictness: f32) -> bool {
    if strictness <= 0.0 || candidate.is_empty() {
        return false;
    }
    let (a, b) = (candidate.to_lowercase(), word.to_lowercase());
    if a == b {
        return false; // exact: nothing to fix
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la.abs_diff(lb) > 2 {
        return false;
    }
    let dist = strsim::levenshtein(&a, &b) as f32;
    dist / la.max(lb) as f32 <= strictness
}

/// A split word ("Charge B") matches when the join is close, or is a clear prefix of the dictionary word.
fn join_matches(joined: &str, word: &str, strictness: f32) -> bool {
    let (a, b) = (joined.to_lowercase(), word.to_lowercase());
    if a == b {
        return true;
    }
    if close_enough(joined, word, strictness) {
        return true;
    }
    b.starts_with(&a) && a.chars().count() * 10 >= b.chars().count() * 7
}

/// Fix near-misses of dictionary words in `text`. Exact matches, short tokens and words of a different length class
/// are left alone; punctuation around a token is kept; two adjacent tokens may be joined into one dictionary word.
pub fn correct(text: &str, words: &[String], strictness: f32) -> String {
    let words: Vec<&str> = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()).collect();
    if words.is_empty() || strictness <= 0.0 {
        return text.to_string();
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let t = split_token(tokens[i]);
        // Joined pair first ("Charge B" → "ChargeBee"), only when both halves are plain alphabetic pieces.
        if i + 1 < tokens.len() && !t.core.is_empty() && t.suffix.is_empty() {
            let n = split_token(tokens[i + 1]);
            if !n.core.is_empty() && n.prefix.is_empty() && t.core.chars().all(|c| c.is_alphabetic()) && n.core.chars().all(|c| c.is_alphabetic()) {
                let joined = format!("{}{}", t.core, n.core);
                if let Some(w) = words.iter().find(|w| join_matches(&joined, w, strictness)) {
                    out.push(format!("{}{}{}", t.prefix, with_case_of(&joined, w), n.suffix));
                    i += 2;
                    continue;
                }
            }
        }
        if t.core.chars().count() >= MIN_TOKEN_LEN {
            if let Some(w) = words.iter().find(|w| close_enough(t.core, w, strictness)) {
                out.push(format!("{}{}{}", t.prefix, with_case_of(t.core, w), t.suffix));
                i += 1;
                continue;
            }
        }
        out.push(tokens[i].to_string());
        i += 1;
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prompt_is_none_when_empty() {
        assert_eq!(initial_prompt(&[]), None);
        assert_eq!(initial_prompt(&w(&["  ", ""])), None);
    }

    #[test]
    fn prompt_joins_words() {
        assert_eq!(initial_prompt(&w(&["ChargeBee", "Roy", "Kubernetes"])).as_deref(), Some("ChargeBee, Roy, Kubernetes"));
        let many: Vec<String> = (0..400).map(|i| format!("word{i}")).collect();
        let p = initial_prompt(&many).unwrap();
        assert!(p.split(", ").count() <= 150, "capped at 150 words");
    }

    #[test]
    fn fuzzy_fixes_near_misses() {
        assert_eq!(correct("I used Charge B and Kuber netes today", &w(&["ChargeBee", "Kubernetes"]), 0.18), "I used ChargeBee and Kubernetes today");
        assert_eq!(correct("talk to chargebe about it", &w(&["ChargeBee"]), 0.18), "talk to ChargeBee about it");
        assert_eq!(correct("the kubernets cluster", &w(&["Kubernetes"]), 0.18), "the Kubernetes cluster");
    }

    #[test]
    fn fuzzy_pass_leaves_longer_words_alone() {
        assert_eq!(correct("the royal road", &w(&["Roy"]), 0.18), "the royal road");
        assert_eq!(correct("roy is here", &w(&["Roy"]), 0.18), "roy is here", "short tokens are never rewritten");
        assert_eq!(correct("charging ahead", &w(&["ChargeBee"]), 0.18), "charging ahead");
    }

    #[test]
    fn case_pattern_preserved() {
        assert_eq!(correct("CHARGEBE rocks", &w(&["ChargeBee"]), 0.18), "CHARGEBEE rocks");
        assert_eq!(correct("Chargebe rocks", &w(&["chargebee"]), 0.18), "Chargebee rocks");
        assert_eq!(correct("chargebe rocks", &w(&["ChargeBee"]), 0.18), "ChargeBee rocks");
    }

    #[test]
    fn punctuation_kept() {
        assert_eq!(correct("Ask chargebe, then kubernets.", &w(&["ChargeBee", "Kubernetes"]), 0.18), "Ask ChargeBee, then Kubernetes.");
    }

    #[test]
    fn exact_words_untouched_and_strictness_zero_is_exact_only() {
        assert_eq!(correct("ChargeBee works", &w(&["ChargeBee"]), 0.18), "ChargeBee works");
        assert_eq!(correct("chargebe works", &w(&["ChargeBee"]), 0.0), "chargebe works");
    }

    #[test]
    fn settings_defaults() {
        let s = crate::settings::Settings::default();
        assert!(s.dictionary.is_empty());
        assert!((s.dictionary_strictness - 0.18).abs() < 1e-6);
    }
}
