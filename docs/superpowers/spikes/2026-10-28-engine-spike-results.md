# Engine spike results, 2026-10-28

Machine: Apple M1 (7-core GPU), 8 GB unified memory, macOS 27.0.1. Clips: text-to-speech (Samantha), 16 kHz mono. Each config ran twice; "cold" is the first run after load, "warm" the second. Decode time is wall-clock around the engine call. WER is word-level Levenshtein against the spoken script with case and punctuation ignored; number words versus digits ("seventy five" / "75") count as errors, so the floor is not zero.

| Clip | Spoken length | Words |
|---|---|---|
| clip_30s | 31.6 s | 103 |
| clip_10s | 7.5 s | 24 |
| clip_short | 0.7 s | 1 |

Build: `whisper-rs` 0.16.0 (whisper.cpp, feature `metal`); `transcribe-cpp` 0.4.1 (transcribe.cpp, Metal). Both release builds. Runs were sequential on an otherwise idle machine except where marked.

Fairness caveats (from review): the two engines ran different quantizations of the same weights (q5_0 vs Q5_K_M); whisper-rs was given 4 threads and explicit decode settings (greedy, no_context, no timestamps, suppress non-speech) while transcribe-cpp ran its library defaults; the first Core ML table created a fresh decode state per call, which makes every Core ML run a cold one (a retest with a reused state is below).

## large-v3-turbo, whisper-rs (ggml-large-v3-turbo-q5_0.bin, 574 MB)

Load: 482 ms (no flash) / 390 ms (flash). First Metal library init for a given binary: 12.4 s (observed in the contended smoke run); it recurs after every rebuild of the binary, so for users it is once per app update.

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| greedy, en | 6109 | 6064 | 2815 | 2841 | 2642 | 2666 | 0.136 | 0.042 |
| greedy, en, flash attention | 4778 | 4873 | 2183 | 2209 | 2057 | 2054 | 0.136 | 0.042 |
| beam 5, en | 7041 | 6989 | 3117 | 3132 | 2697 | 2694 | 0.136 | 0.042 |
| greedy, language auto | 8676 | 8765 | 5615 | 5666 | 5442 | 5479 | 0.136 | 0.042 |
| greedy, en, audio_ctx 768 | n/a | n/a | 1282 | 1277 | 1102 | 1122 | n/a | 0.042 |

Unpadded 0.7 s clip: identical time and text to the padded one (whisper.cpp pads to its 30 s window itself). RSS with two contexts resident: ~1.15 GB, so one turbo q5_0 context is roughly 550 MB of process memory.

## large-v3-turbo, transcribe-cpp (whisper-large-v3-turbo-Q5_K_M.gguf, 620 MB)

Load: 633 ms. First Metal library compile for this binary: 10.3 s (observed in a run whose log was later overwritten). RSS stays ~100 MB because weights are memory-mapped; RSS is not a footprint measure for this engine.

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| default, en | 6622 | 5676 | 2768 | 2728 | 2624 | 2639 | 0.058 | 0.042 |
| default, language auto | 6348 | 6321 | 3032 | 3049 | 2878 | 2875 | 0.058 | 0.042 |
| initial prompt "Tauri, PrivateVox.", en | 6781 | 6095 | 2803 | 2810 | 2656 | 2651 | 0.049 | 0.042 |

Engine-reported stage timings for the 30 s clip: mel 9 ms, encode 2855 ms, decode 3758 ms.

## large-v3 (ggml-large-v3-q5_0.bin, 1081 MB), whisper-rs

Load: 1305 ms (no flash) / 1178 ms (flash). RSS per resident large-v3 context: ~1.0 GB (two contexts: 2.08 GB).

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| greedy, en | 10870 | 10576 | 4354 | 4404 | 3331 | 3496 | 0.136 | 0.000 |
| greedy, en, flash attention | 8793 | 8670 | 3913 | 3565 | 2916 | 2853 | 0.136 | 0.000 |
| beam 5, en | 18823 | 17011 | 5758 | 5835 | 3438 | 3545 | 0.136 | 0.000 |
| greedy, language auto | 13296 | 19101 | 8259 | 7620 | 6814 | 6181 | 0.136 | 0.000 |
| greedy, en, audio_ctx 768 | n/a | n/a | 2119 | 2116 | 1348 | 1374 | n/a | 0.000 |

