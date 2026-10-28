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

_pending_

## large-v3-turbo with the Core ML encoder (whisper-rs, feature `coreml`)

_pending: encoder download and first Neural Engine compile_

## Findings so far

1. **The encoder is the floor.** A 0.7 s clip costs 2.6 s on both engines, the same as a 7.5 s clip. Whisper always encodes a 30 s window, and on this GPU that is ~2.8 s. Nothing in decode settings changes that; only a faster encoder path (Core ML on the Neural Engine) or a shorter audio context does.
2. **whisper-rs and transcribe-cpp are equal on the GPU path.** Within noise on every clip (6.1 vs 5.7–6.6 s, 2.8 vs 2.7 s, 2.6 vs 2.6 s). The 15% rule of §18.2 is met.
3. **Audio context 768 halves short-clip time** (2.8 s → 1.3 s on the 7.5 s clip, 2.6 s → 1.1 s on the 0.7 s clip) with identical text. Only whisper-rs exposes this. Risk: the setting is marked experimental in whisper.cpp and must never be used on clips longer than the reduced window covers (768 of 1500 ≈ 15 s).
4. **Flash attention is worth 20%** on whisper-rs with no text change.
5. **Language auto-detect costs 2.6 s per clip on whisper-rs** (a second encoder pass) and is nearly free on transcribe-cpp. Fixing the language (§18.7) removes the cost on either.
6. **Beam 5 is 15% slower for no accuracy gain** on these clips. Greedy is the right default.
7. **The 30 s window boundary loses words.** On the 31.6 s clip whisper-rs dropped a whole phrase at the boundary; transcribe-cpp kept it. Chunking at pauses so every run stays under 30 s (§18.6) is an accuracy fix as well as a latency one.
8. **Both wrote "Tori" for "Tauri".** The dictionary / initial prompt path (§18.8) is needed; transcribe-cpp's prompt run did not fix it on its own.

## Decision

_pending the large-v3 and Core ML numbers_
