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
        if ctx_len < 1500 && lvw_core::textnorm::has_repetition_loop(&text, 4) {
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
