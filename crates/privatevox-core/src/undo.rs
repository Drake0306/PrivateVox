//! Undo last paste: send the app's undo keystroke only when it is safe (design §4.2).

use std::time::{Duration, Instant};

/// What the last successful paste put where.
#[derive(Debug, Clone)]
pub struct LastPaste {
    pub text: String,
    pub app: Option<String>,
    pub at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoDecision {
    /// Same app still in front and within the window: send the undo keystroke.
    SendUndo,
    /// Not safe to send keystrokes; put the text back on the clipboard instead.
    CopyAgain,
    /// Nothing was pasted.
    Nothing,
}

pub fn decide(last: Option<&LastPaste>, frontmost: Option<&str>, now: Instant, window: Duration) -> UndoDecision {
    let Some(last) = last else { return UndoDecision::Nothing };
    let same_app = matches!((last.app.as_deref(), frontmost), (Some(a), Some(b)) if a == b);
    let fresh = now.saturating_duration_since(last.at) <= window;
    if same_app && fresh {
        UndoDecision::SendUndo
    } else {
        UndoDecision::CopyAgain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn last(app: &str, ago: Duration) -> LastPaste {
        LastPaste { text: "hello".into(), app: Some(app.into()), at: Instant::now() - ago }
    }

    #[test]
    fn undo_within_window_same_app() {
        let l = last("TextEdit", Duration::from_secs(5));
        assert!(matches!(decide(Some(&l), Some("TextEdit"), Instant::now(), Duration::from_secs(30)), UndoDecision::SendUndo));
    }

    #[test]
    fn undo_refused_when_frontmost_changed() {
        let l = last("TextEdit", Duration::from_secs(5));
        assert!(matches!(decide(Some(&l), Some("Safari"), Instant::now(), Duration::from_secs(30)), UndoDecision::CopyAgain));
        assert!(matches!(decide(Some(&l), None, Instant::now(), Duration::from_secs(30)), UndoDecision::CopyAgain), "unknown front app is not safe");
    }

    #[test]
    fn undo_expired_is_copy_again() {
        let l = last("TextEdit", Duration::from_secs(31));
        assert!(matches!(decide(Some(&l), Some("TextEdit"), Instant::now(), Duration::from_secs(30)), UndoDecision::CopyAgain));
    }

    #[test]
    fn nothing_without_a_paste() {
        assert!(matches!(decide(None, Some("TextEdit"), Instant::now(), Duration::from_secs(30)), UndoDecision::Nothing));
    }
}
