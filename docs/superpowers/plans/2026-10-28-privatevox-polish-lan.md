# PrivateVox Phase 3: LLM Polish and LAN Failover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn raw transcripts into clean text with a local or LAN LLM, chosen automatically by health, with modes, re-polish from history, warm-up on key press, and the Polish LLM segment of the memory strip.

**Architecture:** A new `polish` module in `privatevox-core` holds the pure parts (modes and prompts, request/response shapes for Ollama and OpenAI-compatible servers, the endpoint registry with health and backoff selection) and a blocking HTTP client behind an `LlmClient` trait so the orchestration is testable against a tiny in-process HTTP server. The Tauri app owns one `Polisher` shared between the pipeline (polish after decode, warm on key down, unload with Whisper), a 10 s health loop, the memory report, and the panel commands. Filler removal joins `textnorm` and runs before history save and before the LLM.

**Tech Stack:** Rust 2021, reqwest 0.13 (`blocking` + `json` features), serde_json, rusqlite (schema v2 migration), Tauri 2 commands/events, vanilla JS panel.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §4.1, §4.3 step 5, §4.4, §4.5, §4.7, §6, §7, §8.1–8.2, §9, §18.9.

## Global Constraints

- Modes shipped: Raw (no LLM), Clean (default), Email, Bullets, Code, Translate (Whisper-native, no LLM), Ask. Prompts editable, with reset to default. (§7)
- Request: OpenAI-compatible `POST {base_url}/v1/chat/completions`, non-streaming, `temperature 0.2`, `max_tokens = 2 × input tokens + 64`. Model name per endpoint, chosen from what the server lists. (§7) Ollama endpoints use the native `/api/chat` so `keep_alive` applies (§4.3 5b); ruling recorded in the ledger.
- Polish timeout default 8 s; on timeout or error fall back to the raw transcript and mark the row unpolished. (§4.3 step 5)
- Warm-up on hotkey down in parallel with recording; `keep_alive: -1` on Ollama; explicit unload (`keep_alive: 0`) from the same idle path that frees Whisper. LLM counts in the memory strip only when the endpoint is on this machine and the runtime reports a loaded model. (§4.3 5a–c, §9)
- Local candidates probed: LM Studio `http://127.0.0.1:1234`, Ollama `http://127.0.0.1:11434`, llama.cpp `http://127.0.0.1:8080`. LAN endpoints by manual `host:port`. (§8.1)
- Selection: highest-priority endpoint whose last health check ≤ 10 s old succeeded; on failure mark unhealthy, try the next, pill notice "PC unreachable, polished locally"; none healthy → paste raw, row unpolished. Health: `GET /v1/models`, 1 s timeout, every 10 s, backoff up to 60 s for unhealthy ones. Default order: LAN endpoints the user added first, then local. (§8.2)
- Filler removal: universal fillers always; "um", "ah", "eh" only when the language is English; restore the sentence-opening capital; toggle to turn it off. (§18.9)
- No co-author trailers on commits. Tests run with `cargo test -p <crate>`; the app crate's suite needs `--test-threads=1` for engine tests.

## Review Focus

1. A LAN endpoint that answers health checks but returns a 500 or malformed JSON for chat: the pipeline must fall through to the next endpoint and still paste within the timeout budget, not paste nothing. (Task 5 test `falls_back_to_next_endpoint_on_bad_response`)
2. A model that wraps its answer in quotes, a code fence, or a "Here is the cleaned text:" preamble: the pasted text must be the content only. (Task 2 test `clean_output_strips_wrappers`)
3. Empty or whitespace-only LLM output: paste the raw transcript, never an empty string. (Task 5 test `empty_llm_output_counts_as_unpolished`)
4. Non-English dictation in Clean mode with filler removal on: "um" must survive in Portuguese/German text. (Task 1 test `english_only_fillers_kept_in_other_languages`)
5. The user removes the endpoint that is currently preferred while a polish is in flight: the in-flight request completes against the old list, the next dictation uses the new list, nothing panics. (Task 6 test `registry_replacement_mid_flight_is_safe`)

---

### Task 1: Filler removal in `textnorm`

**Files:**
- Modify: `crates/privatevox-core/src/textnorm.rs`
- Modify: `crates/privatevox-core/src/settings.rs` (add `remove_fillers: bool`, default `true`)

**Interfaces:**
- Produces: `pub fn remove_fillers(text: &str, english: bool) -> String`; `pub fn normalize_with(text: &str, english: bool, fillers: bool) -> String` (stutters → fillers if on → whitespace → trim). Existing `normalize(text)` becomes `normalize_with(text, true, false)` so current callers keep their behaviour.
- Filler lists (exact): universal `["uh", "uhm", "uhh", "umm", "hmm", "mhm", "mm", "erm", "err"]`; English-only `["um", "ah", "eh"]`. Matching is case-insensitive on whole words, with an optional trailing comma or period after the filler consumed along with it.

- [ ] **Step 1: Write the failing tests** in the `tests` module of `textnorm.rs`

```rust
#[test] fn removes_universal_fillers() { assert_eq!(remove_fillers("so uh I think, hmm, we go", false), "so I think, we go"); }
#[test] fn english_only_fillers_kept_in_other_languages() { assert_eq!(remove_fillers("um ein Haus", false), "um ein Haus"); assert_eq!(remove_fillers("um a house", true), "a house"); }
#[test] fn restores_sentence_opening_capital() { assert_eq!(remove_fillers("Um, we should go. Uh, now.", true), "We should go. Now."); }
#[test] fn filler_inside_word_is_untouched() { assert_eq!(remove_fillers("the album hummed", true), "the album hummed"); }
#[test] fn normalize_with_runs_stutters_then_fillers() { assert_eq!(normalize_with("I I I um think", true, true), "I think"); }
#[test] fn settings_default_remove_fillers_on() { assert!(Settings::default().remove_fillers); }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core textnorm`
Expected: compile error, `remove_fillers` not found.

- [ ] **Step 3: Implement** `remove_fillers` and `normalize_with` in `textnorm.rs`; add `#[serde(default = "default_true")] pub remove_fillers: bool` to `Settings` with the default.

