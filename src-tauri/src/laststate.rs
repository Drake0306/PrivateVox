//! Remembers the most recent pill state so a page that loads after the event can catch up.

use crate::pipeline::PillState;
use std::sync::Mutex;

#[derive(Default)]
pub struct LastState(Mutex<Option<PillState>>);

impl LastState {
    pub fn record(&self, s: PillState) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(s);
        }
    }

    pub fn current(&self) -> Option<PillState> {
        self.0.lock().ok().and_then(|g| g.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::PillState;

    #[test]
    fn last_state_returns_most_recent() {
        let holder = LastState::default();
        assert!(holder.current().is_none());
        holder.record(PillState::Transcribing);
        holder.record(PillState::Pasted { preview: "x".into() });
        assert!(matches!(holder.current(), Some(PillState::Pasted { .. })));
    }
}
