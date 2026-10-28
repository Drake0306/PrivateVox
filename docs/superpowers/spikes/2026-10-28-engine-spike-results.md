# Engine spike results, 2026-10-28

Machine: Apple M1 (7-core GPU), 8 GB unified memory, macOS 27.0.1. Clips: text-to-speech (Samantha), 16 kHz mono. Each config ran twice; "cold" is the first run after load, "warm" the second. Decode time is wall-clock around the engine call. WER is word-level Levenshtein against the spoken script with case and punctuation ignored; number words versus digits ("seventy five" / "75") count as errors, so the floor is not zero.

| Clip | Spoken length | Words |
|---|---|---|
| clip_30s | 31.6 s | 103 |
| clip_10s | 7.5 s | 24 |
| clip_short | 0.7 s | 1 |

Build: `whisper-rs` 0.16.0 (whisper.cpp, feature `metal`); `transcribe-cpp` 0.4.1 (transcribe.cpp, Metal). Both release builds. Runs were sequential on an otherwise idle machine except where marked.

## large-v3-turbo, whisper-rs (ggml-large-v3-turbo-q5_0.bin, 574 MB)

Load: 482 ms (no flash) / 390 ms (flash). First-ever Metal library init on this machine: 12.4 s, paid once per install.

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| greedy, en | 6109 | 6064 | 2815 | 2841 | 2642 | 2666 | 0.136 | 0.042 |
| greedy, en, flash attention | 4778 | 4873 | 2183 | 2209 | 2057 | 2054 | 0.136 | 0.042 |
| beam 5, en | 7041 | 6989 | 3117 | 3132 | 2697 | 2694 | 0.136 | 0.042 |
| greedy, language auto | 8676 | 8765 | 5615 | 5666 | 5442 | 5479 | 0.136 | 0.042 |
| greedy, en, audio_ctx 768 | n/a | n/a | 1282 | 1277 | 1102 | 1122 | n/a | 0.042 |

Unpadded 0.7 s clip: identical time and text to the padded one (whisper.cpp pads to its 30 s window itself). RSS with two contexts resident: ~1.15 GB, so one turbo q5_0 context is roughly 550 MB of process memory.

## large-v3-turbo, transcribe-cpp (whisper-large-v3-turbo-Q5_K_M.gguf, 620 MB)

Load: 633 ms. First-ever Metal library compile: 10.3 s, once per install. RSS stays ~100 MB because weights are memory-mapped; RSS is not a footprint measure for this engine.

| Config | 30s cold | 30s warm | 10s cold | 10s warm | short cold | short warm | WER 30s | WER 10s |
|---|---|---|---|---|---|---|---|---|
| default, en | 6622 | 5676 | 2768 | 2728 | 2624 | 2639 | 0.058 | 0.042 |
| default, language auto | 6348 | 6321 | 3032 | 3049 | 2878 | 2875 | 0.058 | 0.042 |
| initial prompt "Tauri, LVW.", en | 6781 | 6095 | 2803 | 2810 | 2656 | 2651 | 0.049 | 0.042 |

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
| initial prompt "Tauri, LVW.", en | 9141 | 9361 | 10919 (outlier) | 4265 | 3223 | 3200 | 0.058 | 0.000 |

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
4. **Flash attention is worth 20%** on whisper-rs with no text change.
5. **Language auto-detect costs 2.6 s per clip on whisper-rs** (a second encoder pass) and is nearly free on transcribe-cpp. Fixing the language (§18.7) removes the cost on either.
6. **Beam 5 is 15% slower for no accuracy gain** on these clips. Greedy is the right default.
7. **The 30 s window boundary loses words.** On the 31.6 s clip whisper-rs dropped a whole phrase at the boundary; transcribe-cpp kept it. Chunking at pauses so every run stays under 30 s (§18.6) is an accuracy fix as well as a latency one.
8. **Both wrote "Tori" for "Tauri".** The dictionary / initial prompt path (§18.8) is needed; transcribe-cpp's prompt run did not fix it on its own.

9. **Core ML brings nothing here.** Equal or slower than Metal on an M1 base, and incompatible with reduced audio context. Not used in v1; revisit only for Intel-free machines with a stronger Neural Engine, as an opt-in.
10. **large-v3 is 1.7× slower than turbo** on the same engine (10.6 vs 6.1 s on 30 s; 4.4 vs 2.8 s on 10 s) and was more accurate on one proper noun. It stays the "Accuracy" option, never the default.

## Decision

**Engine for v1: `whisper-rs` 0.16 (whisper.cpp) with the `metal` feature on macOS and `vulkan` on Windows.** Not `transcribe-cpp`. Reasoning, in order of weight:

1. On the plain GPU path the two are equal on turbo and transcribe-cpp is ~15% faster on large-v3, so the §18.2 rule would allow either.
2. whisper-rs exposes two levers transcribe-cpp does not: flash attention (−20% on every clip) and a reduced audio context (2.6 s → 1.1 s on a one-word clip, 2.8 s → 1.3 s on a 7.5 s clip, same text). Dictation is mostly short clips, so this is a 2–2.5× win on the common case. That outweighs transcribe-cpp's bundled VAD and language ID.
3. VAD is covered: whisper-rs 0.16 ships `WhisperVadContext` over whisper.cpp's Silero ggml models.
4. Language ID: v1 defaults to one spoken language (no detection cost). Multi-language users get Whisper's own detection on a short head with reduced audio context, which is cheap; a separate identifier model is not needed in v1.
5. transcribe-cpp remains documented as the alternative engine behind the `Engine` trait if Parakeet or streaming models are wanted later.

**Default decode settings (turbo, "Fast" tier):** greedy, `best_of 1`, flash attention on, language fixed from the spoken-languages setting, `no_context` on, `suppress_blank` and `suppress_nst` on, timestamps off, 4 threads on the M1, `audio_ctx` chosen from clip length (policy pinned by the sweep below). **"Accuracy" tier:** large-v3, same settings, full audio context, still greedy (beam 5 cost 15–70% for no measured gain).

**Core ML:** not built into v1.

**Chunking:** every decode run stays under 30 s of audio (§18.6), which also fixes the boundary word loss seen on the 31.6 s clip.

**Measured footprints (replace §3 estimates):** turbo q5_0 resident context ~550 MB process memory; large-v3 q5_0 ~1.0 GB. GPU buffers on unified memory are included in those numbers. Model files: 574 MB and 1081 MB.

**Measured latencies (replace §11 targets on this M1 8 GB):** turbo, fast tier, one-word clip ~1.1 s, 7.5 s clip ~1.3 s, 30 s clip ~4.8 s (flash, full context). First Metal shader compile after install ~12 s, once.

## Audio-context sweep (turbo, whisper-rs)

_pending_

## LLM polish round trip (Ollama, qwen2.5:3b, this M1)

_pending_
