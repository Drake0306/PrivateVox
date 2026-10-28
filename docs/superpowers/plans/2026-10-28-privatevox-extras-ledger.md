# Phase 4b ledger: extras, Mac scope (2026-10-28)

Live run on the M1 (dictionary `ChargeBee, Kubernetes`, keep audio on, spoken languages `en, de`, host mode on, Raw mode), sentence played through the speaker: "Please talk to charge bee about the kubernetes cluster tomorrow."

| Check | Result |
|---|---|
| Dictionary fuzzy pass | Whisper heard "charge bee" → pasted "ChargeBee"; "cubanese" (Whisper's mishearing of the synthetic voice) was left alone, as it is four edits from "Kubernetes" |
| Spoken languages `en, de` | decoded with auto-detect, detected `en` (on the list), row `language = en`, `language_source = detected` |
| Keep audio | `audio/34.wav` written next to the row (190 KB, 5.9 s) |
| mDNS | host advertised `MacBookAir._privatevox._tcp.local.`; in-process advertise + browse round trip found it on the LAN address |
| Receipt-sequenced restore | live: dictated into a TextEdit document; the text landed there and the clipboard read "OLD CLIPBOARD" again afterwards (restore after the app's read, no timer) |
| Secure-input warning | implemented with unit tests; needs a person: turn on Terminal's Secure Keyboard Entry and watch the pill |

Not built: notarised .dmg (needs an Apple Developer account); the Windows pass and the RX 9070 timing (need the PC).

## Final review and fix pass

Two Critical and eight Important findings, all fixed with a failing test first: the fuzzy pass rewrote everyday words near six-letter names and glued real words into names (now: near-misses need eight letters and matching Soundex; stop words never join); the paste timeout restored the old clipboard and reported a paste (now the transcript stays and the row says copied-only, per §18.11); clipboard managers could count as the receipt (concealment types declared); the language re-run corrupted the host's session; Re-run broke a live recording, left the model column stale and changed the active model; the language filter dropped hidden selections; the secure-input warning hard-coded the combo. Four minors fixed in passing; the rest are listed in the session summary.
