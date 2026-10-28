//! The activation state machine: push-to-talk, toggle, hold-or-toggle, busy-press memory (design §18.10).
//!
//! Pure: it decides, the coordinator executes. A recording starts optimistically on key-down and is rolled
//! back with `Input::StartResult(false)` if the microphone could not be opened.

use crate::settings::Activation;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Ptt,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Down(Binding),
    Up(Binding),
    Cancel,
    PipelineDone,
    StartResult(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    StartRecording,
    StopRecording,
    CancelRecording,
    Nothing,
}

#[derive(Debug, Clone, Copy)]
pub enum Stage {
    Idle,
    /// `locked` means the recording outlives the key: the next press stops it.
    Recording { locked: bool, since: Instant, binding: Binding },
    Processing,
}

pub struct Machine {
    activation: Activation,
    hold_threshold: Duration,
    stage: Stage,
    remembered: Option<Binding>,
}

impl Machine {
    pub fn new(activation: Activation, hold_threshold: Duration) -> Self {
        Machine { activation, hold_threshold, stage: Stage::Idle, remembered: None }
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    pub fn set_activation(&mut self, activation: Activation, hold_threshold: Duration) {
        self.activation = activation;
        self.hold_threshold = hold_threshold;
    }

    fn locked_from_start(&self, binding: Binding) -> bool {
        binding == Binding::Toggle || matches!(self.activation, Activation::Toggle)
    }

    fn start(&mut self, binding: Binding, now: Instant) -> Effect {
        self.stage = Stage::Recording { locked: self.locked_from_start(binding), since: now, binding };
        Effect::StartRecording
    }

    pub fn on(&mut self, input: Input, now: Instant) -> Effect {
        match (self.stage, input) {
            (Stage::Idle, Input::Down(b)) => self.start(b, now),
            (Stage::Idle, _) => Effect::Nothing,

            (Stage::Recording { .. }, Input::StartResult(false)) => {
                self.stage = Stage::Idle;
                Effect::Nothing
            }
            (Stage::Recording { .. }, Input::StartResult(true)) => Effect::Nothing,
            (Stage::Recording { .. }, Input::Cancel) => {
                self.stage = Stage::Idle;
                Effect::CancelRecording
            }
            (Stage::Recording { locked: true, .. }, Input::Down(_)) => {
                self.stage = Stage::Processing;
                Effect::StopRecording
            }
            (Stage::Recording { locked: true, .. }, Input::Up(_)) => Effect::Nothing,
            (Stage::Recording { locked: false, since, binding }, Input::Up(b)) if b == binding => {
                let held = now.saturating_duration_since(since);
                let tap = matches!(self.activation, Activation::HoldOrToggle) && held < self.hold_threshold;
                if tap {
                    self.stage = Stage::Recording { locked: true, since, binding };
                    Effect::Nothing
                } else {
                    self.stage = Stage::Processing;
                    Effect::StopRecording
                }
            }
            (Stage::Recording { .. }, _) => Effect::Nothing,

            (Stage::Processing, Input::Down(b)) => {
                self.remembered = if self.remembered.is_some() { None } else { Some(b) };
                Effect::Nothing
            }
            (Stage::Processing, Input::Cancel) => {
                self.remembered = None;
                Effect::Nothing
            }
            (Stage::Processing, Input::PipelineDone) => match self.remembered.take() {
                Some(b) => self.start(b, now),
                None => {
                    self.stage = Stage::Idle;
                    Effect::Nothing
                }
            },
            (Stage::Processing, _) => Effect::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Activation;
    use std::time::{Duration, Instant};

    fn m(a: Activation) -> (Machine, Instant) {
        (Machine::new(a, Duration::from_millis(300)), Instant::now())
    }
    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    #[test]
    fn ptt_hold_and_release_records_once() {
        let (mut mc, t) = m(Activation::PushToTalk);
        assert!(matches!(mc.on(Input::Down(Binding::Ptt), t), Effect::StartRecording));
        assert!(matches!(mc.on(Input::StartResult(true), t), Effect::Nothing));
        assert!(matches!(mc.on(Input::Up(Binding::Ptt), ms(t, 800)), Effect::StopRecording));
        assert!(matches!(mc.stage(), Stage::Processing));
        assert!(matches!(mc.on(Input::PipelineDone, ms(t, 2000)), Effect::Nothing));
        assert!(matches!(mc.stage(), Stage::Idle));
    }

    #[test]
    fn toggle_tap_tap_starts_then_stops() {
        let (mut mc, t) = m(Activation::Toggle);
        assert!(matches!(mc.on(Input::Down(Binding::Toggle), t), Effect::StartRecording));
        mc.on(Input::StartResult(true), t);
        assert!(matches!(mc.on(Input::Up(Binding::Toggle), ms(t, 50)), Effect::Nothing));
        assert!(matches!(mc.on(Input::Down(Binding::Toggle), ms(t, 3000)), Effect::StopRecording));
    }

    #[test]
    fn hold_or_toggle_short_tap_locks_until_next_press() {
        let (mut mc, t) = m(Activation::HoldOrToggle);
        mc.on(Input::Down(Binding::Ptt), t);
        mc.on(Input::StartResult(true), t);
        assert!(matches!(mc.on(Input::Up(Binding::Ptt), ms(t, 120)), Effect::Nothing));
        assert!(matches!(mc.stage(), Stage::Recording { locked: true, .. }));
        assert!(matches!(mc.on(Input::Down(Binding::Ptt), ms(t, 4000)), Effect::StopRecording));
    }

    #[test]
    fn hold_or_toggle_long_hold_stops_on_release() {
        let (mut mc, t) = m(Activation::HoldOrToggle);
        mc.on(Input::Down(Binding::Ptt), t);
        mc.on(Input::StartResult(true), t);
        assert!(matches!(mc.on(Input::Up(Binding::Ptt), ms(t, 350)), Effect::StopRecording));
    }

    #[test]
    fn press_while_processing_is_remembered_until_pipeline_done() {
        let (mut mc, t) = m(Activation::PushToTalk);
        mc.on(Input::Down(Binding::Ptt), t);
        mc.on(Input::StartResult(true), t);
        mc.on(Input::Up(Binding::Ptt), ms(t, 500));
        assert!(matches!(mc.on(Input::Down(Binding::Ptt), ms(t, 600)), Effect::Nothing));
        assert!(matches!(mc.on(Input::PipelineDone, ms(t, 900)), Effect::StartRecording));
    }

    #[test]
    fn two_presses_while_processing_cancel_out() {
        let (mut mc, t) = m(Activation::PushToTalk);
        mc.on(Input::Down(Binding::Ptt), t);
        mc.on(Input::StartResult(true), t);
        mc.on(Input::Up(Binding::Ptt), ms(t, 500));
        mc.on(Input::Down(Binding::Ptt), ms(t, 600));
        mc.on(Input::Up(Binding::Ptt), ms(t, 650));
        mc.on(Input::Down(Binding::Ptt), ms(t, 700));
        assert!(matches!(mc.on(Input::PipelineDone, ms(t, 900)), Effect::Nothing));
        assert!(matches!(mc.stage(), Stage::Idle));
    }

    #[test]
    fn optimistic_start_rolls_back_when_capture_fails() {
        let (mut mc, t) = m(Activation::PushToTalk);
        assert!(matches!(mc.on(Input::Down(Binding::Ptt), t), Effect::StartRecording));
        assert!(matches!(mc.stage(), Stage::Recording { .. }));
        mc.on(Input::StartResult(false), t);
        assert!(matches!(mc.stage(), Stage::Idle));
        assert!(matches!(mc.on(Input::Up(Binding::Ptt), ms(t, 100)), Effect::Nothing));
    }

    #[test]
    fn cancel_while_recording_cancels() {
        let (mut mc, t) = m(Activation::Toggle);
        mc.on(Input::Down(Binding::Toggle), t);
        mc.on(Input::StartResult(true), t);
        assert!(matches!(mc.on(Input::Cancel, ms(t, 100)), Effect::CancelRecording));
        assert!(matches!(mc.stage(), Stage::Idle));
    }
}