large-v3 got "Vulkan" right where turbo wrote "Vulcan" (the only difference on the 10 s clip). On the 30 s clip both lose the same phrase at the window boundary.

## large-v3, transcribe-cpp (whisper-large-v3-Q5_K_M.gguf, 1161 MB)

Load: 514 ms. RSS ~350–400 MB (weights memory-mapped, so not a footprint measure).

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| default, en | 9195 | 8826 | 3516 | 3405 | 2700 | 2706 | 0.058 | 0.000 |
| default, language auto | 9009 | 8610 | 4013 | 4002 | 3334 | 3311 | 0.058 | 0.000 |
| initial prompt "Tauri, PrivateVox.", en | 9141 | 9361 | 10919 (outlier) | 4265 | 3223 | 3200 | 0.058 | 0.000 |

On large-v3 transcribe-cpp is about 15% faster than whisper-rs without flash attention (8.8 vs 10.6 s on 30 s, 3.4 vs 4.4 s on 10 s) and level with whisper-rs plus flash attention (8.7 / 3.6 s). It again kept the phrase at the 30 s boundary that whisper-rs dropped.

## large-v3-turbo with the Core ML encoder (whisper-rs, feature `coreml`)

Encoder: `ggml-large-v3-turbo-encoder.mlmodelc` (1.2 GB extracted). whisper.cpp strips the quantization suffix when it looks for the encoder, so the file keeps its original name beside `ggml-large-v3-turbo-q5_0.bin`. The log confirms "Core ML model loaded" for every context. Load with the encoder: 517 ms (first-ever load on this machine 8.7 s while the Neural Engine compiled it).

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 10s |
|---|---|---|---|---|---|---|---|
| greedy, en | 8665 | 6440 | 3328 | 3211 | 3048 | 3152 | 0.042 |
| greedy, en, flash attention | 6020 | 6091 | 3472 | 3330 | 4438 | 3688 | 0.042 |
| greedy, language auto | 7965 | 7961 | 5161 | 5183 | 5337 | 5304 | 0.042 |
| greedy, en, audio_ctx 768 | n/a | n/a | 157 | 137 | 150 | 138 | **0.958 (garbage)** |

Core ML is not faster than Metal on this M1 for the turbo encoder (3.2 vs 2.8 s on the 10 s clip, 6.4 vs 6.1 s warm on the 30 s clip), it costs a 1.2 GB download and a one-time multi-second compile, and it silently breaks the reduced audio context: the fixed-shape Core ML encoder ignores `audio_ctx` and returns nonsense in 150 ms. Part of this run overlapped an Ollama model download (network and disk only), which does not explain a slowdown of this size.

## Findings so far

1. **The encoder is the floor.** A 0.7 s clip costs 2.6 s on both engines, the same as a 7.5 s clip. Whisper always encodes a 30 s window, and on this GPU that is ~2.8 s. Nothing in decode settings changes that; only a faster encoder path (Core ML on the Neural Engine) or a shorter audio context does.
2. **whisper-rs and transcribe-cpp are equal on the GPU path.** Within noise on every clip (6.1 vs 5.7–6.6 s, 2.8 vs 2.7 s, 2.6 vs 2.6 s). The 15% rule of §18.2 is met.
3. **Audio context 768 halves short-clip time** (2.8 s → 1.3 s on the 7.5 s clip, 2.6 s → 1.1 s on the 0.7 s clip) with identical text. Only whisper-rs exposes this. Risk: the setting is marked experimental in whisper.cpp and must never be used on clips longer than the reduced window covers (768 of 1500 ≈ 15 s).
4. **Flash attention is worth about 20% on turbo** (22% on the 30 s clip) and 10–12% on large-v3, with no text change.
5. **Language auto-detect costs 2.6 s per clip on whisper-rs** (a second encoder pass) and is nearly free on transcribe-cpp. Fixing the language (§18.7) removes the cost on either.
6. **Beam 5 is 15% slower on turbo and 30–70% slower on large-v3** for no accuracy gain on these clips. Greedy is the right default.
7. **The 30 s window boundary loses words only with timestamps disabled.** On the 31.6 s clip whisper-rs with `no_timestamps(true)` dropped a whole phrase at the boundary; the retest with timestamps on kept it (WER 0.136 → 0.058, same as transcribe-cpp) at no time cost (6.2 vs 6.1 s). whisper.cpp advances by a full window when it has no segment timestamps. **Decode default changed: timestamps stay on.** Chunking at pauses (§18.6) remains the latency fix for long clips.
8. **Both wrote "Tori" for "Tauri" without a prompt.** With the initial prompt "Tauri, PrivateVox." transcribe-cpp wrote "Tauri" correctly on turbo (WER 0.058 → 0.049, exactly that word) but not on large-v3. The dictionary / initial prompt path (§18.8) works and is needed.

