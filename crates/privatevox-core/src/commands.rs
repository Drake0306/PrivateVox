//! Spoken end-of-dictation commands (phase 5, A3): only the trailing phrase of a dictation is checked.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenCommand {
    NewLine,
    NewParagraph,
    DeleteThat,
    Undo,
    Cancel,
}

impl SpokenCommand {
    /// Drops or undoes text, so it is only taken after a sentence break (or alone).
    pub fn is_destructive(self) -> bool {
        matches!(self, SpokenCommand::DeleteThat | SpokenCommand::Undo | SpokenCommand::Cancel)
    }
}

/// Phrases, longest first so "undo that" wins over "undo".
const PHRASES: &[(&str, SpokenCommand)] = &[
    ("new paragraph", SpokenCommand::NewParagraph),
    ("new line", SpokenCommand::NewLine),
    ("newline", SpokenCommand::NewLine),
    ("delete that", SpokenCommand::DeleteThat),
    ("scratch that", SpokenCommand::DeleteThat),
    ("undo that", SpokenCommand::Undo),
    ("undo", SpokenCommand::Undo),
    ("never mind", SpokenCommand::Cancel),
    ("nevermind", SpokenCommand::Cancel),
    ("cancel", SpokenCommand::Cancel),
];

fn core(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Split off a trailing command phrase (one or two words, any case, trailing punctuation ignored).
pub fn strip_trailing(text: &str) -> (String, Option<SpokenCommand>) {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return (String::new(), None);
    }
    for (phrase, cmd) in PHRASES {
        let n = phrase.split_whitespace().count();
        if words.len() < n {
            continue;
        }
        let tail: Vec<String> = words[words.len() - n..].iter().map(|w| core(w)).collect();
        if tail.join(" ") == *phrase {
            let rest = words[..words.len() - n].join(" ");
            // "tell him I will cancel." is text; "Send the report. Cancel." is a command. Breaks are meant mid-sentence.
            if cmd.is_destructive() && !rest.is_empty() && !rest.ends_with(['.', '!', '?']) {
                return (text.to_string(), None);
            }
            return (rest, Some(*cmd));
        }
    }
    (text.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_each_phrase() {
        assert_eq!(strip_trailing("send the report new line"), ("send the report".into(), Some(SpokenCommand::NewLine)));
        assert_eq!(strip_trailing("send the report, newline."), ("send the report,".into(), Some(SpokenCommand::NewLine)));
        assert_eq!(strip_trailing("first point new paragraph"), ("first point".into(), Some(SpokenCommand::NewParagraph)));
        assert_eq!(strip_trailing("that was wrong. delete that"), ("that was wrong.".into(), Some(SpokenCommand::DeleteThat)));
        assert_eq!(strip_trailing("that was wrong! scratch that"), ("that was wrong!".into(), Some(SpokenCommand::DeleteThat)));
        assert_eq!(strip_trailing("hmm. undo"), ("hmm.".into(), Some(SpokenCommand::Undo)));
        assert_eq!(strip_trailing("hmm? undo that"), ("hmm?".into(), Some(SpokenCommand::Undo)));
        assert_eq!(strip_trailing("no wait. cancel"), ("no wait.".into(), Some(SpokenCommand::Cancel)));
        assert_eq!(strip_trailing("no wait. never mind."), ("no wait.".into(), Some(SpokenCommand::Cancel)));
        assert_eq!(strip_trailing("Please do it. New Line"), ("Please do it.".into(), Some(SpokenCommand::NewLine)));
    }

    #[test]
    fn plain_text_is_untouched() {
        for t in ["the new line of products is out", "please cancel the meeting", "I will undo the change later", "a new paragraph starts here"] {
            assert_eq!(strip_trailing(t), (t.to_string(), None), "{t}");
        }
        assert_eq!(strip_trailing(""), (String::new(), None));
    }

    #[test]
    fn phrase_alone_is_empty() {
        assert_eq!(strip_trailing("cancel"), (String::new(), Some(SpokenCommand::Cancel)));
        assert_eq!(strip_trailing("Delete that."), (String::new(), Some(SpokenCommand::DeleteThat)));
        assert!(crate::settings::Settings::default().spoken_commands);
    }

    #[test]
    fn destructive_commands_need_a_sentence_break() {
        // A sentence that merely ends in "cancel" or "undo" is text, not a command: nothing is lost and no ⌘Z is sent.
        for t in ["tell him I will cancel.", "I don't know what to undo", "we might never mind", "the thing to delete that"] {
            assert_eq!(strip_trailing(t), (t.to_string(), None), "{t}");
        }
        assert_eq!(strip_trailing("Send the report. Cancel."), ("Send the report.".into(), Some(SpokenCommand::Cancel)));
        // Line breaks are meant mid-sentence, so they need no break before them.
        assert_eq!(strip_trailing("the first item new line"), ("the first item".into(), Some(SpokenCommand::NewLine)));
    }
}
