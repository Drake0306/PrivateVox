//! Paste method per app (design §4.3, §4.5): a terminal or a remote desktop that ignores ⌘V / Ctrl+V gets the
//! text typed instead, from a small table like the per-app modes. Resolved from the app in front at paste time.

use crate::settings::PasteMethod;
use serde::{Deserialize, Serialize};

/// One table row: when the app in front contains `app` (case-insensitive), paste with `method`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppPaste {
    pub app: String,
    pub method: PasteMethod,
}

/// The first rule whose pattern is a case-insensitive substring of the app name wins; no match means `default`.
pub fn resolve(rules: &[AppPaste], app_name: Option<&str>, default: PasteMethod) -> PasteMethod {
    let Some(name) = app_name.map(|n| n.to_lowercase()) else { return default };
    rules
        .iter()
        .filter(|r| !r.app.trim().is_empty())
        .find(|r| name.contains(&r.app.trim().to_lowercase()))
        .map(|r| r.method)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<AppPaste> {
        vec![AppPaste { app: "terminal".into(), method: PasteMethod::Type }, AppPaste { app: "Remote Desktop".into(), method: PasteMethod::Type }, AppPaste { app: "Mail".into(), method: PasteMethod::Clipboard }]
    }

    #[test]
    fn resolve_matches_substring_first_wins() {
        assert_eq!(resolve(&rules(), Some("Terminal"), PasteMethod::Clipboard), PasteMethod::Type);
        assert_eq!(resolve(&rules(), Some("Windows Terminal"), PasteMethod::Clipboard), PasteMethod::Type, "substring, case-insensitive");
        assert_eq!(resolve(&rules(), Some("Microsoft Remote Desktop"), PasteMethod::Clipboard), PasteMethod::Type);
        assert_eq!(resolve(&rules(), Some("Safari"), PasteMethod::Clipboard), PasteMethod::Clipboard, "no rule: the default");
        assert_eq!(resolve(&rules(), Some("Mail"), PasteMethod::Type), PasteMethod::Clipboard, "a rule can also force the clipboard");
        assert_eq!(resolve(&rules(), None, PasteMethod::Type), PasteMethod::Type, "unknown app: the default");
        let two = vec![AppPaste { app: "code".into(), method: PasteMethod::Type }, AppPaste { app: "xcode".into(), method: PasteMethod::Clipboard }];
        assert_eq!(resolve(&two, Some("Xcode"), PasteMethod::Clipboard), PasteMethod::Type, "first rule in the table wins");
        let blank = vec![AppPaste { app: "  ".into(), method: PasteMethod::Type }];
        assert_eq!(resolve(&blank, Some("Anything"), PasteMethod::Clipboard), PasteMethod::Clipboard, "a blank pattern never matches");
    }

    #[test]
    fn settings_default_has_no_rules_and_old_files_load() {
        assert!(crate::settings::Settings::default().app_paste_methods.is_empty());
        let s: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert!(s.app_paste_methods.is_empty(), "a settings.json from before this field still loads");
        let s: crate::settings::Settings = serde_json::from_str(r#"{"app_paste_methods":[{"app":"Terminal","method":"type"}]}"#).unwrap();
        assert_eq!(s.app_paste_methods[0].method, PasteMethod::Type);
    }
}
