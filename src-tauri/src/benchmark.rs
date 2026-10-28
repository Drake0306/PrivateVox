//! Per-model benchmark: a fixed 30 s clip, decoded once, reported as seconds of audio per second (design §4.6).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCRIPT: &str = "Can you send me the updated deck before the three p.m. call? I folded the review comments into the spec and pushed a new revision this morning. Two points are still open: the retry budget, and who owns the on-call rota next week. We are building the shell with Tauri, so the user interface stays plain HTML. The benchmark clip is thirty seconds long and contains about a hundred words, give or take. If the Vulkan build is slower than expected, we fall back to the CPU path and measure again. Please add the numbers to the results table and push before you leave.";

/// Seconds of audio decoded per second of wall time. Zero when the decode took no measurable time.
pub fn speed_x(audio_seconds: f32, decode: Duration) -> f32 {
    let secs = decode.as_secs_f32();
    if secs <= 0.0 {
        0.0
    } else {
        audio_seconds / secs
    }
}

/// Makes (once) and returns the 30 s benchmark clip as 16 kHz mono WAV in `dir`.
pub fn ensure_benchmark_clip(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let wav = dir.join("bench_30s.wav");
    if wav.exists() {
        return Ok(wav);
    }
    #[cfg(target_os = "macos")]
    {
        let aiff = dir.join("bench_30s.aiff");
        let status = std::process::Command::new("say").args(["-v", "Samantha", "-r", "175", "-o"]).arg(&aiff).arg(SCRIPT).status().context("run say")?;
        if !status.success() {
            bail!("say failed with {status}");
        }
        let status = std::process::Command::new("afconvert").args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"]).arg(&aiff).arg(&wav).status().context("run afconvert")?;
        let _ = std::fs::remove_file(&aiff);
        if !status.success() {
            bail!("afconvert failed with {status}");
        }
        Ok(wav)
    }
    #[cfg(not(target_os = "macos"))]
    {
        bail!("benchmark clip generation is macOS-only for now")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn speed_x_is_audio_over_decode() {
        assert!((speed_x(30.0, Duration::from_secs(6)) - 5.0).abs() < 1e-4);
        assert!((speed_x(30.0, Duration::from_millis(1200)) - 25.0).abs() < 1e-4);
        assert_eq!(speed_x(30.0, Duration::ZERO), 0.0, "no division by zero");
    }

    #[test]
    #[ignore = "runs the macOS speech synthesizer"]
    fn ensure_benchmark_clip_is_cached() {
        let d = tempfile::tempdir().unwrap();
        let p = ensure_benchmark_clip(d.path()).unwrap();
        let first = std::fs::metadata(&p).unwrap().modified().unwrap();
        let pcm = privatevox_core::audio::load_wav_16k(&p).unwrap();
        assert!((pcm.len() as f32 / 16_000.0) > 20.0, "clip should be around 30 s, got {} s", pcm.len() as f32 / 16_000.0);
        let p2 = ensure_benchmark_clip(d.path()).unwrap();
        assert_eq!(p, p2);
        assert_eq!(std::fs::metadata(&p2).unwrap().modified().unwrap(), first, "second call must reuse the cached clip");
    }
}
