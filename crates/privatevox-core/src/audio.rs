//! Audio conversion and capture: mixdown, 16 kHz resampling, bounded clip buffer, level meter, cpal input.

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

/// Everything downstream of capture is 16 kHz mono f32 in [-1, 1].
pub const SAMPLE_RATE: u32 = 16_000;

/// Averages interleaved channels into one.
pub fn mixdown(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

/// Converts mono audio at the device rate to 16 kHz. Identity when the device already runs at 16 kHz.
pub struct Resampler16k {
    inner: Option<Fft<f32>>,
    chunk: usize,
    pending: Vec<f32>,
    out: Vec<Vec<f32>>,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self> {
        if input_rate == SAMPLE_RATE {
            return Ok(Self { inner: None, chunk: 0, pending: Vec::new(), out: Vec::new() });
        }
        let chunk = 1024usize;
        let fft = Fft::<f32>::new(input_rate as usize, SAMPLE_RATE as usize, chunk, 1, FixedSync::Input)
            .map_err(|e| anyhow!("resampler: {e}"))?;
        let out = vec![vec![0.0f32; fft.output_frames_max()]];
        Ok(Self { inner: Some(fft), chunk, pending: Vec::new(), out })
    }

    /// Feeds device-rate mono samples; returns whatever 16 kHz audio is ready.
    pub fn push(&mut self, mono: &[f32]) -> Vec<f32> {
        let Some(fft) = self.inner.as_mut() else { return mono.to_vec() };
        self.pending.extend_from_slice(mono);
        let mut result = Vec::new();
        while self.pending.len() >= self.chunk {
            let frame = [self.pending.drain(..self.chunk).collect::<Vec<f32>>()];
            let input = SequentialSliceOfVecs::new(&frame, 1, self.chunk).expect("input adapter");
            let mut output = SequentialSliceOfVecs::new_mut(&mut self.out, 1, fft.output_frames_max()).expect("output adapter");
            if let Ok((_, written)) = fft.process_into_buffer(&input, &mut output, None) {
                result.extend_from_slice(&self.out[0][..written]);
            }
        }
        result
    }

    /// Pads the tail with silence to flush the last partial chunk.
    pub fn flush(&mut self) -> Vec<f32> {
        if self.inner.is_none() || self.pending.is_empty() {
            return Vec::new();
        }
        let missing = self.chunk - self.pending.len();
        let ratio = SAMPLE_RATE as f32 / self.input_rate() as f32;
        let keep = (self.pending.len() as f32 * ratio) as usize;
        let pad = vec![0.0f32; missing];
        let mut out = self.push(&pad);
        out.truncate(keep);
        out
    }

    fn input_rate(&self) -> u32 {
        // Fft resamplers keep a fixed ratio; recover the input rate from it.
        match &self.inner {
            Some(fft) => (self.chunk as f32 * SAMPLE_RATE as f32 / fft.output_frames_max() as f32).round() as u32,
            None => SAMPLE_RATE,
        }
    }
}

/// A recording buffer with a hard cap in seconds (design §4.3: 5 minutes by default).
pub struct ClipBuffer {
    samples: Vec<f32>,
    max_samples: usize,
}

impl ClipBuffer {
    pub fn new(max_seconds: u32) -> Self {
        Self { samples: Vec::new(), max_samples: max_seconds as usize * SAMPLE_RATE as usize }
    }

    /// Appends audio; returns `false` once the cap is reached (the overflow is dropped).
    pub fn push(&mut self, pcm: &[f32]) -> bool {
        let room = self.max_samples.saturating_sub(self.samples.len());
        let take = pcm.len().min(room);
        self.samples.extend_from_slice(&pcm[..take]);
        take == pcm.len() && self.samples.len() < self.max_samples
    }

    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / SAMPLE_RATE as f32
    }

    pub fn take(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.samples)
    }

    /// The audio recorded so far, without taking it.
    pub fn peek(&self) -> &[f32] {
        &self.samples
    }
}

/// RMS level in dBFS, floored at -100 for digital silence.
pub fn level_db(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return -100.0;
    }
    let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
    if rms <= 1e-5 {
        -100.0
    } else {
        (20.0 * rms.log10()).max(-100.0)
    }
}

/// Whether a stream error means the recording cannot continue. A buffer under/overrun (`Xrun`) is a glitch
/// the stream recovers from on its own; everything else means the device or permission is gone.
pub fn stream_error_is_fatal(kind: cpal::ErrorKind) -> bool {
    !matches!(kind, cpal::ErrorKind::Xrun)
}

/// Reads a 16 kHz mono 16-bit WAV (the benchmark clip and test fixtures) into f32 samples.
pub fn load_wav_16k(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 || spec.bits_per_sample != 16 {
        bail!("{}: expected 16 kHz mono 16-bit, got {:?}", path.display(), spec);
    }
    Ok(reader.samples::<i16>().map(|s| s.map(|v| v as f32 / 32768.0)).collect::<std::result::Result<_, _>>()?)
}

/// Names of the input devices cpal can see.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.description().ok().map(|desc| desc.name().to_string())).collect())
        .unwrap_or_default()
}

