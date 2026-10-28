# Phase 3 ledger: LLM polish and LAN failover (2026-10-28)

Measured on the M1 (8 GB) with Ollama 0.40 and qwen2.5:3b, through the real pipeline (speaker → mic → Whisper turbo → polish):

| Case | Polish time | Result |
|---|---|---|
| First dictation, model cold behind the warm-up | 7.4 s | polished (would have hit the 8 s budget on a slower load; cold-start grace added) |
| Model resident, dictation 1 | 0.82 s | polished |
| Model resident, dictation 2 | 1.07 s | polished |
| Warm-up on key press, model resident | 35–98 ms | |
| Ollama stopped | 0 ms | raw text pasted, row `unpolished`, pill "Unpolished · …" |
| Ollama restarted | reachable again within 15 s (health loop) | |

Filler removal before the LLM: "Um, so, uh, hello there." → "So, hello there." (raw column), "Hello there." after Clean.

Not verified by a person yet: the LAN path against the RX 9070 PC (built and unit-tested with a fake server; needs the PC on), the Re-polish control and the Polish settings page by hand in the panel, pasting into an app (Accessibility is not granted on the dev Mac, so every run ended in "Copied (paste blocked)").