Approach: tokenise on whitespace keeping punctuation attached; a token is a filler when its alphabetic core (lowercased, trailing `,`/`.` stripped) is in the active list; after removal, if the removed token began a sentence (start of text or previous kept token ends with `.`, `!`, `?`), uppercase the first letter of the next kept token.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass (78 + 6).

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/textnorm.rs crates/privatevox-core/src/settings.rs
git commit -m "feat(core): language-gated filler removal and the remove_fillers setting"
```

---

### Task 2: Modes, prompts and request shapes

**Files:**
- Create: `crates/privatevox-core/src/polish/mod.rs` (declares `modes`, `request`, `endpoints`, `client`, `polisher`)
- Create: `crates/privatevox-core/src/polish/modes.rs`
- Create: `crates/privatevox-core/src/polish/request.rs`
- Modify: `crates/privatevox-core/src/lib.rs` (`pub mod polish;`)
- Modify: `crates/privatevox-core/src/settings.rs` (`mode: Mode` default `Clean`, `mode_prompts: HashMap<String, String>` overrides, `llm_timeout_secs: u64` default 8, `polish_enabled: bool` default true)

**Interfaces:**
- Produces in `modes.rs`:
  - `#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)] #[serde(rename_all = "snake_case")] pub enum Mode { Raw, Clean, Email, Bullets, Code, Translate, Ask }`
  - `impl Mode { pub const ALL: [Mode; 7]; pub fn id(self) -> &'static str; pub fn label(self) -> &'static str; pub fn uses_llm(self) -> bool /* false for Raw, Translate */; pub fn whisper_translate(self) -> bool /* true for Translate */; pub fn parse(id: &str) -> Option<Mode>; }`
  - `pub fn default_prompt(mode: Mode) -> &'static str` (empty for Raw and Translate). Every LLM prompt ends with the shared rules sentence: "Keep the speaker's words and meaning. Never add information. Reply with only the text to paste, no preamble, no quotes, no code fences unless the text itself is code. Keep the language the speaker used unless told otherwise."
  - `pub fn effective_prompt(mode: Mode, overrides: &HashMap<String, String>) -> String` (override by `mode.id()` wins when non-empty).
- Produces in `request.rs`:
  - `pub fn estimate_tokens(text: &str) -> u32` = `ceil(words × 4 / 3) + 8`.
  - `pub fn max_tokens_for(input: &str) -> u32` = `2 × estimate_tokens(input) + 64`.
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)] pub enum Flavor { Ollama, OpenAi }`
  - `pub fn chat_path(flavor: Flavor) -> &'static str` (`/api/chat` or `/v1/chat/completions`).
  - `pub fn chat_body(flavor: Flavor, model: &str, system: &str, user: &str) -> serde_json::Value`. OpenAI: `{model, messages:[system,user], temperature:0.2, max_tokens, stream:false}`. Ollama: `{model, messages, stream:false, keep_alive:-1, options:{temperature:0.2, num_predict: max_tokens}}`.
  - `pub fn parse_chat_reply(flavor: Flavor, body: &serde_json::Value) -> Result<String, String>` (OpenAI `choices[0].message.content`; Ollama `message.content`; missing → `Err("no content in reply")`).
  - `pub fn clean_output(s: &str) -> String`: trim; strip one pair of wrapping straight or curly quotes; strip a wrapping ``` fence (with optional language tag) when the fence encloses the whole reply; drop a first line that ends with a colon and is ≤ 8 words (preamble like "Here is the cleaned text:").
  - `pub fn warm_body(flavor: Flavor, model: &str) -> Option<serde_json::Value>` (Ollama: `{model, keep_alive:-1}` for `/api/generate`; OpenAi: `None` — a 1-token chat with `max_tokens:1` is sent instead by the client).
  - `pub fn unload_body(model: &str) -> serde_json::Value` = `{model, keep_alive:0}` (Ollama only).

- [ ] **Step 1: Write the failing tests** in `modes.rs` and `request.rs`

```rust
// modes.rs
#[test] fn ids_round_trip() { for m in Mode::ALL { assert_eq!(Mode::parse(m.id()), Some(m)); } }
#[test] fn raw_and_translate_skip_the_llm() { assert!(!Mode::Raw.uses_llm()); assert!(!Mode::Translate.uses_llm()); assert!(Mode::Clean.uses_llm()); assert!(Mode::Translate.whisper_translate()); }
#[test] fn every_llm_prompt_carries_the_rules() { for m in Mode::ALL.into_iter().filter(|m| m.uses_llm()) { assert!(default_prompt(m).contains("Reply with only the text to paste"), "{m:?}"); } }
#[test] fn override_wins_when_non_empty() { let mut o = HashMap::new(); o.insert("clean".to_string(), "custom".to_string()); assert_eq!(effective_prompt(Mode::Clean, &o), "custom"); o.insert("clean".into(), "".into()); assert_eq!(effective_prompt(Mode::Clean, &o), default_prompt(Mode::Clean)); }
#[test] fn settings_default_mode_is_clean() { assert_eq!(Settings::default().mode, Mode::Clean); assert_eq!(Settings::default().llm_timeout_secs, 8); }
// request.rs
#[test] fn max_tokens_is_twice_input_plus_64() { assert_eq!(estimate_tokens("one two three"), 12); assert_eq!(max_tokens_for("one two three"), 88); }
#[test] fn openai_body_shape() { let b = chat_body(Flavor::OpenAi, "m", "sys", "hi"); assert_eq!(b["temperature"], 0.2); assert_eq!(b["stream"], false); assert_eq!(b["messages"][0]["role"], "system"); assert_eq!(b["messages"][1]["content"], "hi"); assert_eq!(b["max_tokens"], 82); }
#[test] fn ollama_body_keeps_alive_forever() { let b = chat_body(Flavor::Ollama, "m", "sys", "hi"); assert_eq!(b["keep_alive"], -1); assert_eq!(b["options"]["num_predict"], 82); assert_eq!(chat_path(Flavor::Ollama), "/api/chat"); }
#[test] fn parses_both_reply_shapes() { assert_eq!(parse_chat_reply(Flavor::OpenAi, &json!({"choices":[{"message":{"content":"a"}}]})).unwrap(), "a"); assert_eq!(parse_chat_reply(Flavor::Ollama, &json!({"message":{"content":"b"}})).unwrap(), "b"); assert!(parse_chat_reply(Flavor::OpenAi, &json!({})).is_err()); }
#[test] fn clean_output_strips_wrappers() { assert_eq!(clean_output("\"Hello there.\""), "Hello there."); assert_eq!(clean_output("```text\nHello\n```"), "Hello"); assert_eq!(clean_output("Here is the cleaned text:\nHello"), "Hello"); assert_eq!(clean_output("Note: keep this"), "Note: keep this"); }
#[test] fn unload_body_is_keep_alive_zero() { assert_eq!(unload_body("m")["keep_alive"], 0); assert!(warm_body(Flavor::OpenAi, "m").is_none()); }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core polish`
Expected: compile error, module `polish` not found.

