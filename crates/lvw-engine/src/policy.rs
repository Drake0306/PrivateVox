//! Decode and supervision policies pinned by the engine spike (docs/superpowers/spikes/2026-10-28-engine-spike-results.md).

use std::time::Duration;

/// Encoder positions to run for a clip: `clamp(round_up_64(1500 × (seconds + 2) / 30), 768, 1500)`.
pub fn audio_ctx_for(seconds: f32) -> i32 {
    let raw = (1500.0 * (seconds + 2.0) / 30.0).ceil() as i32;
    let rounded = (raw + 63) / 64 * 64;
    rounded.clamp(768, 1500)
}

/// How long the supervisor waits for one decode: 15 s plus three times the clip length.
pub fn deadline_for(seconds: f32) -> Duration {
    Duration::from_secs_f32(15.0 + 3.0 * seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn audio_ctx_policy_matches_spike() {
        assert_eq!(audio_ctx_for(0.7), 768);
        assert_eq!(audio_ctx_for(7.5), 768);
        assert_eq!(audio_ctx_for(13.0), 768);
        assert_eq!(audio_ctx_for(20.0), 1152);
        assert_eq!(audio_ctx_for(28.0), 1500);
        assert_eq!(audio_ctx_for(40.0), 1500);
    }

    #[test]
    fn deadline_scales_with_clip() {
        assert_eq!(deadline_for(10.0), Duration::from_secs(45));
        assert_eq!(deadline_for(300.0), Duration::from_secs(915));
    }
}
