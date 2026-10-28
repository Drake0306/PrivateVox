//! Per-app default modes: Mail → Email, VS Code → Code, from a small table (phase 5, A4).

use crate::polish::Mode;
use serde::{Deserialize, Serialize};

/// One table row: when the app in front contains `app` (case-insensitive), start in `mode`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppMode {
    pub app: String,
    pub mode: Mode,
}

/// The first rule whose pattern is a case-insensitive substring of the app name wins; no match means `default`.
pub fn resolve(rules: &[AppMode], app_name: Option<&str>, default: Mode) -> Mode {
    let Some(name) = app_name.map(|n| n.to_lowercase()) else { return default };
    rules
        .iter()
        .filter(|r| !r.app.trim().is_empty())
        .find(|r| name.contains(&r.app.trim().to_lowercase()))
        .map(|r| r.mode)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polish::Mode;

    fn rules() -> Vec<AppMode> {
        vec![AppMode { app: "mail".into(), mode: Mode::Email }, AppMode { app: "Code".into(), mode: Mode::Code }, AppMode { app: "Terminal".into(), mode: Mode::Code }]
    }

    #[test]
    fn resolve_matches_substring_case_insensitively() {
        assert_eq!(resolve(&rules(), Some("Mail"), Mode::Clean), Mode::Email);
        assert_eq!(resolve(&rules(), Some("Visual Studio Code"), Mode::Clean), Mode::Code);
        assert_eq!(resolve(&rules(), Some("terminal"), Mode::Clean), Mode::Code);
        assert_eq!(resolve(&rules(), Some("Safari"), Mode::Clean), Mode::Clean);
    }

    #[test]
    fn no_rule_means_default_and_first_match_wins() {
        assert_eq!(resolve(&[], Some("Mail"), Mode::Bullets), Mode::Bullets);
        assert_eq!(resolve(&rules(), None, Mode::Raw), Mode::Raw);
        let two = vec![AppMode { app: "code".into(), mode: Mode::Code }, AppMode { app: "xcode".into(), mode: Mode::Ask }];
        assert_eq!(resolve(&two, Some("Xcode"), Mode::Clean), Mode::Code, "first rule in the table wins");
        let blank = vec![AppMode { app: "   ".into(), mode: Mode::Ask }];
        assert_eq!(resolve(&blank, Some("Anything"), Mode::Clean), Mode::Clean, "a blank pattern never matches");
    }

    #[test]
    fn settings_default_has_no_rules() {
        assert!(crate::settings::Settings::default().app_modes.is_empty());
    }
}
