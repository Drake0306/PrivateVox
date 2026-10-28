# LVW Engine Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure, on this M1 8 GB Mac, how fast and how accurate large-v3-turbo and large-v3 are through `whisper-rs` and through `transcribe-cpp`, so the v1 plan can fix the engine crate, the default decode settings and the memory numbers.

**Architecture:** One throwaway Cargo workspace under `spike/engine-bench/` with two binaries that share a tiny `common` crate (clip loading, word error rate, timing, RSS). Each binary loads a model, runs a fixed set of clips under a fixed set of decode configurations, and prints one CSV line per run. A results doc records the table and the decision.

**Tech Stack:** Rust 1.93, `whisper-rs` 0.16 (feature `metal`), `transcribe-cpp` 0.4.1 (feature `metal`, default), `hound` for WAV, macOS `say` and `afconvert` for the clips.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §3, §11, §13 step 1, §18.2, §18.9.

## Global Constraints

- Spike code lives under `spike/` and is never imported by the app. It may be deleted after the results doc is written.
- Models download to `~/.cache/lvw-spike/models/`, not into the repo. Total about 3.4 GB; the machine has 26 GB free.
- Audio is 16 kHz mono f32 in [-1, 1], the format both crates require.
- Every measured number goes in `docs/superpowers/spikes/2026-10-28-engine-spike-results.md`; nothing is reported from memory.
- Decision rule from §18.2: if transcribe-cpp is within 15% of whisper-rs on decode time for the 10 s and 30 s clips with turbo, transcribe-cpp is the v1 default engine.
- No commit attribution trailers.

## Review Focus