- [ ] **Step 3: Implement** the two files and the settings fields (`#[serde(default)]` on every new field; `Mode` default `Clean`).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/polish crates/privatevox-core/src/lib.rs crates/privatevox-core/src/settings.rs
git commit -m "feat(core): polish modes, default prompts, and chat request shapes for Ollama and OpenAI-compatible servers"
```

---

### Task 3: Endpoint registry, health and selection

**Files:**
- Create: `crates/privatevox-core/src/polish/endpoints.rs`
- Modify: `crates/privatevox-core/src/settings.rs` (`llm_endpoints: Vec<Endpoint>` default empty; `auto_detect_local_llm: bool` default true)

**Interfaces:**
- Produces:
  - `#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)] pub struct Endpoint { pub id: String, pub label: String, pub base_url: String, pub model: Option<String>, pub token: Option<String>, pub flavor: Flavor, #[serde(default)] pub source: Source /* Manual | Local */, #[serde(default = "default_true")] pub enabled: bool }`
  - `pub fn normalize_base_url(input: &str) -> Result<String, String>`: accepts `host:port`, `http://host:port`, trailing slash; rejects empty, non-numeric port, schemes other than http/https; output has a scheme and no trailing slash.
  - `pub fn is_local(base_url: &str) -> bool` (`127.0.0.1`, `localhost`, `::1`, `0.0.0.0`).
  - `pub fn local_candidates() -> Vec<Endpoint>` in this order: Ollama `http://127.0.0.1:11434` (Flavor::Ollama, id `local-ollama`), LM Studio `http://127.0.0.1:1234` (OpenAi, `local-lmstudio`), llama.cpp `http://127.0.0.1:8080` (OpenAi, `local-llamacpp`).
  - `pub fn endpoint_id(base_url: &str) -> String` = `"ep-" + hex(fnv1a64(base_url))`.
  - `pub fn ordered(user: &[Endpoint], auto_local: bool) -> Vec<Endpoint>`: enabled user endpoints in list order, then local candidates not already present by base_url (when `auto_local`).
  - `#[derive(Clone, Debug, Default)] pub struct Health { pub last_ok: Option<Instant>, pub last_checked: Option<Instant>, pub failures: u32, pub next_check: Option<Instant>, pub models: Vec<String>, pub loaded_models: Vec<String>, pub last_error: Option<String> }`
  - `pub struct Registry { endpoints: Vec<Endpoint>, health: HashMap<String, Health> }` with `pub fn new(endpoints: Vec<Endpoint>) -> Self`, `pub fn replace(&mut self, endpoints: Vec<Endpoint>)` (keeps health for ids that remain), `pub fn endpoints(&self) -> &[Endpoint]`, `pub fn health(&self, id: &str) -> Option<&Health>`, `pub fn mark_ok(&mut self, id: &str, now: Instant, models: Vec<String>, loaded: Vec<String>)`, `pub fn mark_failure(&mut self, id: &str, now: Instant, error: String)` (failures += 1, `next_check = now + backoff(failures)`), `pub fn due_for_check(&self, now: Instant) -> Vec<Endpoint>` (never checked, or `next_check <= now`, or `last_checked` older than 10 s), `pub fn select(&self, now: Instant) -> Option<Endpoint>` (first in order with `last_ok` within 10 s and `failures == 0`), `pub fn candidates(&self, now: Instant) -> Vec<Endpoint>` (selected first, then the rest in order, for failover at request time), `pub fn active_local(&self, now: Instant) -> Option<Endpoint>` (selected endpoint if `is_local`).
  - `pub fn backoff(failures: u32) -> Duration` = `min(10 s × 2^(failures−1), 60 s)`, so 10, 20, 40, 60, 60…

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn normalizes_host_port_forms() { assert_eq!(normalize_base_url("192.168.1.20:1234").unwrap(), "http://192.168.1.20:1234"); assert_eq!(normalize_base_url("http://pc.local:11434/").unwrap(), "http://pc.local:11434"); assert!(normalize_base_url("ftp://x:1").is_err()); assert!(normalize_base_url("host:abc").is_err()); assert!(normalize_base_url("").is_err()); }
#[test] fn local_detection() { assert!(is_local("http://127.0.0.1:11434")); assert!(is_local("http://localhost:1234")); assert!(!is_local("http://192.168.1.20:1234")); }
#[test] fn user_endpoints_come_before_local_candidates() { let lan = Endpoint { id: "ep-1".into(), label: "PC".into(), base_url: "http://192.168.1.20:1234".into(), model: None, token: None, flavor: Flavor::OpenAi, source: Source::Manual, enabled: true }; let o = ordered(&[lan.clone()], true); assert_eq!(o[0].id, "ep-1"); assert_eq!(o[1].id, "local-ollama"); assert_eq!(o.len(), 4); assert_eq!(ordered(&[lan], false).len(), 1); }
#[test] fn disabled_endpoints_are_skipped() { let mut e = local_candidates().remove(0); e.enabled = false; assert!(ordered(&[e], false).is_empty()); }
#[test] fn backoff_doubles_and_caps() { assert_eq!(backoff(1).as_secs(), 10); assert_eq!(backoff(2).as_secs(), 20); assert_eq!(backoff(3).as_secs(), 40); assert_eq!(backoff(4).as_secs(), 60); assert_eq!(backoff(9).as_secs(), 60); }
#[test] fn select_prefers_first_healthy_within_ten_seconds() { let mut r = Registry::new(ordered(&[], true)); let t0 = Instant::now(); assert!(r.select(t0).is_none()); r.mark_ok("local-lmstudio", t0, vec!["q".into()], vec![]); assert_eq!(r.select(t0).unwrap().id, "local-lmstudio"); r.mark_ok("local-ollama", t0, vec![], vec![]); assert_eq!(r.select(t0).unwrap().id, "local-ollama"); assert!(r.select(t0 + Duration::from_secs(11)).is_none()); }
#[test] fn failure_marks_unhealthy_and_schedules_backoff() { let mut r = Registry::new(local_candidates()); let t0 = Instant::now(); r.mark_ok("local-ollama", t0, vec![], vec![]); r.mark_failure("local-ollama", t0, "timeout".into()); assert!(r.select(t0).is_none()); assert!(r.due_for_check(t0).iter().all(|e| e.id != "local-ollama")); assert!(r.due_for_check(t0 + Duration::from_secs(10)).iter().any(|e| e.id == "local-ollama")); r.mark_ok("local-ollama", t0 + Duration::from_secs(10), vec![], vec![]); assert_eq!(r.select(t0 + Duration::from_secs(10)).unwrap().id, "local-ollama"); }
#[test] fn replace_keeps_health_of_surviving_ids() { let mut r = Registry::new(local_candidates()); let t0 = Instant::now(); r.mark_ok("local-ollama", t0, vec!["m".into()], vec![]); r.replace(vec![local_candidates().remove(0)]); assert_eq!(r.health("local-ollama").unwrap().models, vec!["m".to_string()]); assert!(r.health("local-lmstudio").is_none()); }
#[test] fn candidates_put_selected_first_then_the_rest() { let mut r = Registry::new(local_candidates()); let t0 = Instant::now(); r.mark_ok("local-lmstudio", t0, vec![], vec![]); let c = r.candidates(t0); assert_eq!(c[0].id, "local-lmstudio"); assert_eq!(c.len(), 3); }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core endpoints`
Expected: compile error, `Registry` not found.