/// An open microphone stream delivering 16 kHz mono audio to a callback.
pub struct Capture {
    stream: cpal::Stream,
}

impl Capture {
    /// Opens `device_name` (or the default input), mixes to mono, resamples to 16 kHz, and calls `on_pcm16k`.
    pub fn start(device_name: Option<&str>, on_pcm16k: Box<dyn FnMut(&[f32]) + Send>) -> Result<Capture> {
        Self::start_with_errors(device_name, on_pcm16k, Box::new(|e| log::error!("input stream error: {e}")))
    }

    /// Like `start`, with a sink for stream errors (device lost, permission revoked).
    pub fn start_with_errors(device_name: Option<&str>, mut on_pcm16k: Box<dyn FnMut(&[f32]) + Send>, mut on_error: Box<dyn FnMut(String) + Send>) -> Result<Capture> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .input_devices()?
                .find(|d| d.description().map(|x| x.name() == name).unwrap_or(false))
                .ok_or_else(|| anyhow!("input device not found: {name}"))?,
            None => host.default_input_device().ok_or_else(|| anyhow!("no default input device"))?,
        };
        let supported = device.default_input_config().context("default input config")?;
        let channels = supported.channels() as usize;
        let rate = supported.sample_rate();
        let config = cpal::StreamConfig { channels: supported.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let mut resampler = Resampler16k::new(rate)?;
        let stream = device
            .build_input_stream::<f32, _, _>(
                config,
                move |data, _| {
                    let mono = mixdown(data, channels);
                    let out = resampler.push(&mono);
                    if !out.is_empty() {
                        on_pcm16k(&out);
                    }
                },
                move |e| {
                    if stream_error_is_fatal(e.kind()) {
                        on_error(e.to_string());
                    } else {
                        log::debug!("input stream glitch (ignored): {e}");
                    }
                },
                None,
            )
            .context("build input stream")?;
        stream.play().context("start input stream")?;
        Ok(Capture { stream })
    }

    pub fn stop(self) {
        drop(self.stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5).collect()
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count()
    }

    #[test]
    fn mixdown_averages_channels() {
        assert_eq!(mixdown(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(mixdown(&[0.2, 0.4], 1), vec![0.2, 0.4]);
    }

    #[test]
    fn resampler_48k_to_16k_keeps_duration() {
        let mut r = Resampler16k::new(48_000).unwrap();
        let mut out = r.push(&tone(48_000, 440.0, 1.0));
        out.extend(r.flush());
        assert!((15_900..=16_100).contains(&out.len()), "got {} samples", out.len());
    }

    #[test]
    fn resampler_preserves_tone() {
        let mut r = Resampler16k::new(48_000).unwrap();
        let mut out = r.push(&tone(48_000, 440.0, 1.0));
        out.extend(r.flush());
        let zc = zero_crossings(&out) as f32;
        assert!((zc - 880.0).abs() / 880.0 < 0.05, "zero crossings {zc}");
    }

    #[test]
    fn identity_resampler_passes_through() {
        let mut r = Resampler16k::new(16_000).unwrap();
        let x = tone(16_000, 440.0, 0.25);
        let mut out = r.push(&x);
        out.extend(r.flush());
        assert_eq!(out, x);
    }

    #[test]
    fn clip_buffer_caps_at_max_seconds() {
        let mut b = ClipBuffer::new(1);
        assert!(b.push(&vec![0.1; 12_000]));
        assert!(!b.push(&vec![0.1; 12_000]));
        assert_eq!(b.seconds(), 1.0);
        assert_eq!(b.take().len(), 16_000);
        assert_eq!(b.seconds(), 0.0);
    }

    #[test]
    fn level_db_of_silence_is_floor() {
        assert_eq!(level_db(&[0.0; 160]), -100.0);
        assert!(level_db(&[0.5; 160]) > -7.0 && level_db(&[0.5; 160]) < -5.0);
    }

    #[test]
    fn overruns_are_not_fatal_but_device_loss_is() {
        assert!(!stream_error_is_fatal(cpal::ErrorKind::Xrun), "a dropped buffer must not end the recording");
        assert!(stream_error_is_fatal(cpal::ErrorKind::DeviceNotAvailable));
        assert!(stream_error_is_fatal(cpal::ErrorKind::DeviceChanged));
        assert!(stream_error_is_fatal(cpal::ErrorKind::StreamInvalidated));
        assert!(stream_error_is_fatal(cpal::ErrorKind::PermissionDenied));
        assert!(stream_error_is_fatal(cpal::ErrorKind::BackendError));
    }

    #[test]
    #[ignore = "needs a microphone and the permission prompt"]
    fn capture_from_default_device_delivers_16k_audio() {
        use std::sync::{Arc, Mutex};
        let got = Arc::new(Mutex::new(0usize));
        let g = got.clone();
        let cap = Capture::start(None, Box::new(move |pcm| *g.lock().unwrap() += pcm.len())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        cap.stop();
        assert!(*got.lock().unwrap() >= 6_000, "got {}", got.lock().unwrap());
    }
}
