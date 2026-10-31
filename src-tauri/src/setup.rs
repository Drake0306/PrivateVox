//! First-run walkthrough (design §4.8): the Setup tab as numbered steps, each with a state, so a new user sees what
//! is left to do in order. Pure: the command layer gathers the inputs, this module decides the steps.

use privatevox_core::settings::Settings;
use serde::Serialize;

/// What the command layer knows about the machine right now.
pub struct SetupInputs<'a> {
    pub model_present: bool,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub settings: &'a Settings,
    /// Polish master switch and the label of the endpoint that would be used, when one is reachable.
    pub llm_enabled: bool,
    pub llm_selected: Option<String>,
    /// Dictations in the local history; the first successful one proves the microphone and the hotkey.
    pub history_count: u32,
    /// `std::env::consts::OS`: macOS has two extra permission steps.
    pub os: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Done,
    Todo,
    Optional,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetupStep {
    /// `model`, `microphone`, `accessibility`, `input_monitoring`, `hotkey`, `polish`, `say_something`.
    pub id: &'static str,
    pub label: String,
    pub state: StepState,
    pub detail: String,
    /// What the panel offers for the step: `download`, `mic_test`, `open_privacy`, `settings`, `polish`, `record`.
    pub action: Option<&'static str>,
}

/// The steps in order. macOS gets Accessibility and Input Monitoring; other platforms need no separate grants.
pub fn steps(i: &SetupInputs<'_>) -> Vec<SetupStep> {
    let mut out = Vec::new();
    let model = privatevox_core::models::info(&i.settings.model_id);
    let model_label = model.as_ref().map(|m| format!("{} ({} MB)", m.label, m.bytes / 1_000_000)).unwrap_or_else(|| i.settings.model_id.clone());
    out.push(SetupStep {
        id: "model",
        label: "Whisper model".into(),
        state: if i.model_present { StepState::Done } else { StepState::Todo },
        detail: if i.model_present { format!("{model_label} is on this machine.") } else { format!("Download {model_label}; it runs on your GPU, nothing leaves the machine.") },
        action: if i.model_present { None } else { Some("download") },
    });
    out.push(SetupStep {
        id: "microphone",
        label: "Microphone".into(),
        state: if i.history_count > 0 { StepState::Done } else { StepState::Todo },
        detail: match (&i.settings.microphone, i.history_count > 0) {
            (_, true) => "Heard you: a dictation went through.".into(),
            (Some(name), false) => format!("Using {name}. Press Test and speak; the meter should move."),
            (None, false) => "Using the system default input. Press Test and speak; the meter should move.".into(),
        },
        action: Some("mic_test"),
    });
    if i.os == "macos" {
        out.push(SetupStep {
            id: "accessibility",
            label: "Accessibility".into(),
            state: if i.accessibility { StepState::Done } else { StepState::Todo },
            detail: if i.accessibility { "Granted: PrivateVox can paste into the app in front.".into() } else { "Needed to send ⌘V into the app in front. Tick PrivateVox under Privacy & Security → Accessibility, then quit and start it again.".into() },
            action: if i.accessibility { None } else { Some("open_privacy") },
        });
        out.push(SetupStep {
            id: "input_monitoring",
            label: "Input Monitoring".into(),
            state: if i.input_monitoring { StepState::Done } else { StepState::Todo },
            detail: if i.input_monitoring { "Granted: the hold key is seen in every app.".into() } else { "Needed to see the hold key (Right ⌥) in other apps. Tick PrivateVox under Privacy & Security → Input Monitoring, then quit and start it again.".into() },
            action: if i.input_monitoring { None } else { Some("open_privacy") },
        });
    }
    out.push(SetupStep {
        id: "hotkey",
        label: "Dictation key".into(),
        state: StepState::Done,
        detail: format!("{}. Change the key or the activation mode in Settings.", crate::pipeline::hotkey_hint(i.settings)),
        action: Some("settings"),
    });
    out.push(SetupStep {
        id: "polish",
        label: "Polish LLM".into(),
        state: match (i.llm_enabled, &i.llm_selected) {
            (true, Some(_)) => StepState::Done,
            (true, None) => StepState::Todo,
            (false, _) => StepState::Optional,
        },
        detail: match (i.llm_enabled, &i.llm_selected) {
            (true, Some(l)) => format!("{l} cleans up the text before it is pasted."),
            (true, None) => "Polish is on but no LLM answers. Install Ollama and run `ollama pull qwen2.5:3b`, or add the PC's address under Polish.".into(),
            (false, _) => "Off: Whisper's text is pasted as it is. Turn it on under Polish when you want punctuation and clean-up.".into(),
        },
        action: Some("polish"),
    });
    out.push(SetupStep {
        id: "say_something",
        label: "Say something".into(),
        state: if i.history_count > 0 { StepState::Done } else { StepState::Todo },
        detail: if i.history_count > 0 { format!("{} dictation{} in History.", i.history_count, if i.history_count == 1 { "" } else { "s" }) } else { "Press Record, speak a sentence, press Stop: the text lands in History (and in the app in front).".into() },
        action: Some("record"),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(settings: &Settings) -> SetupInputs<'_> {
        SetupInputs { model_present: false, accessibility: false, input_monitoring: false, settings, llm_enabled: false, llm_selected: None, history_count: 0, os: "macos" }
    }

    fn state_of(steps: &[SetupStep], id: &str) -> StepState {
        steps.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no step {id}")).state
    }

    #[test]
    fn steps_mark_done_and_todo() {
        let s = Settings::default();
        // Nothing done yet on a Mac.
        let fresh = steps(&inputs(&s));
        assert_eq!(fresh.iter().map(|x| x.id).collect::<Vec<_>>(), vec!["model", "microphone", "accessibility", "input_monitoring", "hotkey", "polish", "say_something"]);
        assert_eq!(state_of(&fresh, "model"), StepState::Todo);
        assert_eq!(state_of(&fresh, "accessibility"), StepState::Todo);
        assert_eq!(state_of(&fresh, "input_monitoring"), StepState::Todo);
        assert_eq!(state_of(&fresh, "polish"), StepState::Optional, "polish off is a choice, not a gap");
        assert_eq!(state_of(&fresh, "hotkey"), StepState::Done, "a default key is always set");
        assert_eq!(state_of(&fresh, "say_something"), StepState::Todo);
        assert!(fresh.iter().find(|x| x.id == "model").unwrap().detail.contains("large-v3-turbo"), "names the configured model");
        assert!(fresh.iter().find(|x| x.id == "hotkey").unwrap().detail.contains("right ⌥"), "shows the current key");

        // Everything granted, model present, LLM reachable, one dictation done.
        let all = steps(&SetupInputs { model_present: true, accessibility: true, input_monitoring: true, llm_enabled: true, llm_selected: Some("Ollama · qwen2.5:3b".into()), history_count: 1, ..inputs(&s) });
        assert!(all.iter().all(|x| x.state == StepState::Done), "{all:?}");
        assert!(all.iter().find(|x| x.id == "polish").unwrap().detail.contains("qwen2.5:3b"));

        // Polish on but nothing answers: that is a gap to fix.
        let no_llm = steps(&SetupInputs { llm_enabled: true, ..inputs(&s) });
        assert_eq!(state_of(&no_llm, "polish"), StepState::Todo);
        assert!(no_llm.iter().find(|x| x.id == "polish").unwrap().detail.contains("Ollama"));
    }

    #[test]
    fn windows_and_linux_skip_the_macos_permissions() {
        let s = Settings::default();
        for os in ["windows", "linux"] {
            let st = steps(&SetupInputs { os, ..inputs(&s) });
            assert_eq!(st.iter().map(|x| x.id).collect::<Vec<_>>(), vec!["model", "microphone", "hotkey", "polish", "say_something"], "{os}");
        }
    }

    #[test]
    fn microphone_step_names_the_chosen_device() {
        let mut s = Settings::default();
        s.microphone = Some("Yeti Nano".into());
        let st = steps(&inputs(&s));
        let mic = st.iter().find(|x| x.id == "microphone").unwrap();
        assert!(mic.detail.contains("Yeti Nano"));
        assert_eq!(mic.action, Some("mic_test"));
    }
}
