//! Trailing-silence counter for auto-stop in toggle mode (phase 5, A2).

/// Speech frames in a row before a run counts as speech (4 × 32 ms): a keyboard click or a cough scores one or two.
pub const MIN_SPEECH_FRAMES: usize = 4;

/// Counts silent VAD frames since the last sustained speech. Nothing counts until speech was heard once, so a latched
/// recording that has not started yet never stops itself; blips shorter than `MIN_SPEECH_FRAMES` count as silence.
#[derive(Debug, Default, Clone)]
pub struct TrailingSilence {
    heard_speech: bool,
    silent_frames: usize,
    speech_run: usize,
}

impl TrailingSilence {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed VAD probabilities (one per frame); `threshold` marks a frame as speech.
    pub fn push(&mut self, probs: &[f32], threshold: f32) {
        for &p in probs {
            if p >= threshold {
                self.speech_run += 1;
                if self.speech_run >= MIN_SPEECH_FRAMES {
                    self.heard_speech = true;
                    self.silent_frames = 0;
                    continue;
                }
            } else {
                self.speech_run = 0;
            }
            if self.heard_speech {
                self.silent_frames += 1; // a frame of a run that may still turn out to be a blip counts as silence for now
            }
        }
    }

    pub fn heard_speech(&self) -> bool {
        self.heard_speech
    }

    /// Seconds of silence since the last speech frame (0 until speech was heard).
    pub fn silent_for(&self, frame_ms: f32) -> f32 {
        self.silent_frames as f32 * frame_ms / 1000.0
    }

    pub fn due(&self, limit_secs: f32, frame_ms: f32) -> bool {
        self.heard_speech && self.silent_for(frame_ms) >= limit_secs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_MS: f32 = 32.0;

    #[test]
    fn due_only_after_speech_then_silence() {
        let mut t = TrailingSilence::new();
        t.push(&[0.0; 100], 0.5); // ~3.2 s of silence before anything was said
        assert!(!t.due(2.0, FRAME_MS), "silence before speech never stops a recording");
        assert_eq!(t.silent_for(FRAME_MS), 0.0, "nothing counts until speech was heard");
        t.push(&[0.9; 10], 0.5);
        assert!(!t.due(2.0, FRAME_MS));
        t.push(&[0.0; 40], 0.5); // 1.28 s
        assert!(!t.due(2.0, FRAME_MS));
        assert!((t.silent_for(FRAME_MS) - 1.28).abs() < 0.01);
        t.push(&[0.0; 30], 0.5); // 2.24 s
        assert!(t.due(2.0, FRAME_MS));
    }

    #[test]
    fn speech_resets_the_counter() {
        let mut t = TrailingSilence::new();
        t.push(&[0.9; 5], 0.5);
        t.push(&[0.0; 60], 0.5);
        assert!(t.silent_for(FRAME_MS) > 1.9);
        t.push(&[0.2, 0.7, 0.1], 0.5);
        assert!(t.silent_for(FRAME_MS) > 2.0, "a lone speech frame is a blip and keeps counting as silence");
        t.push(&[0.9; MIN_SPEECH_FRAMES], 0.5);
        t.push(&[0.0], 0.5);
        assert!((t.silent_for(FRAME_MS) - 0.032).abs() < 0.001, "one silent frame after sustained speech");
    }

    #[test]
    fn settings_default_two_seconds() {
        let s = crate::settings::Settings::default();
        assert_eq!(s.auto_stop_silence_secs, Some(2));
        let mut s = s;
        s.auto_stop_silence_secs = Some(0);
        assert!(s.validate().unwrap_err().contains("silence"));
        s.auto_stop_silence_secs = None;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn brief_blips_do_not_count_as_speech() {
        // A keyboard click or a cough scores one or two frames high; only sustained speech starts or resets the counter.
        let mut t = TrailingSilence::new();
        t.push(&[0.9], 0.5);
        t.push(&[0.1; 40], 0.5);
        assert!(!t.heard_speech(), "a lone frame is not speech");
        t.push(&[0.9; 20], 0.5); // 640 ms of speech
        assert!(t.heard_speech());
        t.push(&[0.1; 40], 0.5); // 1.28 s silence
        t.push(&[0.9, 0.9], 0.5); // a two-frame blip
        t.push(&[0.1; 40], 0.5); // 1.28 s more
        assert!(t.silent_for(32.0) > 2.5, "the blip did not reset the counter: {}", t.silent_for(32.0));
        assert!(t.due(2.0, 32.0));
    }
}
