//! Keep the microphone warm (design §18.3, after Handy): opening an input stream costs 100–300 ms on some
//! devices and shows the OS microphone indicator a beat late. After a dictation the stream stays open, with
//! delivery paused, for a few seconds; the next dictation reuses it, and a timer closes it when nobody came back.
//! Pure timing state; the adapter owns the stream.

use std::time::{Duration, Instant};

/// What `on_start` asks the adapter to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The stream is still open: resume delivery, no new open.
    Reuse,
    /// Nothing open (or the window passed): open the device.
    Open,
}

/// What `on_stop` asks the adapter to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Pause delivery, keep the stream; `due` says when to close it.
    Keep,
    /// Lazy close is off (0 s): close the stream now.
    Close,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WarmMic {
    open: bool,
    close_at: Option<Instant>,
}

impl WarmMic {
    pub fn new() -> Self {
        Self::default()
    }

    /// A dictation (or mic test) starts.
    pub fn on_start(&mut self) -> Start {
        self.close_at = None;
        if self.open {
            Start::Reuse
        } else {
            self.open = true;
            Start::Open
        }
    }

    /// A dictation ends; `lazy` is the keep-warm window (0 closes at once).
    pub fn on_stop(&mut self, now: Instant, lazy: Duration) -> Stop {
        if !self.open {
            return Stop::Close;
        }
        if lazy.is_zero() {
            self.open = false;
            self.close_at = None;
            Stop::Close
        } else {
            self.close_at = Some(now + lazy);
            Stop::Keep
        }
    }

    /// True when the window has passed and the stream should be closed.
    pub fn due(&self, now: Instant) -> bool {
        self.open && self.close_at.is_some_and(|t| now >= t)
    }

    /// The adapter closed the stream (timer fired, device changed, or an error).
    pub fn mark_closed(&mut self) {
        self.open = false;
        self.close_at = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_is_reused_within_the_window_and_closed_after() {
        let t0 = Instant::now();
        let lazy = Duration::from_secs(20);
        let mut w = WarmMic::new();
        assert_eq!(w.on_start(), Start::Open, "first dictation opens the device");
        assert_eq!(w.on_stop(t0, lazy), Stop::Keep, "the stream stays open after the dictation");
        assert!(!w.due(t0 + Duration::from_secs(5)), "not due inside the window");
        assert_eq!(w.on_start(), Start::Reuse, "a dictation 5 s later reuses the open stream");
        assert!(!w.due(t0 + Duration::from_secs(60)), "a running dictation is never due, however old the stop was");
        assert_eq!(w.on_stop(t0 + Duration::from_secs(10), lazy), Stop::Keep);
        assert!(w.due(t0 + Duration::from_secs(30)), "20 s after the last stop the close is due");
        w.mark_closed();
        assert!(!w.is_open());
        assert!(!w.due(t0 + Duration::from_secs(100)), "nothing to close once closed");
        assert_eq!(w.on_start(), Start::Open, "after the close the next dictation opens again");
    }

    #[test]
    fn zero_window_closes_at_once() {
        let mut w = WarmMic::new();
        assert_eq!(w.on_start(), Start::Open);
        assert_eq!(w.on_stop(Instant::now(), Duration::ZERO), Stop::Close, "0 s means the old behaviour: close on stop");
        assert!(!w.is_open());
        assert_eq!(w.on_stop(Instant::now(), Duration::from_secs(20)), Stop::Close, "stopping an unopened stream asks for nothing to be kept");
    }
}
