//! Speech trimming with Silero VAD (through whisper.cpp's VAD context) and clip classification.

use anyhow::{anyhow, Result};
use std::path::Path;
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

use crate::audio::SAMPLE_RATE;

/// Smoothing and Silero thresholds (design §18.4, §18.6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VadConfig {
    pub threshold: f32,
    pub pre_roll_ms: u32,
    pub onset_ms: u32,
    pub hangover_ms: u32,
    pub min_speech_ms: u32,
    pub min_silence_ms: u32,
    pub speech_pad_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        VadConfig { threshold: 0.5, pre_roll_ms: 450, onset_ms: 60, hangover_ms: 450, min_speech_ms: 250, min_silence_ms: 100, speech_pad_ms: 30 }
    }
}

/// What a recorded clip turned out to be.
pub enum ClipClass {
    /// Only digital zeros: the microphone is muted or not delivering audio.
    Silent,
    /// Real audio but no speech detected.
    NothingHeard,
    /// Speech, trimmed to the detected span plus pre-roll and hangover.
    Speech(Vec<f32>),
}

/// A loaded Silero VAD model.
pub struct Vad {
    ctx: WhisperVadContext,
}

impl Vad {
    pub fn new(model_path: &Path) -> Result<Vad> {
        let mut params = WhisperVadContextParams::new();
        params.set_n_threads(2);
        params.set_use_gpu(false);
        let path = model_path.to_str().ok_or_else(|| anyhow!("non-UTF-8 VAD model path"))?;
        let ctx = WhisperVadContext::new(path, params).map_err(|e| anyhow!("load VAD model {}: {e:?}", model_path.display()))?;
        Ok(Vad { ctx })
    }

    /// Classifies a 16 kHz mono clip and trims it to speech.
    pub fn classify(&mut self, pcm: &[f32], cfg: &VadConfig) -> Result<ClipClass> {
        if pcm.iter().all(|x| x.abs() < 1e-6) {
            return Ok(ClipClass::Silent);
        }
        let mut p = WhisperVadParams::new();
        p.set_threshold(cfg.threshold);
        p.set_min_speech_duration(cfg.min_speech_ms as i32);
        p.set_min_silence_duration(cfg.min_silence_ms as i32);
        p.set_speech_pad(cfg.speech_pad_ms as i32);
        let segs = self.ctx.segments_from_samples(p, pcm).map_err(|e| anyhow!("vad: {e:?}"))?;
        let n = segs.num_segments();
        if n <= 0 {
            return Ok(ClipClass::NothingHeard);
        }
        let cs_to_samples = |cs: f32| (cs * SAMPLE_RATE as f32 / 100.0) as usize;
        let first = segs.get_segment_start_timestamp(0).map(cs_to_samples).unwrap_or(0);
        let last = segs.get_segment_end_timestamp(n - 1).map(cs_to_samples).unwrap_or(pcm.len());
        let pre = cfg.pre_roll_ms as usize * SAMPLE_RATE as usize / 1000;
        let post = cfg.hangover_ms as usize * SAMPLE_RATE as usize / 1000;
        let start = first.saturating_sub(pre);
        let end = (last + post).min(pcm.len());
        if end <= start {
            return Ok(ClipClass::NothingHeard);
        }
        Ok(ClipClass::Speech(pcm[start..end].to_vec()))
    }
}

/// Zero-pads to at least `min_seconds` (Whisper is unreliable below one second; design §18.5).
pub fn pad_min(mut pcm: Vec<f32>, min_seconds: f32) -> Vec<f32> {
    let want = (min_seconds * SAMPLE_RATE as f32) as usize;
    if pcm.len() < want {
        pcm.resize(want, 0.0);
    }
    pcm
}

/// Clips shorter than `min_ms` are discarded before any processing (design §4.3 step 2).
pub fn is_too_short(pcm: &[f32], min_ms: u32) -> bool {
    pcm.len() < min_ms as usize * SAMPLE_RATE as usize / 1000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/models/ggml-silero-v5.1.2.bin")
    }

    fn fixture() -> Vec<f32> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hello.wav");
        let mut r = hound::WavReader::open(p).unwrap();
        r.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect()
    }

    fn noise(n: usize, amp: f32) -> Vec<f32> {
        let mut x: u32 = 12345;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                ((x >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    #[test]
    fn digital_silence_is_silent() {
        let mut v = Vad::new(&model()).unwrap();
        assert!(matches!(v.classify(&vec![0.0; 16_000], &VadConfig::default()).unwrap(), ClipClass::Silent));
    }

    #[test]
    fn low_noise_is_nothing_heard() {
        let mut v = Vad::new(&model()).unwrap();
        assert!(matches!(v.classify(&noise(16_000, 0.002), &VadConfig::default()).unwrap(), ClipClass::NothingHeard));
    }

    #[test]
    fn speech_fixture_is_speech_and_trimmed() {
        let mut v = Vad::new(&model()).unwrap();
        let mut pcm = vec![0.0f32; 16_000];
        pcm.extend(fixture());
        pcm.extend(vec![0.0f32; 16_000]);
        let total = pcm.len();
        match v.classify(&pcm, &VadConfig::default()).unwrap() {
            ClipClass::Speech(s) => {
                assert!(s.len() >= 8_000 && s.len() <= 32_000, "trimmed to {} samples of {}", s.len(), total);
            }
            other => panic!("expected speech, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn pad_min_extends_to_1_25_s() {
        assert_eq!(pad_min(vec![0.1; 100], 1.25).len(), 20_000);
        assert_eq!(pad_min(vec![0.1; 30_000], 1.25).len(), 30_000);
    }

    #[test]
    fn too_short_under_300_ms() {
        assert!(is_too_short(&vec![0.0; 4_000], 300));
        assert!(!is_too_short(&vec![0.0; 4_800], 300));
    }
}
