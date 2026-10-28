//! Live preview while speaking (phase 5, A1): when it is on, and the text window the pill shows.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Preview {
    /// On when a host does the decoding or this machine has 16 GB or more; off on small machines doing their own.
    #[default]
    Auto,
    On,
    Off,
}

const AUTO_MIN_BYTES: u64 = 16_000_000_000;

pub fn preview_enabled(setting: Preview, host_in_use: bool, total_bytes: u64) -> bool {
    match setting {
        Preview::On => true,
        Preview::Off => false,
        Preview::Auto => host_in_use || total_bytes >= AUTO_MIN_BYTES,
    }
}

/// Finished chunks plus the latest window, trimmed to the last `max_chars` characters with a leading ellipsis.
pub fn partial_text(finished: &[&str], window: &str, max_chars: usize) -> String {
    let mut parts: Vec<&str> = finished.iter().copied().filter(|t| !t.trim().is_empty()).collect();
    if !window.trim().is_empty() {
        parts.push(window.trim());
    }
    let joined = parts.join(" ");
    let n = joined.chars().count();
    if n <= max_chars {
        return joined;
    }
    let tail: String = joined.chars().skip(n - max_chars).collect();
    format!("…{}", tail.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_enabled_rules() {
        const GB: u64 = 1_000_000_000;
        assert!(!preview_enabled(Preview::Auto, false, 8 * GB), "auto: off on a small machine doing its own decoding");
        assert!(preview_enabled(Preview::Auto, true, 8 * GB), "auto: on when a host does the work");
        assert!(preview_enabled(Preview::Auto, false, 16 * GB), "auto: on with 16 GB or more");
        assert!(preview_enabled(Preview::On, false, 4 * GB));
        assert!(!preview_enabled(Preview::Off, true, 64 * GB));
        assert_eq!(crate::settings::Settings::default().live_preview, Preview::On, "owner's call 2026-10-29");
    }

    #[test]
    fn partial_window_keeps_the_tail() {
        assert_eq!(partial_text(&["first chunk here"], "and the window", 70), "first chunk here and the window");
        let long = "word ".repeat(40);
        let p = partial_text(&[], long.trim(), 20);
        assert!(p.starts_with('…'));
        assert!(p.chars().count() <= 21);
        assert_eq!(partial_text(&[], "", 70), "");
    }
}
