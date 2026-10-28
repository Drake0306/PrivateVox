# Phase 4b ledger: extras, Mac scope (2026-10-28)

Live run on the M1 (dictionary `ChargeBee, Kubernetes`, keep audio on, spoken languages `en, de`, host mode on, Raw mode), sentence played through the speaker: "Please talk to charge bee about the kubernetes cluster tomorrow."

| Check | Result |
|---|---|
| Dictionary fuzzy pass | Whisper heard "charge bee" → pasted "ChargeBee"; "cubanese" (Whisper's mishearing of the synthetic voice) was left alone, as it is four edits from "Kubernetes" |
| Spoken languages `en, de` | decoded with auto-detect, detected `en` (on the list), row `language = en`, `language_source = detected` |
| Keep audio | `audio/34.wav` written next to the row (190 KB, 5.9 s) |
| mDNS | host advertised `MacBookAir._privatevox._tcp.local.`; in-process advertise + browse round trip found it on the LAN address |
| Receipt-sequenced restore, secure-input warning | implemented with unit tests; need a person: paste into TextEdit and press ⌘V again (old clipboard returns); turn on Terminal's Secure Keyboard Entry (pill warns) |

Not built: notarised .dmg (needs an Apple Developer account); the Windows pass and the RX 9070 timing (need the PC).