- [ ] **Step 3: Implement** `endpoints.rs` and the two settings fields.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/polish/endpoints.rs crates/privatevox-core/src/settings.rs
git commit -m "feat(core): LLM endpoint registry with health, backoff and priority selection"
```

---

### Task 4: Blocking HTTP client and a fake LLM server for tests

**Files:**
- Create: `crates/privatevox-core/src/polish/client.rs`
- Create: `crates/privatevox-core/src/polish/fake_server.rs` (`#[cfg(any(test, feature = "test-support"))]`; add `test-support = []` feature to the crate so the app crate's tests can use it)
- Modify: `crates/privatevox-core/Cargo.toml` (reqwest features add `"blocking", "json"`)

**Interfaces:**
- Produces:
  - `pub trait LlmClient: Send + Sync { fn probe(&self, ep: &Endpoint, timeout: Duration) -> Result<Probe, String>; fn chat(&self, ep: &Endpoint, model: &str, system: &str, user: &str, timeout: Duration) -> Result<String, String>; fn warm(&self, ep: &Endpoint, model: &str) -> Result<(), String>; fn unload(&self, ep: &Endpoint, model: &str) -> Result<(), String>; }`
  - `#[derive(Clone, Debug, Default, PartialEq)] pub struct Probe { pub models: Vec<String>, pub loaded_models: Vec<String>, pub loaded_bytes: u64 }`
  - `pub struct HttpLlmClient` with `pub fn new() -> Self` (one `reqwest::blocking::Client` with `connect_timeout 1 s`). `probe`: Ollama → `GET /api/tags` (`models[].name`) then `GET /api/ps` (`models[].name`, sum of `size_vram` falling back to `size`); OpenAi → `GET /v1/models` (`data[].id`), then best-effort `GET /api/v0/models` (LM Studio; entries with `state == "loaded"` are loaded, bytes unknown → 0). Bearer token header when `ep.token` is set. `chat`: `POST base_url + chat_path(flavor)` with `chat_body`, parse with `parse_chat_reply`, then `clean_output`. `warm`: Ollama `POST /api/generate` with `warm_body`; OpenAi: chat with system "ok", user "ok", `max_tokens` forced to 1 (`chat_body` then override). `unload`: Ollama `POST /api/generate` with `unload_body`; OpenAi: `Ok(())` no-op.
  - `fake_server.rs`: `pub struct FakeLlm { pub base_url: String, pub hits: Arc<Mutex<Vec<String>>> /* "METHOD path" */, ... }` with `pub fn start(behaviour: Behaviour) -> FakeLlm` on a std `TcpListener` thread, where `pub enum Behaviour { Ollama { reply: String, loaded_bytes: u64 }, OpenAi { reply: String }, BadJson, ServerError, Hang /* accepts, never replies */ }`. Flavors answer the paths above with the documented shapes; `reply` is the chat content. `FakeLlm::endpoint(&self, flavor) -> Endpoint`. `Drop` closes the listener.

- [ ] **Step 1: Write the failing tests** in `client.rs`