9. **Core ML is not worth its cost here.** Measured fairly (state reused) it is 10–15% faster than Metal without flash attention and level with it, uses ~1 GB more memory, and is incompatible with the reduced audio context that gives 2×. Not used in v1; revisit as an opt-in on machines with a stronger Neural Engine.
10. **large-v3 is 1.7× slower than turbo** on the same engine (10.6 vs 6.1 s on 30 s; 4.4 vs 2.8 s on 10 s) and was more accurate on one proper noun. It stays the "Accuracy" option, never the default.

## Retests after review

**Timestamps on (whisper-rs, turbo, greedy, en):** 30 s clip 6241 / 6201 ms, WER 0.058 (full text kept); 7.5 s clip 2866 / 2860 ms; 0.7 s clip 2693 / 2742 ms. Same speed as timestamps off, boundary loss gone.

**Core ML and Metal with one decode state reused per context** (the first Core ML table created a fresh state per call, which made every Core ML run cold):

| Config | Core ML 30s | Metal 30s | Core ML 10s | Metal 10s | Core ML short | Metal short | RSS Core ML | RSS Metal |
|---|---|---|---|---|---|---|---|---|
| greedy, en (warm) | 5009 | 5981 | 2403 | 2741 | 2086 | 2556 | 2.2 GB | 1.3 GB |
| greedy, en, flash (warm) | 4877 | 4827 | 2180 | 2158 | 2009 | 2282 | 2.2 GB | 1.3 GB |

With the state reused Core ML is 10–15% faster than Metal without flash attention and level with Metal plus flash attention, costs about 1 GB more resident memory and a 1.2 GB download, and still cannot run a reduced audio context. The decision below stands; the numbers are now fair.

## Decision

**Engine for v1: `whisper-rs` 0.16 (whisper.cpp) with the `metal` feature on macOS and `vulkan` on Windows.** Not `transcribe-cpp`. Reasoning, in order of weight:

1. On the plain GPU path the two are equal on turbo and transcribe-cpp is ~15% faster on large-v3, so the §18.2 rule would allow either.
2. whisper-rs exposes two levers transcribe-cpp does not (its `WhisperRunOptions` carries only prompt, temperature fallback and thresholds; see the crate reference): flash attention (−20% on turbo) and a reduced audio context (2.6 s → 1.1 s on a one-word clip, 2.8 s → 1.3 s on a 7.5 s clip, same text). Dictation is mostly short clips, so this is a 2–2.5× win on the common case. That outweighs transcribe-cpp's bundled VAD and language ID.
3. VAD is covered: whisper-rs 0.16 ships `WhisperVadContext` over whisper.cpp's Silero ggml models.
4. Language ID: v1 defaults to one spoken language (no detection cost). Multi-language users get Whisper's own detection on a short head with reduced audio context, which is cheap; a separate identifier model is not needed in v1.
5. transcribe-cpp remains documented as the alternative engine behind the `Engine` trait if Parakeet or streaming models are wanted later.

**Default decode settings (turbo, "Fast" tier):** greedy, `best_of 1`, flash attention on, language fixed from the spoken-languages setting, `no_context` on, `suppress_blank` and `suppress_nst` on, **timestamps on** (see retest), 4 threads on the M1, `audio_ctx` from clip length: `clamp(round_up_64(1500 × (seconds + 2) / 30), 768, 1500)` with a repetition guard (sweep below). **"Accuracy" tier:** large-v3, same settings, full audio context, still greedy (beam 5 cost 15–70% for no measured gain).