- A clip shorter than 1 s: both engines must return without panicking (feeds §18.5's padding decision). Pinned in Task 3 and Task 4 by the 0.6 s clip.
- A run with `language = None` versus `Some("en")`: the difference in decode time is the cost of Whisper's own language detection (§18.7). Pinned in Task 3's config matrix.
- Cold first run after load versus warm second run: the first Metal run pays shader and buffer setup; both are recorded so §11's targets separate load from steady state. Pinned in Tasks 3 and 4 by running every config twice.
- Memory: process RSS after load and after the first decode, per model, so the §9 strip has measured footprints instead of estimates. Pinned by the `rss_mb` column.
- Word error rate against the known script, so a "fast" config that mangles text is visible. Pinned by the `wer` column.

---

### Task 1: Benchmark clips and reference script

**Files:**
- Create: `spike/engine-bench/clips/make_clips.sh`
- Create: `spike/engine-bench/clips/script_30s.txt`, `script_10s.txt`, `script_short.txt`
- Output (git-ignored): `spike/engine-bench/clips/*.wav`

**Interfaces:**
- Produces: `clips/clip_30s.wav`, `clips/clip_10s.wav`, `clips/clip_short.wav` (16 kHz mono 16-bit PCM WAV) and the matching `script_*.txt` reference text, one sentence per line.

- [ ] **Step 1: Write the three scripts.** `script_30s.txt` is about 75 words of ordinary dictation containing two numbers, one proper noun ("Tauri") and one question. `script_10s.txt` is about 25 words. `script_short.txt` is the single word "Hello." (under 1 s when spoken).

- [ ] **Step 2: Write `make_clips.sh`.** For each script: `say -v Samantha -r 175 -o tmp.aiff -f script.txt` then `afconvert -f WAVE -d LEI16@16000 -c 1 tmp.aiff clip.wav`. Print each clip's duration with `afinfo clip.wav | grep "estimated duration"`.

- [ ] **Step 3: Run it.** Run: `bash spike/engine-bench/clips/make_clips.sh`. Expected: three WAV files; durations roughly 30 s, 10 s and under 1 s printed.

- [ ] **Step 4: Add `spike/engine-bench/clips/*.wav` and `spike/engine-bench/target/` to `.gitignore`. Commit.**

```bash
git add .gitignore spike/engine-bench/clips/make_clips.sh spike/engine-bench/clips/*.txt
git commit -m "spike: benchmark clips and reference scripts"
```

### Task 2: Model download script

**Files:**
- Create: `spike/engine-bench/models/fetch.sh`

**Interfaces:**
- Produces, under `~/.cache/lvw-spike/models/`:
  - `ggml-large-v3-turbo-q5_0.bin` (574 MB) and `ggml-large-v3-q5_0.bin` (1081 MB) from `https://huggingface.co/ggerganov/whisper.cpp/resolve/main/`
  - `whisper-large-v3-turbo-Q5_K_M.gguf` (620 MB) from `https://huggingface.co/handy-computer/whisper-large-v3-turbo-gguf/resolve/main/`
  - `whisper-large-v3-Q5_K_M.gguf` (1161 MB) from `https://huggingface.co/handy-computer/whisper-large-v3-gguf/resolve/main/`

- [ ] **Step 1: Write `fetch.sh`** using `curl -L -C - -o <file> <url>` for each of the four files, skipping files whose size already matches, and printing the final `ls -la` of the directory.

- [ ] **Step 2: Run it in the background and move on.** Run: `bash spike/engine-bench/models/fetch.sh`. Expected: four files with the sizes above.

- [ ] **Step 3: Commit.**

```bash
git add spike/engine-bench/models/fetch.sh
git commit -m "spike: model fetch script"
```

### Task 3: whisper-rs benchmark binary

**Files:**
- Create: `spike/engine-bench/Cargo.toml` (workspace: `common`, `bench-whisper-rs`, `bench-transcribe-cpp`)
- Create: `spike/engine-bench/common/Cargo.toml`, `common/src/lib.rs`
- Create: `spike/engine-bench/bench-whisper-rs/Cargo.toml`, `bench-whisper-rs/src/main.rs`

**Interfaces:**
- `common` produces:
  - `pub fn load_clip(path: &Path) -> Vec<f32>` (reads 16 kHz mono 16-bit WAV with `hound`, scales to f32)
  - `pub fn wer(reference: &str, hypothesis: &str) -> f32` (lowercase, strip punctuation, word-level Levenshtein / reference word count)
  - `pub fn rss_mb() -> f64` (via `libc::proc_pidinfo`/`mach_task_basic_info`, or parse `ps -o rss= -p $$`)
  - `pub struct Row { engine, model, clip, config, run: u8, load_ms, decode_ms, rss_mb, wer, text }` with `pub fn csv_header() -> &'static str` and `pub fn to_csv(&self) -> String`
- `bench-whisper-rs` consumes `common`; takes args `<model.bin> <clips_dir> <scripts_dir>`; prints CSV rows to stdout.

- [ ] **Step 1: Write `common` with a unit test for `wer`.** Test: `wer("hello world foo", "hello world")` is `0.333…`; `wer("a b", "a b")` is `0.0`; punctuation and case are ignored.

- [ ] **Step 2: Run it.** Run: `cargo test -p common` (in `spike/engine-bench`). Expected: PASS.

- [ ] **Step 3: Write `bench-whisper-rs/src/main.rs`.** Dependencies: `whisper-rs = { version = "0.16", features = ["metal"] }`. Load once with `WhisperContextParameters` `use_gpu(true)`, `flash_attn(cfg.flash)`; record `load_ms` and `rss_mb`. Configs, each run twice (cold, warm) on each clip:

  | config id | SamplingStrategy | flash_attn | language | audio_ctx | notes |
  |---|---|---|---|---|---|
  | `greedy_en` | `Greedy { best_of: 1 }` | false | `Some("en")` | default | baseline |
  | `greedy_en_flash` | `Greedy { best_of: 1 }` | true | `Some("en")` | default | needs a second context load; record its own load_ms |
  | `beam5_en` | `BeamSearch { beam_size: 5, patience: -1.0 }` | false | `Some("en")` | default | accuracy tier candidate |
  | `greedy_auto` | `Greedy { best_of: 1 }` | false | `None` | default | cost of Whisper's own language detection |
  | `greedy_en_ctx768` | `Greedy { best_of: 1 }` | false | `Some("en")` | 768 | 10 s and short clips only |

  Common `FullParams` for every run: `set_n_threads(4)`, `set_no_context(true)`, `set_single_segment(false)`, `set_suppress_blank(true)`, `set_suppress_nst(true)`, all `set_print_*` false, `set_no_timestamps(true)`. Text is the concatenation of `state.as_iter()` segments' `to_str_lossy()`. Pad the short clip to 1.25 s of samples before decoding, as §18.5 says, and also run it unpadded once so the panic/behaviour difference is visible in the row's `text`.

- [ ] **Step 4: Build and run on turbo.** Run: `cargo run --release -p bench-whisper-rs -- ~/.cache/lvw-spike/models/ggml-large-v3-turbo-q5_0.bin clips clips > results/whisper-rs-turbo.csv`. Expected: a CSV with `5 configs × 3 clips × 2 runs` rows (minus the ctx768 rows for the 30 s clip), no panics, `wer` under 0.10 for `greedy_en` on the 30 s clip.

- [ ] **Step 5: Run on large-v3.** Same command with `ggml-large-v3-q5_0.bin` to `results/whisper-rs-v3.csv`.

- [ ] **Step 6: Commit** (`results/*.csv` are committed; they are the deliverable).

```bash
git add spike/engine-bench
git commit -m "spike: whisper-rs benchmark and results"
```

### Task 4: transcribe-cpp benchmark binary

**Files:**
- Create: `spike/engine-bench/bench-transcribe-cpp/Cargo.toml`, `bench-transcribe-cpp/src/main.rs`

**Interfaces:**
- Consumes `common` (Task 3).
- Same CLI and CSV shape as Task 3 with `engine = "transcribe-cpp"`.

- [ ] **Step 1: Write `main.rs`.** Dependency: `transcribe-cpp = "0.4.1"` (default feature `metal`). Load with `Model::load_with(path, &ModelOptions { backend: Backend::Metal, ..Default::default() })`; print `model.backend()` and fail the run if it is not `"metal"`. `let mut session = model.session()?`. Configs, each run twice on each clip:

  | config id | RunOptions |
  |---|---|
  | `default_en` | `language: Some("en")`, rest `Default` |
  | `default_auto` | `language: None`, rest `Default` |
  | `prompt_en` | `language: Some("en")`, `family: Some(RunExtension::Whisper(WhisperRunOptions { initial_prompt: Some("Tauri, LVW."), ..Default::default() }))` |

  `decode_ms` is wall time around `session.run(&pcm, &opts)`; also print `transcript.timings` in a `notes` column. Text is `transcript.text`. Short clip padded to 1.25 s plus one unpadded run, as in Task 3.

- [ ] **Step 2: Build and run on turbo and on large-v3.** Run: `cargo run --release -p bench-transcribe-cpp -- ~/.cache/lvw-spike/models/whisper-large-v3-turbo-Q5_K_M.gguf clips clips > results/transcribe-cpp-turbo.csv`, then the large-v3 GGUF to `results/transcribe-cpp-v3.csv`. Expected: CSVs with `3 configs × 3 clips × 2 runs` rows, backend `metal`, no panics.

- [ ] **Step 3: Commit.**

```bash
git add spike/engine-bench
git commit -m "spike: transcribe-cpp benchmark and results"
```

### Task 5: Results and decision

**Files:**
- Create: `docs/superpowers/spikes/2026-10-28-engine-spike-results.md`
- Modify: `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §3 (memory table), §11 (targets), §18.2 (engine decision)

- [ ] **Step 1: Write the results doc.** One table per clip length with columns engine, model, config, cold decode ms, warm decode ms, load ms, RSS MB after load, WER. Below it, four one-line findings: (a) turbo versus large-v3 speed ratio, (b) transcribe-cpp versus whisper-rs on `greedy_en`/`default_en` for 10 s and 30 s, (c) greedy versus beam 5 time and WER, (d) `Some("en")` versus `None`. Then the decision per the Global Constraints rule, and the measured footprints that replace §3's estimates.

- [ ] **Step 2: Update the design doc** with the measured numbers and the engine decision, marking each changed line **[measured 2026-10-28]**.

- [ ] **Step 3: Commit and push.**

```bash
git add docs/superpowers/spikes docs/superpowers/specs
git commit -m "spike: engine results and design updates"
git push origin main
```
