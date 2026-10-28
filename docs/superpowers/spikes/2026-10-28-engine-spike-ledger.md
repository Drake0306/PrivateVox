# SDD ledger — plan: docs/superpowers/plans/2026-10-28-engine-spike.md

Ruling: executing on main, no worktree — owner is the sole developer, has directed pushes to main and said "go ahead and build"; cost if wrong: a broken main for a spike that is isolated under spike/ anyway.
Pre-flight: Task 3 produces common::{load_clip,wer,rss_mb,Row}; Task 4 consumes the same names — consistent. Task 1 produces clip_*.wav + script_*.txt consumed by Tasks 3 and 4 by directory argument — consistent.
Task 1: complete (commits 6d275d9..6a6a492, tests: bash spike/engine-bench/clips/make_clips.sh → clip_short.wav estimated duration: 0.716562 sec)
Task 2: complete (commits 6a6a492..35cac19, tests: bash -c 'test -x spike/engine-bench/models/fetch.sh' → (no output))
Task 3: smoke run (contended, not the record): turbo greedy_en 30s=7.6s, 10s=3.3s, short=3.2s (fixed ~3 s encoder cost); auto-language +3 s/clip; audio_ctx 768 → 1.2–1.7 s same WER; flash attn −15%; beam5 +10% same WER; first Metal lib init 12.4 s; two contexts RSS 1.1 GB.
Task 3: Ruling: add a Core ML encoder variant (whisper-rs feature `coreml`, separate target dir, encoder .mlmodelc from ggerganov/whisper.cpp renamed to match the q5_0 model name) — the encoder dominates on this M1 and transcribe.cpp has no Core ML path, so the engine decision needs this number — cost if wrong: ~1.2 GB download and one extra build.
Task 3: quiet turbo run recorded in results/whisper-rs-turbo.csv (greedy_en 30s 6.1 s, 10s 2.8 s, short 2.6 s; flash -20%; beam5 +15% same WER; auto +2.6 s; ctx768 1.1-1.3 s).
Task 4: Ruling: transcribe-cpp reports backend "MTL0" not "metal"; guard relaxed to MTL*/metal - cost if wrong: none, it only gates the run.
Task 4: quiet turbo run recorded in results/transcribe-cpp-turbo.csv (default_en 30s 5.7-6.6 s, 10s 2.7 s, short 2.6 s; auto-language ~free; encode 2.85 s / decode 3.76 s on 30s; RSS ~100 MB because weights are mmapped, so RSS is not a footprint measure for this engine).
Task 4: finding: WER gap on the 31.6 s clip is whisper-rs dropping "Please add the numbers to the results table" at the 30 s window boundary (no_context=true, single run); transcribe-cpp kept it. Both wrote "Tori" for "Tauri" (dictionary case). Supports §18.6 chunking at pauses to keep every run under 30 s.
Task 3: whisper-rs large-v3 q5_0 (quiet): greedy_en 30s 10.6 s, 10s 4.4 s, short 3.4 s; flash 8.7/3.6/2.9; beam5 17-19 s; auto +3-8 s; ctx768 2.1/1.35 s; WER 10s 0.000 (v3 got "Vulkan" right, turbo wrote "Vulcan"); ~1.0 GB RSS per v3 context, ~550 MB per turbo context.
Task 3: coreml variant run 1: times identical to Metal (6.0 s / 2.8 s / 2.6 s) and load 8.7 s - encoder likely not picked up; investigating before recording.
Task 3: Ruling: Core ML encoder must be named ggml-large-v3-turbo-encoder.mlmodelc (whisper.cpp strips -q5_0 before appending -encoder); first coreml run is void, rerunning - cost if wrong: one more 3-minute run.
Task 4: quiet large-v3 run recorded in results/transcribe-cpp-v3.csv (default_en 30s 8.8-9.2 s, 10s 3.4-3.5 s, short 2.7 s; ~15% faster than whisper-rs no-flash, level with whisper-rs flash; kept the 30 s boundary phrase).
Task 3: coreml rerun recorded: Core ML loaded, equal or slower than Metal (3.2 vs 2.8 s on 10s), audio_ctx incompatible (garbage). Not used in v1.
Task 5: Ruling: engine = whisper-rs (flash attention + reduced audio_ctx give 2-2.5x on short clips; transcribe-cpp cannot) - cost if wrong: engine swap behind the trait later.
Task 3: ctx sweep: 768 lowest safe on all clips; 512/384 fine on 7.5 s but loop-hallucinate on 0.7 s. Policy clamp(.., 768, 1500) + repetition guard.
Task 5: Ollama qwen2.5:3b: cold 13.4 s (load 10.7 s), warm 1.95 s / 51 tok; 2.2 GB resident; with it resident the next whisper load took 8.6 s (memory pressure on 8 GB).
Task 5: results doc, design doc updated and pushed.
Final: fixed review findings in results doc: memory-pressure claim was a Metal recompile (per-build); finding 8 wrong (prompt fixed Tauri on turbo); flash/beam ranges corrected; fairness caveats added; policy middle region marked untested.
Final: Ruling: two retests added (timestamps on for the 30 s boundary; Core ML and Metal with reused state) - cost if wrong: 6 minutes of GPU.
Task 3: complete (commits 35cac19..61962a9, tests: bash -c 'cd spike/engine-bench && cargo test -p common 2>&1 | grep "test result" | head -1' → test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s)
Task 4: complete (commits c117e32..61962a9, tests: bash -c 'test -s spike/engine-bench/results/transcribe-cpp-v3.csv && echo results present' → results present)
Task 5: complete (commits cbeb6cb..61962a9, tests: bash -c 'grep -c "## Decision" docs/superpowers/spikes/2026-10-28-engine-spike-results.md' → 1)
Final review: fresh subagent reviewed the results doc and bench code; all Important findings fixed in be73488 and this commit; remaining caveats (quantization mismatch, thread/decode asymmetry, untested 13-28 s region) recorded in the doc as deferred.
Final: minor (deferred): rerun the engine comparison with matching quantizations and identical decode settings before any future engine switch.