**Core ML:** not built into v1 (retested with a reused state: at best 10–15% without flash attention, nothing with it, +1 GB memory, incompatible with the audio-context lever).

**Chunking:** every decode run stays under 30 s of audio (§18.6), which also fixes the boundary word loss seen on the 31.6 s clip.

**Measured footprints (replace §3 estimates):** turbo q5_0 resident context ~550 MB process memory; large-v3 q5_0 ~1.0 GB. GPU buffers on unified memory are included in those numbers. Model files: 574 MB and 1081 MB.

**Measured latencies (replace §11 targets on this M1 8 GB):** turbo, fast tier, one-word clip ~1.1 s, 7.5 s clip ~1.3 s, 30 s clip ~4.8 s (flash, full context). First Metal shader compile after install ~12 s, once.

## Audio-context sweep (turbo, whisper-rs, greedy, en)

`audio_ctx` is the number of encoder positions whisper.cpp runs; 1500 covers the full 30 s window, so 768 covers about 15 s and 512 about 10 s. Run while Ollama held a 2.2 GB model resident, so absolute times are slightly worse than the quiet turbo table; the pattern is what matters.

| audio_ctx | 7.5 s clip (ms, warm) | text | 0.7 s clip (ms, warm) | text |
|---|---|---|---|---|
| 1500 (full) | 2766 | correct | 2567 | correct |
| 1024 | 1579 | correct | 1400 | correct |
| 768 | 1258 | correct | 1078 | correct |
| 768 + flash | 1112 | correct | 986 | correct |
| 512 | 832 | correct | 6104 | **"Hello." repeated 7 times** |
| 384 | 670 | correct | 9104 | **"Hello." repeated 14 times** |

Two things follow. The context can go well below the clip's duration without harming text on a 7.5 s clip (384 still correct, 4× faster than full). But a very short clip with a small context hallucinates a repetition loop and takes longer than the full context did. 768 is the lowest value that was correct on everything tested.

**Policy for v1:** `audio_ctx = clamp(round_up_64(1500 × (clip_seconds + 2) / 30), 768, 1500)`. So anything under 13 s runs at 768 (about 1.0–1.3 s on this M1 with flash attention), 20 s at 1152, 28 s and above at 1500. The 13–28 s region of the formula is an interpolation, not a measurement: every reduced-context run used the 7.5 s or 0.7 s clip. The chunked-decode plan adds a sweep on 15 s and 25 s clips before that region is trusted. Plus a repetition guard: if the output contains the same word four or more times in a row, re-run once at 1500 before accepting it. The lower floor (512) is left for a later experiment with more clip lengths between 1 and 7 s.

## LLM polish round trip (Ollama 0.40, qwen2.5:3b Q4_K_M, this M1)

Clean-mode system prompt, 104 prompt tokens (a 43-word filler-laden transcript), 51 output tokens.

| Run | Wall | Model load | Prompt eval | Generation |
|---|---|---|---|---|
| Cold (model not loaded) | 13369 ms | 10730 ms | 718 ms | 1849 ms |
| Warm | 1950 ms | 1 ms | 39 ms | 1862 ms |

Resident size reported by Ollama: 2164 MB. Generation runs at ~27 tokens/s, so a 100-word cleaned output costs about 4–5 s on this machine and a 40-word one about 2 s. The 10.7 s cold load is the number that makes §18's LLM warm-up (fire on hotkey down, keep-alive while Whisper is warm) a requirement rather than a nicety on the 8 GB Mac.

**Memory interaction on 8 GB:** none measured. The 8.6 s context load seen during the sweep (with the 3B model resident) was a Metal shader-library recompile (`ggml_metal_library_init: loaded in 8.083 sec` in the sweep log), not memory pressure: the bench binary had just been rebuilt, and macOS keys its shader cache to the binary. Decode times in the sweep match the quiet run. Correction to an earlier line in this doc: the Metal compile is paid once per *build or update of the app binary*, not once per install.
