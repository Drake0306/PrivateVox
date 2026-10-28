//! Long-form chunking: cut long audio at pauses so every decode stays under 30–40 s and can run while the
//! user is still speaking (design §18.6, ported from Handy's plan). Everything is in samples at 16 kHz.

use std::ops::Range;

const RATE: usize = 16_000;
const TARGET: usize = 30 * RATE;
/// Audio up to this long is decoded in one piece; longer audio is cut.
pub const MAX_CHUNK: usize = 40 * RATE;
const MIN_CHUNK: usize = 5 * RATE;
/// A cut is certain once this much audio past the chunk's full window is scored.
const LOOKAHEAD: usize = MIN_CHUNK;
const THRESHOLD: f32 = 0.5;
const NEG_THRESHOLD: f32 = THRESHOLD - 0.15;
const MIN_SPEECH: usize = 250 * RATE / 1000;
const MIN_SILENCE: usize = 100 * RATE / 1000;
const SPEECH_PAD: usize = 30 * RATE / 1000;

/// Speech spans from per-frame probabilities (Silero's `get_speech_timestamps` without a max length).
pub fn speech_segments(probs: &[f32], frame_samples: usize, samples: usize) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut triggered = false;
    let mut start = 0usize;
    let mut temp_end = 0usize;
    for (i, &p) in probs.iter().enumerate() {
        let t = i * frame_samples;
        if p >= THRESHOLD && temp_end != 0 {
            temp_end = 0;
        }
        if p >= THRESHOLD && !triggered {
            triggered = true;
            start = t;
            continue;
        }
        if p < NEG_THRESHOLD && triggered {
            if temp_end == 0 {
                temp_end = t;
            }
            if t - temp_end < MIN_SILENCE {
                continue;
            }
            if temp_end - start > MIN_SPEECH {
                out.push(start..temp_end);
            }
            triggered = false;
            temp_end = 0;
        }
    }
    if triggered && samples.saturating_sub(start) > MIN_SPEECH {
        out.push(start..samples);
    }
    // Pad and keep segments disjoint.
    let mut padded: Vec<Range<usize>> = Vec::with_capacity(out.len());
    for r in out {
        let mut s = r.start.saturating_sub(SPEECH_PAD);
        let e = (r.end + SPEECH_PAD).min(samples);
        if let Some(prev) = padded.last() {
            s = s.max(prev.end);
        }
        if e > s {
            padded.push(s..e);
        }
    }
    padded
}

/// A place a chunk may end: the middle of a pause, with the pause length as its weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutCandidate {
    pub pos: usize,
    pub pause: usize,
}

/// One candidate per pause between speech spans, lead-in and tail included.
pub fn cut_candidates(segments: &[Range<usize>], samples: usize) -> Vec<CutCandidate> {
    let mut c = Vec::new();
    let mut prev_end = 0usize;
    for seg in segments {
        if seg.start > prev_end {
            c.push(CutCandidate { pos: (prev_end + seg.start) / 2, pause: seg.start - prev_end });
        }
        prev_end = seg.end;
    }
    if samples > prev_end {
        c.push(CutCandidate { pos: (prev_end + samples) / 2, pause: samples - prev_end });
    }
    c
}

fn score(c: &CutCandidate, from: usize) -> f32 {
    let pause_s = c.pause as f32 / RATE as f32;
    let dist_s = (c.pos as f32 - (from + TARGET) as f32).abs() / RATE as f32;
    pause_s * 2.0 - dist_s / 10.0
}

/// Greedy plan from `from`: each cut is the best pause 5–40 s after the previous cut, preferring long pauses
/// near 30 s, leaving at least 5 s after it; with no pause, a hard cut at 40 s, or halfway when the rest
/// would be under 5 s. Audio of 40 s or less (after `from`) is never cut.
pub fn plan_cuts_from(candidates: &[CutCandidate], samples: usize, from: usize) -> Vec<usize> {
    let mut cuts = Vec::new();
    let mut prev = from;
    while samples.saturating_sub(prev) > MAX_CHUNK {
        let best = candidates
            .iter()
            .filter(|c| c.pos >= prev + MIN_CHUNK && c.pos <= prev + MAX_CHUNK && samples - c.pos >= MIN_CHUNK)
            .max_by(|a, b| score(a, prev).partial_cmp(&score(b, prev)).unwrap_or(std::cmp::Ordering::Equal))
            .map(|c| c.pos);
        let cut = match best {
            Some(pos) => pos,
            None => {
                let hard = prev + MAX_CHUNK;
                if samples - hard < MIN_CHUNK {
                    prev + (samples - prev) / 2
                } else {
                    hard
                }
            }
        };
        cuts.push(cut);
        prev = cut;
    }
    cuts
}

/// Plan for a whole clip from its start.
pub fn plan_cuts(segments: &[Range<usize>], samples: usize) -> Vec<usize> {
    plan_cuts_from(&cut_candidates(segments, samples), samples, 0)
}

/// Cuts audio into chunks as its speech probabilities arrive. A chunk is handed back as soon as its cut is
/// certain, so it can be decoded while the user is still speaking. Pushed all at once, a clip is cut exactly
/// as it is live.
#[derive(Debug, Default)]
pub struct Chunker {
    probs: Vec<f32>,
    frame_samples: usize,
    start: usize,
}

