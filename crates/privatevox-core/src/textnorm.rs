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

/// Stutter collapse, whitespace collapse, trim.
pub fn normalize(text: &str) -> String {
    collapse_stutters(text).split_whitespace().collect::<Vec<_>>().join(" ")
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
}
