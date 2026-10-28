# Phase 4a ledger: host mode and the remote interface (2026-10-28)

Live checks on one Mac, the app serving as host to itself (`host.enabled`, `remote` → `127.0.0.1:8765`), Ollama with qwen2.5:3b as the host's polish LLM:

| Scenario | Result |
|---|---|
| Host on, "use a host" on, dictation through the speaker | Chunk sent to `/v1/dictations`, polish via `/v1/polish`; pill "Polishing on PC…"; client row `engine_kind = remote:PC`, host row `engine_kind = host` with `app_name = MacBookAir`; both polished (`ok`) |
| Host off, "use a host" on | Host never reachable → plain local dictation, row `local`, no false notice |
| Host without an LLM (Ollama was down) | Host answers "could not polish" → both rows `unpolished`, raw text pasted |
| Unit: host dies between chunks | Local decode of the remaining chunks, row `local`, preview "PC unreachable, transcribed locally" |

Findings fixed during the live check: the client held the engine lock while waiting on the host, which deadlocked a host served from the same process (remote transcription now runs ahead of the lock, and a host always does its own work via `with_local_only`); `tiny_http::Server::unblock` wakes one worker per call; the automatic machine name kept its domain suffix.

Not verified by a person yet: a real second machine (the Windows PC) as host; the Network settings card, the history source toggle and the host figures in the strip by hand; mDNS discovery is not built (manual `ip:port` only).
