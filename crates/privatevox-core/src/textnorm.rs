//! Rule-based transcript clean-up that needs no LLM (design §18.9).

fn key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Collapses three or more consecutive repeats of a word (case-insensitive) into one.
pub fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let k = key(words[i]);
        let mut j = i + 1;
        while j < words.len() && !k.is_empty() && key(words[j]) == k {
            j += 1;
        }
        let run = j - i;
        if run >= 3 {
            out.push(words[i]);
        } else {
            out.extend_from_slice(&words[i..j]);
        }
        i = j;
    }
    out.join(" ")
}

/// True when the same word appears `n` or more times in a row: the failure mode of a too-small audio context.
pub fn has_repetition_loop(text: &str, n: usize) -> bool {
    let keys: Vec<String> = text.split_whitespace().map(key).filter(|k| !k.is_empty()).collect();
    let mut run = 1;
    for w in keys.windows(2) {
        run = if w[0] == w[1] { run + 1 } else { 1 };
        if run >= n {
            return true;
        }
    }
    false
}

/// Fillers removed in every language.
const UNIVERSAL_FILLERS: &[&str] = &["uh", "uhm", "uhh", "umm", "hmm", "mhm", "mm", "erm", "err"];
/// Fillers that are real words elsewhere ("um" in German and Portuguese), so only removed when the text is English.
const ENGLISH_FILLERS: &[&str] = &["um", "ah", "eh"];

fn is_filler(token: &str, english: bool) -> bool {
    let core = token.trim_end_matches(|c| c == ',' || c == '.').to_lowercase();
    if core.chars().any(|c| !c.is_alphabetic()) || core.is_empty() {
        return false;
    }
    UNIVERSAL_FILLERS.contains(&core.as_str()) || (english && ENGLISH_FILLERS.contains(&core.as_str()))
}

fn ends_sentence(token: &str) -> bool {
    token.ends_with('.') || token.ends_with('!') || token.ends_with('?')
}

fn capitalize_first(token: &str) -> String {
    let mut c = token.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Removes filler words (design §18.9). Whole words only; a trailing comma or period on the filler goes with it.
/// When a capitalised filler opened a sentence, the next word gets the capital back.
pub fn remove_fillers(text: &str, english: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut restore_capital = false;
    for token in text.split_whitespace() {
        if is_filler(token, english) {
            // The filler owed its capital to the next word only when it had one ("Um, we go" → "We go").
            let at_sentence_start = out.last().map(|t| ends_sentence(t)).unwrap_or(true);
            let had_capital = token.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
            restore_capital = restore_capital || (at_sentence_start && had_capital);
            continue;
        }
        if restore_capital {
            out.push(capitalize_first(token));
            restore_capital = false;
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// Stutter collapse, optional filler removal, whitespace collapse, trim.
pub fn normalize_with(text: &str, english: bool, fillers: bool) -> String {
    let collapsed = collapse_stutters(text);
    let cleaned = if fillers { remove_fillers(&collapsed, english) } else { collapsed };
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Stutter collapse, whitespace collapse, trim (no filler removal).
pub fn normalize(text: &str) -> String {
    normalize_with(text, true, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_three_or_more_repeats() {
        assert_eq!(collapse_stutters("I I I think"), "I think");
        assert_eq!(collapse_stutters("no no"), "no no");
        assert_eq!(collapse_stutters("the The THE the end"), "the end");
        assert_eq!(collapse_stutters("wh wh wh wh what"), "wh what");
    }

    #[test]
    fn repetition_loop_detected_at_four() {
        assert!(has_repetition_loop("Hello. Hello. Hello. Hello.", 4));
        assert!(!has_repetition_loop("Hello. Hello.", 4));
        assert!(!has_repetition_loop("", 4));
    }

    #[test]
    fn normalize_trims_and_collapses_spaces() {
        assert_eq!(normalize("  hello   world \n"), "hello world");
        assert_eq!(normalize("I I I am  here"), "I am here");
    }

    #[test]
    fn removes_universal_fillers() {
        assert_eq!(remove_fillers("so uh I think, hmm, we go", false), "so I think, we go");
    }

    #[test]
    fn english_only_fillers_kept_in_other_languages() {
        assert_eq!(remove_fillers("um ein Haus", false), "um ein Haus");
        assert_eq!(remove_fillers("um a house", true), "a house");
    }

    #[test]
    fn restores_sentence_opening_capital() {
        assert_eq!(remove_fillers("Um, we should go. Uh, now.", true), "We should go. Now.");
    }

    #[test]
    fn filler_inside_word_is_untouched() {
        assert_eq!(remove_fillers("the album hummed", true), "the album hummed");
    }

    #[test]
    fn normalize_with_runs_stutters_then_fillers() {
        assert_eq!(normalize_with("I I I um think", true, true), "I think");
    }

    #[test]
    fn settings_default_remove_fillers_on() {
        assert!(crate::settings::Settings::default().remove_fillers);
    }
}
