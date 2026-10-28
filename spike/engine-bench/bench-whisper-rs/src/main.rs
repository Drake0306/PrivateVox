//! Spike: benchmark whisper-rs (whisper.cpp, Metal) on the three clips under several decode configs.
//! Usage: bench-whisper-rs <model.bin> <clips_dir>

use anyhow::Result;
use common::{csv_header, load_clips, pad_to_seconds, rss_mb, wer, Row};
use std::path::{Path, PathBuf};
use std::time::Instant;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

struct Cfg {
    id: &'static str,
    beam: Option<i32>,
    flash: bool,
    language: Option<&'static str>,
    audio_ctx: Option<i32>,
    short_only: bool,
}

const CONFIGS: &[Cfg] = &[
    Cfg { id: "greedy_en", beam: None, flash: false, language: Some("en"), audio_ctx: None, short_only: false },
    Cfg { id: "greedy_en_flash", beam: None, flash: true, language: Some("en"), audio_ctx: None, short_only: false },
    Cfg { id: "beam5_en", beam: Some(5), flash: false, language: Some("en"), audio_ctx: None, short_only: false },
    Cfg { id: "greedy_auto", beam: None, flash: false, language: None, audio_ctx: None, short_only: false },
    Cfg { id: "greedy_en_ctx768", beam: None, flash: false, language: Some("en"), audio_ctx: Some(768), short_only: true },
    Cfg { id: "greedy_en_flash_ctx768", beam: None, flash: true, language: Some("en"), audio_ctx: Some(768), short_only: true },
    Cfg { id: "greedy_en_ctx512", beam: None, flash: false, language: Some("en"), audio_ctx: Some(512), short_only: true },
    Cfg { id: "greedy_en_ctx384", beam: None, flash: false, language: Some("en"), audio_ctx: Some(384), short_only: true },
    Cfg { id: "greedy_en_ctx1024", beam: None, flash: false, language: Some("en"), audio_ctx: Some(1024), short_only: true },
    Cfg { id: "greedy_en_ctx1500", beam: None, flash: false, language: Some("en"), audio_ctx: Some(1500), short_only: true },
];

fn load_ctx(path: &Path, flash: bool) -> Result<(WhisperContext, u128)> {
    let mut cp = WhisperContextParameters::default();
    cp.use_gpu(true);
    cp.flash_attn(flash);
    let t = Instant::now();
    let ctx = WhisperContext::new_with_params(path.to_str().unwrap(), cp)?;
    Ok((ctx, t.elapsed().as_millis()))
}

fn transcribe(ctx: &WhisperContext, state: &mut whisper_rs::WhisperState, cfg: &Cfg, pcm: &[f32]) -> Result<(String, u128)> {
    let _ = ctx;
    let strategy = match cfg.beam {
        Some(n) => SamplingStrategy::BeamSearch { beam_size: n, patience: -1.0 },
        None => SamplingStrategy::Greedy { best_of: 1 },
    };
    let mut p = FullParams::new(strategy);
    p.set_n_threads(4);
    p.set_language(cfg.language);
    p.set_no_context(true);
    p.set_single_segment(false);
    p.set_suppress_blank(true);
    p.set_suppress_nst(true);
    p.set_print_progress(false);
    p.set_print_special(false);
    p.set_print_realtime(false);
    p.set_print_timestamps(false);
    p.set_no_timestamps(std::env::var("PRIVATEVOX_TIMESTAMPS").is_err());
    if let Some(ctx_len) = cfg.audio_ctx {
        p.set_audio_ctx(ctx_len);
    }
    let t = Instant::now();
    state.full(p, pcm)?;
    let ms = t.elapsed().as_millis();
    let mut text = String::new();
    for seg in state.as_iter() {
        text.push_str(&seg.to_str_lossy()?);
    }
    Ok((text.trim().to_string(), ms))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model_path = PathBuf::from(&args[1]);
    let clips = load_clips(Path::new(&args[2]))?;
    let model = model_path.file_name().unwrap().to_string_lossy().to_string();
    println!("{}", csv_header());

    // Two contexts: flash attention is a load-time parameter.
    let reuse = std::env::var("PRIVATEVOX_REUSE_STATE").is_ok();
    let mut loaded: Vec<(bool, WhisperContext, u128)> = Vec::new();
    for flash in [false, true] {
        let (ctx, ms) = load_ctx(&model_path, flash)?;
        eprintln!("loaded flash={flash} in {ms} ms, rss {:.0} MB", rss_mb());
        loaded.push((flash, ctx, ms));
    }
    let mut states: Vec<Option<whisper_rs::WhisperState>> = vec![None, None];

    let only_ctx = std::env::var("PRIVATEVOX_ONLY_CTX").is_ok();
    let only: Vec<String> = std::env::var("PRIVATEVOX_ONLY").map(|v| v.split(',').map(|x| x.to_string()).collect()).unwrap_or_default();
    for cfg in CONFIGS.iter().filter(|c| (!only_ctx || c.id.contains("ctx")) && (only.is_empty() || only.iter().any(|o| o == c.id))) {
        let (_, ctx, load_ms) = loaded.iter().find(|(f, _, _)| *f == cfg.flash).unwrap();
        for (name, pcm, reference) in &clips {
            if cfg.short_only && name == "30s" {
                continue;
            }
            let variants: Vec<(String, Vec<f32>)> = if name == "short" {
                vec![(cfg.id.to_string(), pad_to_seconds(pcm.clone(), 1.25)), (format!("{}_unpadded", cfg.id), pcm.clone())]
            } else {
                vec![(cfg.id.to_string(), pcm.clone())]
            };
            for (config, audio) in variants {
                for run in 1..=2u8 {
                    let idx = usize::from(cfg.flash);
                    if !reuse || states[idx].is_none() { states[idx] = Some(ctx.create_state()?); }
                    let st = states[idx].as_mut().unwrap();
                    let (text, decode_ms, notes) = match transcribe(ctx, st, cfg, &audio) {
                        Ok((t, ms)) => (t, ms, String::new()),
                        Err(e) => (String::new(), 0, format!("error: {e}")),
                    };
                    let row = Row {
                        engine: "whisper-rs",
                        model: model.clone(),
                        clip: name.clone(),
                        config: config.clone(),
                        run,
                        load_ms: *load_ms,
                        decode_ms,
                        rss_mb: rss_mb(),
                        wer: wer(reference, &text),
                        text,
                        notes,
                    };
                    println!("{}", row.to_csv());
                }
            }
        }
    }
    Ok(())
}