```rust
#[test] fn probes_ollama_models_and_loaded_bytes() { let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 2_000_000_000 }); let p = HttpLlmClient::new().probe(&s.endpoint(Flavor::Ollama), Duration::from_secs(1)).unwrap(); assert_eq!(p.models, vec!["qwen2.5:3b".to_string()]); assert_eq!(p.loaded_bytes, 2_000_000_000); }
#[test] fn probes_openai_models() { let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() }); let p = HttpLlmClient::new().probe(&s.endpoint(Flavor::OpenAi), Duration::from_secs(1)).unwrap(); assert_eq!(p.models, vec!["qwen2.5-7b-instruct".to_string()]); }
#[test] fn chat_returns_cleaned_content_for_both_flavors() { for (b, f) in [(Behaviour::Ollama { reply: "\"Hello.\"".into(), loaded_bytes: 0 }, Flavor::Ollama), (Behaviour::OpenAi { reply: "Hello.".into() }, Flavor::OpenAi)] { let s = FakeLlm::start(b); assert_eq!(HttpLlmClient::new().chat(&s.endpoint(f), "m", "sys", "hi", Duration::from_secs(2)).unwrap(), "Hello."); } }
#[test] fn ollama_chat_sends_keep_alive_and_native_path() { let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 0 }); HttpLlmClient::new().chat(&s.endpoint(Flavor::Ollama), "m", "sys", "hi", Duration::from_secs(2)).unwrap(); assert!(s.hits.lock().unwrap().iter().any(|h| h == "POST /api/chat")); assert!(s.last_body().contains("\"keep_alive\":-1")); }
#[test] fn server_error_and_bad_json_are_errors() { for b in [Behaviour::ServerError, Behaviour::BadJson] { let s = FakeLlm::start(b); assert!(HttpLlmClient::new().chat(&s.endpoint(Flavor::OpenAi), "m", "s", "u", Duration::from_secs(2)).is_err()); } }
#[test] fn hang_times_out_within_budget() { let s = FakeLlm::start(Behaviour::Hang); let t = Instant::now(); assert!(HttpLlmClient::new().chat(&s.endpoint(Flavor::OpenAi), "m", "s", "u", Duration::from_millis(300)).is_err()); assert!(t.elapsed() < Duration::from_millis(1500)); }
#[test] fn refused_connection_fails_fast() { let ep = Endpoint { base_url: "http://127.0.0.1:1".into(), ..local_candidates().remove(0) }; let t = Instant::now(); assert!(HttpLlmClient::new().probe(&ep, Duration::from_secs(1)).is_err()); assert!(t.elapsed() < Duration::from_secs(2)); }
#[test] fn warm_and_unload_hit_ollama_generate() { let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 0 }); let c = HttpLlmClient::new(); c.warm(&s.endpoint(Flavor::Ollama), "m").unwrap(); c.unload(&s.endpoint(Flavor::Ollama), "m").unwrap(); let hits = s.hits.lock().unwrap().clone(); assert_eq!(hits.iter().filter(|h| *h == "POST /api/generate").count(), 2); assert!(s.last_body().contains("\"keep_alive\":0")); }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core client`
Expected: compile error, `HttpLlmClient` not found.

- [ ] **Step 3: Implement** `fake_server.rs` first (minimal HTTP/1.1: read headers + `Content-Length` body, route by method and path, answer with `Content-Type: application/json` and `Connection: close`), then `client.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass; the hang test finishes in under 2 s.

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/polish crates/privatevox-core/Cargo.toml
git commit -m "feat(core): blocking LLM client for Ollama and OpenAI-compatible servers, with an in-process fake server for tests"
```

---

### Task 5: The `Polisher` orchestrator with failover and fail-back

**Files:**
- Create: `crates/privatevox-core/src/polish/polisher.rs`

