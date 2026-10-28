//! Spike: benchmark transcribe-cpp (transcribe.cpp, Metal) on the three clips under three run configs.
//! Usage: bench-transcribe-cpp <model.gguf> <clips_dir>

use anyhow::{bail, Result};
use common::{csv_header, load_clips, pad_to_seconds, rss_mb, wer, Row};
use std::path::{Path, PathBuf};
use std::time::Instant;
use transcribe_cpp::{Backend, Model, ModelOptions, RunExtension, RunOptions, WhisperRunOptions};

fn options(id: &str) -> RunOptions {
    match id {
        "default_en" => RunOptions { language: Some("en".into()), ..Default::default() },
        "default_auto" => RunOptions { language: None, ..Default::default() },
        "prompt_en" => RunOptions {
            language: Some("en".into()),
            family: Some(RunExtension::Whisper(WhisperRunOptions {
                initial_prompt: Some("Tauri, LVW.".into()),
                ..Default::default()
            })),
            ..Default::default()
        },
        _ => unreachable!(),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model_path = PathBuf::from(&args[1]);
    let clips = load_clips(Path::new(&args[2]))?;
    let model_name = model_path.file_name().unwrap().to_string_lossy().to_string();
    println!("{}", csv_header());

    let t = Instant::now();
    let model = Model::load_with(&model_path, &ModelOptions { backend: Backend::Metal, ..Default::default() })?;
    let load_ms = t.elapsed().as_millis();
    let backend = model.backend();
    eprintln!("loaded in {load_ms} ms on backend {backend}, rss {:.0} MB", rss_mb());
    if backend != "metal" {
        bail!("expected metal backend, got {backend}");
    }
    let mut session = model.session()?;

    for id in ["default_en", "default_auto", "prompt_en"] {
        for (name, pcm, reference) in &clips {
            let variants: Vec<(String, Vec<f32>)> = if name == "short" {
                vec![(id.to_string(), pad_to_seconds(pcm.clone(), 1.25)), (format!("{id}_unpadded"), pcm.clone())]
            } else {
                vec![(id.to_string(), pcm.clone())]
            };
            for (config, audio) in variants {
                for run in 1..=2u8 {
                    let opts = options(id);
                    let t = Instant::now();
                    let (text, notes) = match session.run(&audio, &opts) {
                        Ok(tr) => (tr.text.trim().to_string(), format!("lang={:?} timings={:?}", tr.language, tr.timings)),
                        Err(e) => (String::new(), format!("error: {e}")),
                    };
                    let decode_ms = t.elapsed().as_millis();
                    let row = Row {
                        engine: "transcribe-cpp",
                        model: model_name.clone(),
                        clip: name.clone(),
                        config: config.clone(),
                        run,
                        load_ms,
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
