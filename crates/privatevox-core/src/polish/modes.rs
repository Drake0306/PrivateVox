//! Polish modes and their default prompts (design §7).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Raw,
    #[default]
    Clean,
    Email,
    Bullets,
    Code,
    Translate,
    Ask,
}

impl Mode {
    pub const ALL: [Mode; 7] = [Mode::Raw, Mode::Clean, Mode::Email, Mode::Bullets, Mode::Code, Mode::Translate, Mode::Ask];

    pub fn id(self) -> &'static str {
        match self {
            Mode::Raw => "raw",
            Mode::Clean => "clean",
            Mode::Email => "email",
            Mode::Bullets => "bullets",
            Mode::Code => "code",
            Mode::Translate => "translate",
            Mode::Ask => "ask",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Raw => "Raw",
            Mode::Clean => "Clean",
            Mode::Email => "Email",
            Mode::Bullets => "Bullets",
            Mode::Code => "Code",
            Mode::Translate => "Translate to English",
            Mode::Ask => "Ask",
        }
    }

    /// Raw and Translate paste Whisper's output; every other mode goes through the LLM.
    pub fn uses_llm(self) -> bool {
        !matches!(self, Mode::Raw | Mode::Translate)
    }

    /// Translate uses Whisper's own translate-to-English pass.
    pub fn whisper_translate(self) -> bool {
        matches!(self, Mode::Translate)
    }

    pub fn parse(id: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.id() == id)
    }
}

/// Shared tail of every LLM prompt: the rules that keep the model from inventing or decorating.
pub const RULES: &str = "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise.";

pub fn default_prompt(mode: Mode) -> &'static str {
    match mode {
        Mode::Raw | Mode::Translate => "",
        Mode::Clean => concat!(
            "You clean up dictated speech. Remove filler words, false starts and repeated words, fix punctuation and capitalisation, and split run-on sentences. Do not rephrase, shorten or summarise. ",
            "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
        ),
        Mode::Email => concat!(
            "You turn dictated speech into a short, polite email body. Clean up fillers and grammar, keep the speaker's tone, add a greeting and sign-off only if the speaker dictated them. No subject line. ",
            "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
        ),
        Mode::Bullets => concat!(
            "You turn dictated speech into a bulleted list. One bullet per distinct point, in the order spoken, starting each line with \"- \". Keep each bullet short and in the speaker's words. ",
            "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
        ),
        Mode::Code => concat!(
            "You turn dictated speech into code or a terminal command. Infer the language from context when the speaker names one; otherwise use the most likely one and keep it minimal. Spoken punctuation words (\"open paren\", \"semicolon\") become symbols. Reply with the code only. ",
            "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
        ),
        Mode::Ask => concat!(
            "The speaker is asking you a question. Answer it directly and briefly, in plain prose, as text that can be pasted into a document. ",
            "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
        ),
    }
}

/// The prompt to send: the user's override when it is non-empty, else the default.
pub fn effective_prompt(mode: Mode, overrides: &HashMap<String, String>) -> String {
    match overrides.get(mode.id()).map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(custom) => custom.to_string(),
        None => default_prompt(mode).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn ids_round_trip() {
        for m in Mode::ALL {
            assert_eq!(Mode::parse(m.id()), Some(m));
        }
    }

    #[test]
    fn raw_and_translate_skip_the_llm() {
        assert!(!Mode::Raw.uses_llm());
        assert!(!Mode::Translate.uses_llm());
        assert!(Mode::Clean.uses_llm());
        assert!(Mode::Translate.whisper_translate());
        assert!(!Mode::Clean.whisper_translate());
    }

    #[test]
    fn every_llm_prompt_carries_the_rules() {
        for m in Mode::ALL.into_iter().filter(|m| m.uses_llm()) {
            assert!(default_prompt(m).contains("Reply with only the text to paste"), "{m:?}");
        }
        assert!(default_prompt(Mode::Raw).is_empty());
    }

    #[test]
    fn override_wins_when_non_empty() {
        let mut o = HashMap::new();
        o.insert("clean".to_string(), "custom".to_string());
        assert_eq!(effective_prompt(Mode::Clean, &o), "custom");
        o.insert("clean".into(), "".into());
        assert_eq!(effective_prompt(Mode::Clean, &o), default_prompt(Mode::Clean));
    }

    #[test]
    fn settings_default_mode_is_clean() {
        let s = crate::settings::Settings::default();
        assert_eq!(s.mode, Mode::Clean);
        assert_eq!(s.llm_timeout_secs, 8);
        assert!(s.polish_enabled);
        assert!(s.mode_prompts.is_empty());
    }

    #[test]
    fn mode_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&Mode::Clean).unwrap(), "\"clean\"");
    }
}
