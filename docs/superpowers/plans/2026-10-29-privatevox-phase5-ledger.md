# Phase 5 ledger (2026-10-29)

Live run on the M1: toggle mode, auto-stop 2 s, live preview on, spoken commands on, Raw mode; a 15-second sentence through the speaker ending in "new line".

| Check | Result |
|---|---|
| Live preview | three preview updates logged while speaking ("is a longer sentence to give the live preview something.", "…ng to show while i keep talking…", "and then I stop new line"), each from a 6 s window decode on the free engine |
| Auto-stop | "auto-stop: 2 s of silence after speech" fired by itself; the dictation was transcribed and stored |
| Spoken command | the stored text ends with a line break and the words "new line" are gone |
| Per-app modes, cues | unit-tested; the app in front during a scripted run is not controllable, and sounds are not observable from here |
| MLX | measured; not adopted (`docs/superpowers/spikes/2026-10-29-mlx-spike.md`) |

Rulings: chunked dictations keep decoding with the first spoken language; the preview decodes a 6 s window every 3 s only when the engine is free; "delete that"/"undo" reuse the Undo hotkey's rules.