**Interfaces:**
- Consumes: `Registry`, `LlmClient`, `Mode`, `effective_prompt`, `normalize_with`.
- Produces:
  - `#[derive(Clone, Debug, PartialEq, Serialize)] pub enum PolishStatus { NotNeeded /* Raw, Translate, or polish disabled */, Polished { endpoint_id: String, model: String, ms: u64 }, Unpolished { reason: String } }`
  - `#[derive(Clone, Debug, PartialEq)] pub struct PolishOutcome { pub text: String, pub status: PolishStatus, pub notice: Option<String> /* "PC unreachable, polished locally" when the first candidate failed and a later one succeeded */ }`
  - `pub struct Polisher { registry: Registry, client: Arc<dyn LlmClient>, pub timeout: Duration, pub prompts: HashMap<String, String>, pub enabled: bool }` with `pub fn new(client: Arc<dyn LlmClient>, endpoints: Vec<Endpoint>, timeout: Duration) -> Self`, `pub fn set_endpoints(&mut self, endpoints: Vec<Endpoint>)` (calls `Registry::replace`), `pub fn registry(&self) -> &Registry`, `pub fn check_due(&mut self, now: Instant)` (probes each `due_for_check` endpoint with a 1 s timeout and marks ok/failure; `loaded_bytes` kept on `Health` as a new `pub loaded_bytes: u64` field — add it to Task 3's struct), `pub fn polish(&mut self, mode: Mode, raw: &str, now: Instant) -> PolishOutcome`, `pub fn warm(&self, now: Instant)` (selected endpoint's model; no-op if none), `pub fn unload_all(&self)` (every endpoint whose health shows a loaded model), `pub fn active_model(&self, ep: &Endpoint) -> Option<String>` (`ep.model` or the first probed model), `pub fn resident_local_bytes(&self, now: Instant) -> u64` (selected local endpoint's `loaded_bytes`, else 0).
  - `polish` algorithm: if `!enabled || !mode.uses_llm()` → `NotNeeded` with `text = raw`. Otherwise for each `candidates(now)` in order with a known model: `chat` with `timeout`; on `Ok(t)` with non-empty `t.trim()` → `mark_ok` and return `Polished` (notice set when this was not the first candidate); on `Err`/empty → `mark_failure`, continue. Exhausted → `Unpolished { reason }` with raw text, where reason is the last error or "no LLM endpoint reachable".

- [ ] **Step 1: Write the failing tests** (each test spins up `FakeLlm`s and injects them as manual endpoints)

```rust
#[test] fn raw_mode_never_calls_the_llm() { let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![s.endpoint(Flavor::OpenAi)], Duration::from_secs(2)); let o = p.polish(Mode::Raw, "hi", Instant::now()); assert_eq!(o.status, PolishStatus::NotNeeded); assert!(s.hits.lock().unwrap().is_empty()); }
#[test] fn polishes_with_the_first_healthy_endpoint() { let s = FakeLlm::start(Behaviour::OpenAi { reply: "Hello there.".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![s.endpoint(Flavor::OpenAi)], Duration::from_secs(2)); let now = Instant::now(); p.check_due(now); let o = p.polish(Mode::Clean, "hello there", now); assert_eq!(o.text, "Hello there."); assert!(matches!(o.status, PolishStatus::Polished { .. })); assert!(o.notice.is_none()); }
#[test] fn falls_back_to_next_endpoint_on_bad_response() { let bad = FakeLlm::start(Behaviour::ServerError); let good = FakeLlm::start(Behaviour::OpenAi { reply: "ok text".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![bad.endpoint(Flavor::OpenAi), good.endpoint(Flavor::OpenAi)], Duration::from_secs(2)); let now = Instant::now(); p.check_due(now); let o = p.polish(Mode::Clean, "x", now); assert_eq!(o.text, "ok text"); assert_eq!(o.notice.as_deref(), Some("PC unreachable, polished locally")); assert!(p.registry().health(&bad.endpoint(Flavor::OpenAi).id).unwrap().failures >= 1); }
#[test] fn unreachable_everything_pastes_raw_and_marks_unpolished() { let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![Endpoint { base_url: "http://127.0.0.1:1".into(), model: Some("m".into()), ..local_candidates().remove(1) }], Duration::from_secs(1)); let o = p.polish(Mode::Clean, "raw words", Instant::now()); assert_eq!(o.text, "raw words"); assert!(matches!(o.status, PolishStatus::Unpolished { .. })); }
#[test] fn empty_llm_output_counts_as_unpolished() { let s = FakeLlm::start(Behaviour::OpenAi { reply: "  ".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![s.endpoint(Flavor::OpenAi)], Duration::from_secs(2)); let now = Instant::now(); p.check_due(now); let o = p.polish(Mode::Clean, "raw", now); assert_eq!(o.text, "raw"); assert!(matches!(o.status, PolishStatus::Unpolished { .. })); }
#[test] fn fails_back_once_the_preferred_endpoint_recovers() { let pref = FakeLlm::start(Behaviour::Hang); let local = FakeLlm::start(Behaviour::OpenAi { reply: "local".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![pref.endpoint(Flavor::OpenAi), local.endpoint(Flavor::OpenAi)], Duration::from_millis(300)); let t0 = Instant::now(); p.check_due(t0); assert_eq!(p.polish(Mode::Clean, "a", t0).text, "local"); drop(pref); let pref2 = FakeLlm::start_on(/* same port */ Behaviour::OpenAi { reply: "pc".into() }, pref_port); p.check_due(t0 + Duration::from_secs(10)); assert_eq!(p.polish(Mode::Clean, "a", t0 + Duration::from_secs(10)).text, "pc"); }
#[test] fn disabled_polish_is_not_needed() { let s = FakeLlm::start(Behaviour::OpenAi { reply: "x".into() }); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![s.endpoint(Flavor::OpenAi)], Duration::from_secs(1)); p.enabled = false; assert_eq!(p.polish(Mode::Clean, "r", Instant::now()).status, PolishStatus::NotNeeded); }
#[test] fn resident_bytes_only_for_a_local_selected_endpoint() { let s = FakeLlm::start(Behaviour::Ollama { reply: "x".into(), loaded_bytes: 3_000_000_000 }); let mut ep = s.endpoint(Flavor::Ollama); let mut p = Polisher::new(Arc::new(HttpLlmClient::new()), vec![ep.clone()], Duration::from_secs(1)); let now = Instant::now(); p.check_due(now); assert_eq!(p.resident_local_bytes(now), 3_000_000_000); ep.base_url = ep.base_url.replace("127.0.0.1", "localhost.invalid"); /* a LAN-looking url */ }
```

For `fails_back_once_the_preferred_endpoint_recovers`, add `FakeLlm::start_on(behaviour, port: u16) -> FakeLlm` and `FakeLlm::port(&self) -> u16` to Task 4's fake server. The last test's second half is dropped if it reads as noise; the assertion that matters is the first.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core polisher`
Expected: compile error, `Polisher` not found.

- [ ] **Step 3: Implement** `polisher.rs`; add `loaded_bytes: u64` to `Health` and `start_on`/`port` to the fake server.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/polish
git commit -m "feat(core): Polisher orchestrates mode prompts over the endpoint registry with failover, fail-back and raw fallback"
```

---

### Task 6: Store schema v2 and re-polish support

**Files:**
- Modify: `crates/privatevox-core/src/store.rs`

**Interfaces:**
- Produces: `Dictation` and `NewDictation` gain `pub polished_by: Option<String>` and `pub polish_status: String` (`"none" | "ok" | "unpolished"`); `pub fn update_polish(&mut self, id: i64, mode: &str, polished_text: Option<&str>, polished_by: Option<&str>, polish_status: &str) -> Result<()>`; `pub fn get(&self, id: i64) -> Result<Option<Dictation>>`. Migration: when `schema_version < 2`, `ALTER TABLE dictations ADD COLUMN polish_status TEXT NOT NULL DEFAULT 'none'` and set version 2 (`polished_by` already exists). `COLUMNS` and `map_row` extended.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn migrates_v1_database_to_v2() { /* open, insert a row with the v1 insert, close; open again; assert schema_version == 2 and list()[0].polish_status == "none" */ }
#[test] fn update_polish_changes_mode_text_and_status() { let mut s = Store::open(&tmp()).unwrap(); let id = s.insert(&row("raw")).unwrap(); s.update_polish(id, "email", Some("Dear all,"), Some("ep-1"), "ok").unwrap(); let d = s.get(id).unwrap().unwrap(); assert_eq!(d.mode, "email"); assert_eq!(d.polished_text.as_deref(), Some("Dear all,")); assert_eq!(d.polished_by.as_deref(), Some("ep-1")); assert_eq!(d.polish_status, "ok"); }
#[test] fn registry_replacement_mid_flight_is_safe() { /* in polisher.rs: clone candidates, call set_endpoints(vec![]) while holding the clone, polish still returns; here only a store test is needed, so this test lives in polisher.rs and is written in this task */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox-core store`
Expected: compile error on `polish_status` / `update_polish`.

- [ ] **Step 3: Implement** the migration, fields, `get`, `update_polish`; update `export.rs` (Markdown metadata line shows mode and "unpolished" when relevant) and any constructor of `NewDictation` in tests.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/privatevox-core/src/store.rs crates/privatevox-core/src/export.rs crates/privatevox-core/src/polish/polisher.rs
git commit -m "feat(core): schema v2 with polish_status, update_polish and get for re-polish"
```

---

### Task 7: Pipeline integration: polish after decode, warm on key down, unload with Whisper, mode-aware decode

**Files:**
- Modify: `src-tauri/src/pipeline.rs`
- Modify: `src-tauri/src/lib.rs` (wire `Deps`), `src-tauri/src/state.rs` (`pub polisher: Arc<Mutex<Polisher>>`)
- Create: `src-tauri/src/llm.rs` (builds the `Polisher` from settings, `fn endpoints_from_settings(&Settings) -> Vec<Endpoint>` = `ordered(&s.llm_endpoints, s.auto_detect_local_llm)`, `fn spawn_health_loop(polisher, app_handle)` every 10 s calling `check_due` and emitting `llm-changed` when any health changed)

**Interfaces:**
- Consumes: `Polisher`, `PolishOutcome`, `Mode`, `normalize_with`.
- Produces: `Deps` gains `pub polish: Box<dyn Fn(Mode, &str) -> PolishOutcome + Send>`, `pub llm_warm: Box<dyn Fn() + Send>`, `pub llm_unload: Box<dyn Fn() + Send>`. `PillState` gains `Polishing` and `Listening` gains `mode: String` (the mode label). `Cmd` gains `SetMode(Mode)` (persists via `SettingsChanged` path) and `Repolish { id: i64, mode: Mode }`.
- Behaviour pinned:
  - On `Cmd::Start`: after the model preload is kicked, call `llm_warm` on a detached thread named `privatevox-llm-warm` (only when the current mode uses the LLM and polish is enabled).
  - After decode text is assembled (both `decode_and_paste` and `finish_chunked`): apply `normalize_with(text, english, settings.remove_fillers)` where `english` = detected language or first spoken language equals `en`; then if `mode.uses_llm()` emit `PillState::Polishing` and call `polish`. Paste the outcome text. Row: `mode = mode.id()`, `raw_text` = normalised raw, `polished_text` = Some(outcome.text) only when `Polished`, `polished_by`, `polish_status` = `ok` / `unpolished` / `none`. Pasted preview prefixed with the notice when present ("PC unreachable, polished locally · Pasted: …").
  - `DecodeOptions.translate = mode.whisper_translate()`.
  - On `Cmd::ModelUnloaded` (idle) and `Cmd::PauseAndUnload`: call `llm_unload` on a detached thread.
  - `Cmd::Repolish { id, mode }`: load the row, run `polish(mode, raw_text)`, `update_polish`, emit `history-changed` event via a new `Deps.on_history_changed: Box<dyn Fn() + Send>`; on `Unpolished` keep the old text and emit `PillState::Error { message: "Re-polish failed: <reason>" }`.

- [ ] **Step 1: Write the failing tests** in `pipeline.rs` tests (the existing harness with fake engine/capture/paste; add a recording `polish` closure)

```rust
#[test] fn clean_mode_pastes_polished_text_and_stores_both() { /* polish closure returns Polished("Hello there.") for raw "hello there"; after Start/Stop with fake transcript "hello there": pasted == "Hello there.", row.raw_text == "hello there", row.polished_text == Some("Hello there."), row.polish_status == "ok", row.mode == "clean" */ }
#[test] fn raw_mode_skips_polish_and_warm() { /* settings.mode = Raw; warm counter stays 0; polish counter 0; row.polish_status == "none" */ }
#[test] fn unpolished_outcome_pastes_raw_and_marks_row() { /* closure returns Unpolished; pasted == raw; row.polish_status == "unpolished"; polished_text None */ }
#[test] fn warm_fires_on_start_in_llm_modes() { /* Start → warm counter == 1 within 500 ms */ }
#[test] fn idle_unload_also_unloads_llm() { /* send ModelUnloaded → llm_unload counter == 1 */ }
#[test] fn notice_prefixes_the_pasted_preview() { /* outcome.notice = Some("PC unreachable, polished locally"); last Pasted preview starts with that notice */ }
#[test] fn repolish_updates_row_and_notifies() { /* insert row, send Repolish{id, Email}; row.mode == "email", polished_text updated, on_history_changed counter == 1 */ }
#[test] fn translate_mode_sets_whisper_translate_flag() { /* fake engine records DecodeOptions; mode Translate → translate == true; polish counter 0 */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox pipeline -- --test-threads=1`
Expected: compile error, `Deps` missing `polish`.

- [ ] **Step 3: Implement** the pipeline changes, `llm.rs`, state and wiring in `lib.rs` (settings change → `polisher.set_endpoints(endpoints_from_settings)`, `timeout`, `prompts`, `enabled = polish_enabled`).

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox -- --test-threads=1`
Expected: all pass (33 + 8, 4 ignored).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src
git commit -m "feat(app): polish after decode with warm-up on key down, LLM unload with Whisper, re-polish, translate flag"
```

---

### Task 8: Commands for modes, prompts, endpoints and re-polish; memory report LLM segment

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs` (register)

**Interfaces:**
- Produces commands:
  - `list_modes() -> Vec<ModeInfo { id, label, uses_llm, prompt, default_prompt, is_default }>`; `set_mode(id: String) -> Result<()>` (persist + `Cmd::SetMode`); `set_mode_prompt(id, prompt: Option<String>) -> Result<()>` (`None` resets).
  - `list_llm_endpoints() -> Vec<EndpointView { id, label, base_url, model, flavor, source, enabled, healthy: bool, last_error, models: Vec<String>, loaded_models, is_local, selected: bool }>` in registry order; `add_llm_endpoint(input: String, label: Option<String>) -> Result<EndpointView>` (normalise, detect flavor by probing `/api/tags` then `/v1/models` with 2 s timeout, error text when neither answers: "Nothing answered at {url}. Is the server running and set to serve on the network?"); `remove_llm_endpoint(id)`; `set_llm_endpoint(id, model: Option<String>, enabled: Option<bool>, label: Option<String>)`; `move_llm_endpoint(id, up: bool)`; `set_polish_enabled(enabled: bool)`; `set_llm_timeout(secs: u64)` (1–60); `repolish_history(id: i64, mode: String)` (sends `Cmd::Repolish`).
  - `memory_report` passes `llm_bytes = polisher.resident_local_bytes(now)`.
  - `llm_status() -> { selected: Option<EndpointView>, enabled: bool }` for the strip's selector and the tray.

- [ ] **Step 1: Write the failing tests** (pure helpers only; command plumbing is exercised by the UI)

```rust
#[test] fn add_endpoint_error_copy_names_the_url() { assert_eq!(unreachable_message("http://10.0.0.5:1234"), "Nothing answered at http://10.0.0.5:1234. Is the server running and set to serve on the network?"); }
#[test] fn move_endpoint_reorders_user_list() { let mut v = vec![ep("a"), ep("b"), ep("c")]; move_in(&mut v, "c", true); assert_eq!(ids(&v), ["a","c","b"]); move_in(&mut v, "a", true); assert_eq!(ids(&v), ["a","c","b"]); }
#[test] fn memory_input_counts_llm_bytes_from_polisher() { /* build_memory_input with llm_bytes 2_000_000_000 → report.llm_bytes == 2_000_000_000 and projected includes it */ }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p privatevox commands -- --test-threads=1`
Expected: compile error, `unreachable_message` not found.

- [ ] **Step 3: Implement** the commands and helpers; register them in `lib.rs`.

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p privatevox -- --test-threads=1`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(app): commands for modes, prompts, LLM endpoints, re-polish and the LLM memory segment"
```

---

### Task 9: Panel and pill UI for polish

**Files:**
- Modify: `ui/panel.html`, `ui/pill.html`, `ui/privatevox.css`
- Modify: `src-tauri/src/tray.rs` (Mode submenu with a radio per mode; "Polish: <endpoint label or Off>" disabled info item)

**Behaviour pinned (no unit tests; verified by a smoke run and a screenshot request to Roy):**
- Header: a Mode `<select>` left of the model dropdown, populated from `list_modes`, persisting through `set_mode`.
- Memory strip: the Polish LLM selector lists "Off" plus every endpoint from `list_llm_endpoints` as "label · model (local|LAN)"; choosing one calls `move_llm_endpoint(id, to_top)` (add `to_top: bool` variant: `set_llm_endpoint_preferred(id)`) and `set_polish_enabled(true)`; "Off" calls `set_polish_enabled(false)`. Legend's Polish LLM value shows the measured bytes or "on <LAN label>" when the selected endpoint is remote.
- Rows: meta shows the mode label and "unpolished" when `polish_status == "unpolished"`; the Raw button is enabled whenever `polished_text` differs from `raw_text`; a "Re-polish" `<select>` with the LLM modes calls `repolish_history`; rows reload on `history-changed`.
- Settings tab gains a "Polish" card: polish on/off, timeout seconds, "Remove filler words" toggle, per-mode prompt editors (textarea, Save, Reset to default), and an Endpoints card: list with health dot (green healthy / amber backing off / grey unknown), label, URL, model `<select>` from the probed list, enable toggle, up/down, remove; an "Add endpoint" row (`host:port` input + label + Add) showing the command's error text inline.
- Setup tab: a line under permissions: "Polish LLM: <selected label> ready" or "No LLM found. Install Ollama and run `ollama pull qwen2.5:3b`, or add a LAN endpoint in Settings → Polish."
- Pill: `listening` label appends ` · <mode label>` when the mode is not Clean; `polishing` state shows the spinner and "Polishing…".
- Status bar shows `llm-changed` transitions once: "Polish LLM: PC reachable again" / "Polish LLM: PC unreachable, using local".

- [ ] **Step 1: Implement** the UI and tray changes.
- [ ] **Step 2: Smoke run** `cargo build -p privatevox && ./target/debug/privatevox` for 10 s with `PRIVATEVOX_DEBUG_SCRIPT="start:1500,stop:5000"` and Ollama running; Expected log lines: `pill: {"state":"polishing"}` then `pill: {"state":"pasted"...}`; a history row with `polish_status = ok` (`sqlite3 ... "select mode, polish_status from dictations order by id desc limit 1"`).
- [ ] **Step 3: Commit**

```bash
git add ui src-tauri/src/tray.rs
git commit -m "feat(ui): mode picker, Polish LLM selector in the memory strip, re-polish on rows, Polish settings and endpoints"
```

---

### Task 10: Live check against Ollama and docs

**Files:**
- Modify: `README.md` (Polish section: modes, local Ollama, adding a LAN endpoint, what LM Studio needs), `CHANGELOG.md` (Unreleased), `docs/privatevox-roadmap.html` (phase 3 rows to done)

- [ ] **Step 1: Live check** with `ollama serve` running and `qwen2.5:3b` pulled: `cargo run -p privatevox` → Setup tab shows "Polish LLM: Ollama ready"; dictate "um so hello there this is uh a test" → pasted text has no fillers and a capital; history row mode `clean`, status `ok`; re-polish as Bullets produces a bulleted result; stop Ollama → next dictation pastes raw within 9 s with the row marked unpolished; start Ollama → within 20 s the status bar reports it reachable again.
- [ ] **Step 2: Record the measured warm and cold polish times** in `docs/superpowers/plans/2026-10-28-privatevox-polish-lan-ledger.md`.
- [ ] **Step 3: Update docs and commit**

```bash
git add README.md CHANGELOG.md docs
git commit -m "docs: polish modes and LAN endpoints; roadmap phase 3 done"
```
