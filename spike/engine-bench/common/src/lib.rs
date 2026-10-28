//! Shared helpers for the LVW engine spike: clip loading, word error rate, RSS, CSV rows.

use anyhow::{bail, Context, Result};
use std::path::Path;

pub const SAMPLE_RATE: usize = 16_000;

/// Reads a 16 kHz mono 16-bit PCM WAV into f32 samples in [-1, 1].
pub fn load_clip(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate as usize != SAMPLE_RATE || spec.channels != 1 || spec.bits_per_sample != 16 {
        bail!("{}: expected 16 kHz mono 16-bit, got {:?}", path.display(), spec);
    }
    Ok(reader.samples::<i16>().map(|s| s.map(|v| v as f32 / 32768.0)).collect::<std::result::Result<_, _>>()?)
}

/// Zero-pads `pcm` to at least `seconds` of audio (the §18.5 rule: short clips are padded to 1.25 s).
pub fn pad_to_seconds(mut pcm: Vec<f32>, seconds: f32) -> Vec<f32> {
    let want = (seconds * SAMPLE_RATE as f32) as usize;
    if pcm.len() < want {
        pcm.resize(want, 0.0);
    }
    pcm
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word error rate: word-level Levenshtein distance divided by the reference word count.
pub fn wer(reference: &str, hypothesis: &str) -> f32 {
    let r = words(reference);
    let h = words(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0; h.len() + 1];
    for i in 1..=r.len() {
        cur[0] = i;
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[h.len()] as f32 / r.len() as f32
}

/// Resident set size of this process in MB (via `ps`, good enough for a spike).
pub fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().map(|kb| kb / 1024.0).unwrap_or(-1.0),
        Err(_) => -1.0,
    }
}

/// One benchmark run.
pub struct Row {
    pub engine: &'static str,
    pub model: String,
    pub clip: String,
    pub config: String,
    pub run: u8,
    pub load_ms: u128,
    pub decode_ms: u128,
    pub rss_mb: f64,
    pub wer: f32,
    pub text: String,
    pub notes: String,
}

pub fn csv_header() -> &'static str {
    "engine,model,clip,config,run,load_ms,decode_ms,rss_mb,wer,text,notes"
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\"").replace('\n', " "))
}

impl Row {
    pub fn to_csv(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{:.0},{:.3},{},{}",
            self.engine, self.model, self.clip, self.config, self.run, self.load_ms, self.decode_ms,
            self.rss_mb, self.wer, q(&self.text), q(&self.notes)
        )
    }
}

/// Loads the three clips and their reference scripts from `dir`: (name, pcm, reference).
pub fn load_clips(dir: &Path) -> Result<Vec<(String, Vec<f32>, String)>> {
    let mut out = Vec::new();
    for name in ["30s", "10s", "short"] {
        let pcm = load_clip(&dir.join(format!("clip_{name}.wav")))?;
        let reference = std::fs::read_to_string(dir.join(format!("script_{name}.txt")))?;
        out.push((name.to_string(), pcm, reference));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_counts_missing_words_against_reference_length() {
        let w = wer("hello world foo", "hello world");
        assert!((w - 1.0 / 3.0).abs() < 1e-6, "got {w}");
    }

    #[test]
    fn wer_is_zero_for_identical_text() {
        assert_eq!(wer("a b", "a b"), 0.0);
    }

    #[test]
    fn wer_ignores_case_and_punctuation() {
        assert_eq!(wer("Hello, world!", "hello world"), 0.0);
    }

    #[test]
    fn wer_counts_substitutions() {
        let w = wer("one two three four", "one 2 three four");
        assert!((w - 0.25).abs() < 1e-6, "got {w}");
    }

    #[test]
    fn pad_to_seconds_extends_short_audio_only() {
        assert_eq!(pad_to_seconds(vec![0.1; 100], 1.25).len(), 20_000);
        assert_eq!(pad_to_seconds(vec![0.1; 30_000], 1.25).len(), 30_000);
    }
}
