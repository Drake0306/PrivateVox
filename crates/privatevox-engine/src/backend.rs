//! whisper-rs backend: loads a ggml model on the GPU and decodes with the spike's defaults.

use crate::policy::audio_ctx_for;
use crate::protocol::DecodeOptions;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

/// One decode's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub language: Option<String>,
    pub audio_ctx: i32,
    pub reran: bool,
}

pub struct WhisperBackend {
    ctx: WhisperContext,
    /// One decode state reused for every run (the spike measured no benefit from fresh states).
    state: WhisperState,
}

impl WhisperBackend {
    pub fn load(path: &Path, flash_attn: bool, use_gpu: bool) -> Result<(Self, Duration)> {
        let t = Instant::now();
        let mut cp = WhisperContextParameters::default();
        cp.use_gpu(use_gpu);
        cp.flash_attn(flash_attn);
        let p = path.to_str().ok_or_else(|| anyhow!("non-UTF-8 model path"))?;
        let ctx = WhisperContext::new_with_params(p, cp).map_err(|e| anyhow!("load {}: {e:?}", path.display()))?;
        let state = ctx.create_state().map_err(|e| anyhow!("create state: {e:?}"))?;
        Ok((WhisperBackend { ctx, state }, t.elapsed()))
    }

    /// Decodes 16 kHz mono audio with the fast-tier defaults and the audio-context policy plus repetition guard.
    pub fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> Result<Decoded> {
        let seconds = pcm.len() as f32 / 16_000.0;
        let ctx_len = audio_ctx_for(seconds);
        let (text, language) = self.run(pcm, opts, ctx_len)?;
        if ctx_len < 1500 && privatevox_core::textnorm::has_repetition_loop(&text, 4) {
            log::warn!("repetition loop at audio_ctx {ctx_len}; re-running at 1500");
            let (text, language) = self.run(pcm, opts, 1500)?;
            return Ok(Decoded { text, language, audio_ctx: 1500, reran: true });
        }
        Ok(Decoded { text, language, audio_ctx: ctx_len, reran: false })
    }

    fn run(&mut self, pcm: &[f32], opts: &DecodeOptions, audio_ctx: i32) -> Result<(String, Option<String>)> {
        let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        p.set_n_threads(opts.n_threads.max(1));
        p.set_language(opts.language.as_deref());
        p.set_translate(opts.translate);
        p.set_no_context(true);
        p.set_single_segment(false);
        p.set_suppress_blank(true);
        p.set_suppress_nst(true);
        p.set_print_progress(false);
        p.set_print_special(false);
        p.set_print_realtime(false);
        p.set_print_timestamps(false);
        // Timestamps stay on: with them off whisper.cpp advances by a full window and drops words at 30 s.
        p.set_no_timestamps(false);
        p.set_audio_ctx(audio_ctx);
        if let Some(prompt) = opts.initial_prompt.as_deref() {
            p.set_initial_prompt(prompt);
        }
        self.state.full(p, pcm).map_err(|e| anyhow!("decode: {e:?}"))?;
        let mut text = String::new();
        for seg in self.state.as_iter() {
            text.push_str(&seg.to_str_lossy().map_err(|e| anyhow!("segment text: {e:?}"))?);
        }
        let language = match &opts.language {
            Some(l) => Some(l.clone()),
            None => whisper_rs::get_lang_str(self.state.full_lang_id_from_state()).map(|s| s.to_string()),
        };
        let _ = &self.ctx;
        Ok((text.trim().to_string(), language))
    }
}

#[cfg(test)]
mod spike_tests {
    use super::*;
    use crate::protocol::DecodeOptions;
    use std::time::Instant;

    /// `PRIVATEVOX_TEST_MODEL=<ggml file> PRIVATEVOX_SPIKE_CLIPS=<dir with clip_*.wav> cargo test -p privatevox-engine --release --lib spike_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn spike_timing_on_clips() {
        let model = std::env::var("PRIVATEVOX_TEST_MODEL").expect("PRIVATEVOX_TEST_MODEL");
        let dir = std::env::var("PRIVATEVOX_SPIKE_CLIPS").expect("PRIVATEVOX_SPIKE_CLIPS");
        let (mut backend, load) = WhisperBackend::load(std::path::Path::new(&model), true, true).unwrap();
        println!("SPIKE load_ms={}", load.as_millis());
        for clip in ["clip_30", "clip_10", "clip_short"] {
            let pcm = privatevox_core::audio::load_wav_16k(std::path::Path::new(&format!("{dir}/{clip}.wav"))).unwrap();
            let opts = DecodeOptions { language: Some("en".into()), translate: false, initial_prompt: None, n_threads: 4, final_chunk: true };
            let mut times = Vec::new();
            let mut text = String::new();
            for _ in 0..3 {
                let t = Instant::now();
                let d = backend.transcribe(&pcm, &opts).unwrap();
                times.push(t.elapsed().as_millis());
                text = d.text.clone();
            }
            println!("SPIKE {clip} secs={:.1} runs_ms={:?} text={:?}", pcm.len() as f32 / 16000.0, times, text);
        }
    }
}