impl Chunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the next chunk begins.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Adds probabilities for new frames; `samples_so_far` is the audio length they cover.
    pub fn push(&mut self, probs: &[f32], frame_samples: usize, samples_so_far: usize) -> Vec<Range<usize>> {
        self.frame_samples = frame_samples;
        self.probs.extend_from_slice(probs);
        self.certain(samples_so_far)
    }

    /// Replaces every probability (when the whole clip is rescored from scratch each tick).
    pub fn rescore(&mut self, probs: Vec<f32>, frame_samples: usize, samples_so_far: usize) -> Vec<Range<usize>> {
        self.frame_samples = frame_samples;
        self.probs = probs;
        self.certain(samples_so_far)
    }

    fn certain(&mut self, scored: usize) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        if self.frame_samples == 0 {
            return out;
        }
        let covered = (self.probs.len() * self.frame_samples).min(scored);
        while covered >= self.start + MAX_CHUNK + LOOKAHEAD {
            let segs = speech_segments(&self.probs, self.frame_samples, covered);
            let cands = cut_candidates(&segs, covered);
            let cuts = plan_cuts_from(&cands, covered, self.start);
            let Some(&cut) = cuts.first() else { break };
            out.push(self.start..cut);
            self.start = cut;
        }
        out
    }

    /// The recording stopped: plan the rest and return every remaining chunk, ending at `total`.
    pub fn finish(&mut self, total: usize) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        if total <= self.start {
            return out;
        }
        let segs = if self.frame_samples == 0 { Vec::new() } else { speech_segments(&self.probs, self.frame_samples, total) };
        let cands = cut_candidates(&segs, total);
        for cut in plan_cuts_from(&cands, total, self.start) {
            out.push(self.start..cut);
            self.start = cut;
        }
        out.push(self.start..total);
        self.start = total;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: usize = 16_000;

    fn segs(spans: &[(f32, f32)]) -> Vec<std::ops::Range<usize>> {
        spans.iter().map(|(a, b)| ((a * R as f32) as usize)..((b * R as f32) as usize)).collect()
    }

    #[test]
    fn short_clip_is_never_cut() {
        let s = segs(&[(0.5, 12.0), (12.5, 25.0), (25.4, 39.0)]);
        assert!(plan_cuts(&s, 40 * R).is_empty());
    }

    #[test]
    fn plan_cuts_prefers_long_pause_near_30s() {
        // pauses at 10 s (0.3 s), 28 s (1.2 s), 33 s (0.4 s); total 60 s
        let s = segs(&[(0.0, 10.0), (10.3, 28.0), (29.2, 33.0), (33.4, 60.0)]);
        let cuts = plan_cuts(&s, 60 * R);
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        let cut_s = cuts[0] as f32 / R as f32;
        assert!((28.5..=28.7).contains(&cut_s), "cut should land in the middle of the 28–29.2 s pause, got {cut_s}");
    }

    #[test]
    fn hard_cut_at_40s_without_pause() {
        let s = segs(&[(0.0, 90.0)]);
        let cuts = plan_cuts(&s, 90 * R);
        assert_eq!(cuts, vec![40 * R, 80 * R], "{cuts:?}");
    }

    #[test]
    fn hard_cut_moves_to_halfway_when_rest_would_be_short() {
        let s = segs(&[(0.0, 43.0)]);
        let cuts = plan_cuts(&s, 43 * R);
        assert_eq!(cuts, vec![43 * R / 2]);
    }

    #[test]
    fn incremental_push_matches_batch() {
        // 70 s with pauses every ~12 s
        let mut probs = Vec::new();
        for i in 0..(70 * R / 512) {
            let t = i as f32 * 512.0 / R as f32;
            let in_pause = (t % 12.0) > 11.4;
            probs.push(if in_pause { 0.05 } else { 0.9 });
        }
        let total = probs.len() * 512;
        let mut batch = Chunker::new();
        let mut out_batch: Vec<_> = batch.push(&probs, 512, total);
        out_batch.extend(batch.finish(total));
        let mut live = Chunker::new();
        let mut out_live = Vec::new();
        let mut fed = 0;
        for piece in probs.chunks(7) {
            fed += piece.len() * 512;
            out_live.extend(live.push(piece, 512, fed));
        }
        out_live.extend(live.finish(total));
        assert_eq!(out_live, out_batch);
        assert!(out_batch.len() >= 2);
        assert_eq!(out_batch.first().unwrap().start, 0);
        assert_eq!(out_batch.last().unwrap().end, total);
        for w in out_batch.windows(2) {
            assert_eq!(w[0].end, w[1].start, "chunks must tile the clip");
        }
    }

    #[test]
    fn live_chunks_only_become_certain_with_lookahead() {
        let mut probs = Vec::new();
        for i in 0..(32 * R / 512) {
            let t = i as f32 * 512.0 / R as f32;
            probs.push(if (29.0..29.6).contains(&t) { 0.05 } else { 0.9 });
        }
        let mut c = Chunker::new();
        // 32 s fed: the 40 s window after the chunk start is not fully scored, so nothing is certain yet
        assert!(c.push(&probs, 512, probs.len() * 512).is_empty());
        let more: Vec<f32> = vec![0.9; (46 - 32) * R / 512];
        let total = 32 * R + more.len() * 512;
        let out = c.push(&more, 512, total);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].end > 29 * R && out[0].end < 30 * R, "cut lands in the 29–29.6 s pause: {:?}", out[0]);
    }
}
