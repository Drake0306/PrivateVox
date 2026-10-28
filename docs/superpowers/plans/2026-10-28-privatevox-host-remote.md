# PrivateVox Phase 4a: Host Mode and the Remote Interface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any PrivateVox can act as a host; a client PrivateVox sends dictation chunks and polish requests to it, falls back to local when it is gone, shows the host's specs in the memory strip, and keeps a marked copy of every row while the host keeps its own.

**Architecture:** A `net` module in core defines the wire types (JSON, PCM as base64 f32 LE) and a blocking `HostClient`. The engine crate gains `RemoteEngine` (the `Engine` trait over HTTP) and `RoutingEngine` (local or remote by a shared `RemoteState`, with fallback and a notice). The app crate gains `host.rs`, a `tiny_http` server that runs VAD trim, the shared engine and the polisher, stores rows with `engine_kind = "host"`, and assembles chunked sessions. Settings get `host` and `remote` blocks; the panel gets a Network card, a history source toggle and a "via PC" marker; the strip shows the host's figures while the host is in use.

**Tech Stack:** Rust 2021; `tiny_http` 0.12 (blocking server, threads), `base64` 0.22, `local-ip-address` 0.6 (host's LAN address); reqwest blocking (existing); Tauri 2 commands/events; vanilla JS.

**Spec:** `docs/superpowers/specs/2026-10-28-whisper-harness-design.md` §8.6 (binding), with §4.1, §4.4, §6, §8.2 (health rules), §9.

## Global Constraints

- Host port default 8765, bound to LAN/all interfaces (`0.0.0.0`), never a public listener; optional Bearer token checked on every route when set. (§8.6)
- Host routes: `GET /v1/status`, `POST /v1/dictations`, `POST /v1/polish`, `GET /v1/history`, `GET /v1/models`, `POST /v1/models/select`. (§8.6; `/v1/polish` added by ruling so the client's chunk pipeline stays unchanged.)
- PCM on the wire: 16 kHz mono f32 little-endian, base64. (§8.6)
- Client health: `GET /v1/status` every 10 s, 1 s timeout, backoff to 60 s, TTL 15 s (same rules as LLM endpoints, §8.2).
- Local Whisper is not loaded while the host is in use; fallback to local (engine and polish) within one dictation when the host fails, with the pill notice "PC unreachable, transcribed locally"; back to the host when reachable. (§8.6)
- Rows: client `engine_kind = "remote:<label>"`, host `engine_kind = "host"` with `app_name = client name`; one host row per session. (§8.6)
- Memory strip shows the host's figures labelled "On <label>" while the host is in use. (§8.6, §9)
- No co-author trailers on commits. App tests run with `--test-threads=1`.

## Review Focus

1. The host receives a chunk whose base64 is malformed or whose length is not a multiple of 4 bytes: it must answer 400 with a message, never panic a server thread. (Task 1 `decode_rejects_bad_payloads`, Task 4 `bad_body_is_400`)
2. The host engine is busy with its own dictation when a client request arrives: the client call must wait or fail cleanly, and the host's own pill must not be disturbed. (Task 4 `concurrent_requests_serialise_on_the_engine`)
3. A session whose final chunk never arrives (client crashed): the host must not keep it forever. (Task 4 `stale_sessions_are_dropped_after_ten_minutes`)
4. The host goes away between chunk 2 and chunk 3 of a long dictation: the client decodes chunk 3 locally and the row still reads in order. (Task 3 `falls_back_mid_dictation_and_keeps_order`)
5. The token is set on the host but not on the client: every route answers 401 with a clear message, and the client's status shows "token rejected". (Task 4 `token_required_when_set`, Task 7 `status_explains_a_rejected_token`)

---

### Task 1: Wire types and PCM encoding

**Files:**
- Create: `crates/privatevox-core/src/net/mod.rs`, `crates/privatevox-core/src/net/protocol.rs`
- Modify: `crates/privatevox-core/src/lib.rs` (`pub mod net;`), `crates/privatevox-core/Cargo.toml` (`base64 = "0.22"`)

**Interfaces:**
- Produces (all `Serialize + Deserialize + Clone + Debug + PartialEq`):
  - `StatusReply { name: String, version: String, platform: String, gpu: GpuInfo { name: String, total_bytes: u64, used_bytes: u64 }, whisper: WhisperInfo { model: String, loaded: bool, resident_bytes: u64 }, polish: PolishInfo { enabled: bool, endpoint_label: Option<String>, model: Option<String>, resident_bytes: u64 }, clients: u32 }`
  - `DictationRequest { client: String, session: String, index: u32, final_: bool /* serde rename "final" */, pcm: String, language: Option<String>, translate: bool, mode: String }`
  - `DictationReply { text: String, language: Option<String>, model: String, decode_ms: u64 }`
  - `PolishRequest { client: String, session: String, mode: String, text: String }`
  - `PolishReply { text: String, polish_status: String /* ok|unpolished|none */, polished_by: Option<String>, ms: u64 }`
  - `RemoteRow { id: i64, created_at_ms: i64, app_name: Option<String>, mode: String, model: String, duration_ms: u32, raw_text: String, polished_text: Option<String>, polish_status: String, engine_kind: String }` and `HistoryReply { rows: Vec<RemoteRow>, total: u32 }`
  - `ModelsReply { models: Vec<RemoteModel { id: String, label: String, downloaded: bool, active: bool }> }`, `SelectModelRequest { id: String }`
  - `ErrorReply { error: String }`
  - `pub fn encode_pcm(pcm: &[f32]) -> String`, `pub fn decode_pcm(b64: &str) -> Result<Vec<f32>, String>` (error on bad base64 or a byte length not divisible by 4; cap at 5 min = 4_800_000 samples).
  - `pub const DEFAULT_HOST_PORT: u16 = 8765; pub const MAX_PCM_SAMPLES: usize = 16_000 * 300;`

- [ ] **Step 1: Write the failing tests** in `protocol.rs`

```rust
#[test] fn pcm_round_trips() { let v = vec![0.0f32, 0.5, -1.0, 1.0e-3]; assert_eq!(decode_pcm(&encode_pcm(&v)).unwrap(), v); assert_eq!(decode_pcm(&encode_pcm(&[])).unwrap(), Vec::<f32>::new()); }
#[test] fn decode_rejects_bad_payloads() { assert!(decode_pcm("not base64!!").is_err()); assert!(decode_pcm(&base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3])).unwrap_err().contains("multiple of 4")); let too_long = vec![0u8; (MAX_PCM_SAMPLES + 1) * 4]; assert!(decode_pcm(&base64::engine::general_purpose::STANDARD.encode(too_long)).unwrap_err().contains("5 minutes")); }
#[test] fn dictation_request_uses_final_on_the_wire() { let r = DictationRequest { client: "Mac".into(), session: "s1".into(), index: 0, final_: true, pcm: "".into(), language: Some("en".into()), translate: false, mode: "clean".into() }; let j = serde_json::to_string(&r).unwrap(); assert!(j.contains("\"final\":true")); assert_eq!(serde_json::from_str::<DictationRequest>(&j).unwrap(), r); }
#[test] fn error_reply_shape() { assert_eq!(serde_json::to_string(&ErrorReply { error: "x".into() }).unwrap(), r#"{"error":"x"}"#); }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p privatevox-core net` → compile error, module missing.
- [ ] **Step 3: Implement** `protocol.rs`, `mod.rs` (`pub mod protocol; pub mod client;` the latter in Task 2 — declare in Task 2), lib.rs, Cargo.toml.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox-core` → all pass.
- [ ] **Step 5: Commit** — `git commit -m "feat(core): host/client wire types and PCM encoding"`

---

### Task 2: `HostClient` (blocking) and a fake host for tests

**Files:**
- Create: `crates/privatevox-core/src/net/client.rs`, `crates/privatevox-core/src/net/fake_host.rs` (`#[cfg(any(test, feature = "test-support"))]`)

**Interfaces:**
- Produces:
  - `pub struct HostClient { base_url: String, token: Option<String>, http: reqwest::blocking::Client }` with `pub fn new(base_url: &str, token: Option<String>) -> Self` (connect timeout 1 s), `pub fn status(&self, timeout: Duration) -> Result<StatusReply, ClientError>`, `pub fn dictation(&self, req: &DictationRequest, timeout: Duration) -> Result<DictationReply, ClientError>`, `pub fn polish(&self, req: &PolishRequest, timeout: Duration) -> Result<PolishReply, ClientError>`, `pub fn history(&self, limit: u32, offset: u32, search: Option<&str>, timeout: Duration) -> Result<HistoryReply, ClientError>`, `pub fn models(&self, timeout: Duration) -> Result<ModelsReply, ClientError>`, `pub fn select_model(&self, id: &str, timeout: Duration) -> Result<(), ClientError>`.
  - `#[derive(Debug, Clone, PartialEq)] pub enum ClientError { Unreachable(String), Timeout, Unauthorized, Server(String) /* 4xx/5xx with the ErrorReply text */, BadReply(String) }` with `impl Display`.
  - Same `assert_not_on_async_runtime` rule as the LLM client (reuse by moving it to `crates/privatevox-core/src/net/mod.rs` as `pub(crate) fn assert_blocking_context()` and calling it from both clients).
  - `fake_host.rs`: `pub struct FakeHost { pub base_url: String, pub hits: Arc<Mutex<Vec<String>>>, pub requests: Arc<Mutex<Vec<DictationRequest>>>, ... }` with `pub fn start(behaviour: HostBehaviour) -> FakeHost`, `pub fn start_on(behaviour, port: u16)`, `pub fn port(&self) -> u16`, `pub enum HostBehaviour { Ok { text: String, polished: String, token: Option<String> }, Hang, ServerError }`. `Ok` answers every route with plausible bodies (status with gpu "Fake RX 9070", 16 GB total, 5 GB used; dictation `text`; polish `polished` with status ok; history with two rows; models with turbo active) and checks the Bearer token when set (401 `{"error":"token required"}` otherwise).

- [ ] **Step 1: Write the failing tests** in `client.rs`

```rust
#[test] fn status_round_trip() { let h = FakeHost::start(HostBehaviour::Ok { text: "hi".into(), polished: "Hi.".into(), token: None }); let s = HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap(); assert_eq!(s.gpu.name, "Fake RX 9070"); assert_eq!(s.gpu.total_bytes, 16_000_000_000); }
#[test] fn dictation_sends_pcm_and_returns_text() { let h = FakeHost::start(HostBehaviour::Ok { text: "hello there".into(), polished: "".into(), token: None }); let req = DictationRequest { client: "Mac".into(), session: "s".into(), index: 0, final_: true, pcm: encode_pcm(&[0.1; 1600]), language: Some("en".into()), translate: false, mode: "clean".into() }; let r = HostClient::new(&h.base_url, None).dictation(&req, Duration::from_secs(2)).unwrap(); assert_eq!(r.text, "hello there"); assert_eq!(h.requests.lock().unwrap()[0].index, 0); }
#[test] fn token_mismatch_is_unauthorized() { let h = FakeHost::start(HostBehaviour::Ok { text: "".into(), polished: "".into(), token: Some("s3cret".into()) }); assert_eq!(HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unauthorized); assert!(HostClient::new(&h.base_url, Some("s3cret".into())).status(Duration::from_secs(1)).is_ok()); }
#[test] fn hang_times_out_and_refused_is_unreachable() { let h = FakeHost::start(HostBehaviour::Hang); assert_eq!(HostClient::new(&h.base_url, None).status(Duration::from_millis(300)).unwrap_err(), ClientError::Timeout); assert!(matches!(HostClient::new("http://127.0.0.1:1", None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Unreachable(_))); }
#[test] fn server_error_carries_the_message() { let h = FakeHost::start(HostBehaviour::ServerError); assert!(matches!(HostClient::new(&h.base_url, None).status(Duration::from_secs(1)).unwrap_err(), ClientError::Server(m) if m.contains("boom"))); }
#[test] fn history_and_models_parse() { let h = FakeHost::start(HostBehaviour::Ok { text: "".into(), polished: "".into(), token: None }); let c = HostClient::new(&h.base_url, None); assert_eq!(c.history(10, 0, None, Duration::from_secs(1)).unwrap().rows.len(), 2); assert!(c.models(Duration::from_secs(1)).unwrap().models.iter().any(|m| m.active)); c.select_model("large-v3", Duration::from_secs(1)).unwrap(); assert!(h.hits.lock().unwrap().iter().any(|x| x == "POST /v1/models/select")); }
```

- [ ] **Step 2: Run to verify they fail** — compile error, `HostClient` missing.
- [ ] **Step 3: Implement** the fake host (same minimal HTTP/1.1 reader as `polish/fake_server.rs`; extract the shared reader into `crates/privatevox-core/src/net/minihttp.rs` used by both fakes) and the client.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox-core` → all pass.
- [ ] **Step 5: Commit** — `git commit -m "feat(core): blocking HostClient and a fake host for tests"`

---

### Task 3: `RemoteEngine` and `RoutingEngine` in the engine crate

**Files:**
- Create: `crates/privatevox-engine/src/remote.rs`, `crates/privatevox-engine/src/routing.rs`
- Modify: `crates/privatevox-engine/src/lib.rs`, `crates/privatevox-engine/Cargo.toml` (`privatevox-core` with `features = ["test-support"]` under dev-dependencies)

**Interfaces:**
- Consumes: `HostClient`, `DictationRequest/Reply`, `encode_pcm`; `Engine`, `Transcript`, `LoadInfo`, `DecodeOptions`.
- Produces:
  - `pub struct RemoteState { pub enabled: AtomicBool, pub healthy: AtomicBool, pub label: Mutex<String>, pub client: Mutex<Option<Arc<HostClient>>>, pub client_name: Mutex<String>, pub session: Mutex<String>, pub next_index: AtomicU32, pub last_status: Mutex<Option<StatusReply>>, pub last_error: Mutex<Option<String>>, pub notice: Mutex<Option<String>> }` with `pub fn new() -> Arc<Self>`, `pub fn in_use(&self) -> bool` (enabled && healthy && client set), `pub fn configure(&self, label, base_url, token, client_name)`, `pub fn clear(&self)`, `pub fn new_session(&self) -> String` (uuid-ish from time + counter; resets `next_index`), `pub fn take_notice(&self) -> Option<String>`.
  - `pub struct RemoteEngine { state: Arc<RemoteState>, timeout: Duration }` implementing `Engine`: `load` → `Ok(LoadInfo { duration: 0, resident_bytes: 0 })` when `in_use` else `Err("host not in use")`; `is_loaded` → `in_use()`; `transcribe` → builds `DictationRequest` with `session`, `index = next_index.fetch_add(1)`, `final_: false` (the pipeline marks the end through polish/final in Task 6), `mode` from `opts.initial_prompt`? No: mode is carried separately in Task 6; here `mode: "raw"`. Returns `Transcript { text, language, decode_ms, audio_ctx: 0, reran: false }`; on error sets `last_error` and `healthy = false` and returns `Err`. `unload`/`cancel` → no-ops.
  - `pub struct RoutingEngine { local: Box<dyn Engine>, remote: RemoteEngine, state: Arc<RemoteState> }` implementing `Engine`: when `state.in_use()` route to remote; on remote `Err` → set `notice = "PC unreachable, transcribed locally"`, mark unhealthy, and run the same call on local (loading local lazily with the last known model path: `RoutingEngine::new(local, remote, state, model_path: Arc<Mutex<Option<PathBuf>>>, flash: bool)`); `load` when in_use → remote (no local load); `is_loaded` → remote in_use || local.is_loaded(); `unload` → local.unload(); `resident_bytes` → local's (0 while remote); `cancel` → both.
  - Timeout: `15 s + 3 × clip seconds` (same policy as the worker, `privatevox_engine::policy::deadline_for`).

- [ ] **Step 1: Write the failing tests** in `routing.rs` (local = a `FakeLocal` engine recording calls and returning "local:<n>")

```rust
#[test] fn routes_to_host_when_in_use_and_never_loads_local() { let h = FakeHost::start(HostBehaviour::Ok { text: "remote".into(), polished: "".into(), token: None }); let st = RemoteState::new(); st.configure("PC", &h.base_url, None, "Mac"); st.enabled.store(true, SeqCst); st.healthy.store(true, SeqCst); let local = FakeLocal::default(); let loads = local.loads.clone(); let mut r = RoutingEngine::new(Box::new(local), RemoteEngine::new(st.clone(), Duration::from_secs(5)), st.clone(), Arc::new(Mutex::new(Some(PathBuf::from("/m.bin")))), true); r.load(Path::new("/m.bin"), true).unwrap(); assert_eq!(*loads.lock().unwrap(), 0); assert!(r.is_loaded()); assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "remote"); assert_eq!(h.requests.lock().unwrap()[0].client, "Mac"); }
#[test] fn falls_back_mid_dictation_and_keeps_order() { let h = FakeHost::start(HostBehaviour::Ok { text: "remote".into(), polished: "".into(), token: None }); let st = RemoteState::new(); st.configure("PC", &h.base_url, None, "Mac"); st.enabled.store(true, SeqCst); st.healthy.store(true, SeqCst); let mut r = RoutingEngine::new(Box::new(FakeLocal::default()), RemoteEngine::new(st.clone(), Duration::from_millis(500)), st.clone(), Arc::new(Mutex::new(Some(PathBuf::from("/m.bin")))), true); assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "remote"); drop(h); let t = r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap(); assert_eq!(t.text, "local:1"); assert!(!st.healthy.load(SeqCst)); assert_eq!(st.take_notice().as_deref(), Some("PC unreachable, transcribed locally")); assert!(st.take_notice().is_none(), "notice is consumed once"); }
#[test] fn disabled_remote_is_plain_local() { let st = RemoteState::new(); let mut r = RoutingEngine::new(Box::new(FakeLocal::default()), RemoteEngine::new(st.clone(), Duration::from_secs(1)), st.clone(), Arc::new(Mutex::new(None)), true); assert!(!r.is_loaded()); r.load(Path::new("/m.bin"), true).unwrap(); assert!(r.is_loaded()); assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "local:1"); }
#[test] fn session_indexes_chunks_in_order() { let h = FakeHost::start(HostBehaviour::Ok { text: "r".into(), polished: "".into(), token: None }); let st = RemoteState::new(); st.configure("PC", &h.base_url, None, "Mac"); st.enabled.store(true, SeqCst); st.healthy.store(true, SeqCst); let s1 = st.new_session(); let mut e = RemoteEngine::new(st.clone(), Duration::from_secs(2)); e.transcribe(&[0.0; 160], &DecodeOptions::default()).unwrap(); e.transcribe(&[0.0; 160], &DecodeOptions::default()).unwrap(); let reqs = h.requests.lock().unwrap().clone(); assert_eq!((reqs[0].session.clone(), reqs[0].index, reqs[1].index), (s1, 0, 1)); let s2 = st.new_session(); assert_ne!(s1, s2); }
```

- [ ] **Step 2: Run to verify they fail** — compile error.
- [ ] **Step 3: Implement** both files; export from `lib.rs`.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox-engine -- --test-threads=1` → all pass.
- [ ] **Step 5: Commit** — `git commit -m "feat(engine): RemoteEngine over the host API and RoutingEngine with local fallback"`

---

### Task 4: The host server

**Files:**
- Create: `src-tauri/src/host.rs`
- Modify: `src-tauri/Cargo.toml` (`tiny_http = "0.12"`, `local-ip-address = "0.6"`; dev-dependency `privatevox-core` `features = ["test-support"]`), `crates/privatevox-core/src/store.rs` (`NewDictation.engine_kind: String`, `Dictation.engine_kind`, `list_by_kind`? no: `list`/`search` already return all; add `engine_kind` to `COLUMNS`/`map_row`/insert; existing constructors set `"local"`).

**Interfaces:**
- Consumes: `SharedEngine`, `Arc<Mutex<Store>>`, `SharedPolisher`, `Vad`, `Arc<Mutex<Settings>>`, `MemoryReport` builder.
- Produces:
  - `pub struct HostDeps { pub engine: SharedEngine, pub store: Arc<Mutex<Store>>, pub polisher: SharedPolisher, pub vad: Arc<Mutex<Vad>>, pub settings: Arc<Mutex<Settings>>, pub model_path: Arc<Mutex<Option<PathBuf>>>, pub status: Arc<dyn Fn() -> StatusReply + Send + Sync>, pub models: Arc<dyn Fn() -> ModelsReply + Send + Sync>, pub select_model: Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync> }`
  - `pub struct HostServer { ... }` with `pub fn start(bind: &str /* "0.0.0.0:8765" or "127.0.0.1:0" */, token: Option<String>, deps: HostDeps) -> anyhow::Result<HostServer>`, `pub fn port(&self) -> u16`, `pub fn clients(&self) -> u32` (distinct client names seen in the last 60 s), `pub fn stop(self)` (also on `Drop`).
  - Routing: `GET /v1/status` → `deps.status()` with `clients` filled; `POST /v1/dictations` → decode (400 on bad body), VAD trim (`Vad::classify`; silent → `text: ""`), `pad_min 1.25`, engine lock → load if needed from `model_path` → transcribe with `language/translate`; append `(index, text)` to the session; if `final_` and the session's mode does not use the LLM → store the row now (`raw_text` = joined texts in index order, `polished_text: None`, `polish_status: "none"`, `app_name: client`, `engine_kind: "host"`, `mode`, `model`, `duration_ms` = samples/16); reply. `POST /v1/polish` → `polisher.polish(mode, text)` → store the session's row with raw = joined texts (or `text` if the session is unknown), `polished_text`, `polish_status`, `polished_by`; reply. `GET /v1/history` → `store.list/search` mapped to `RemoteRow`. `GET /v1/models`, `POST /v1/models/select`. Token: when set, every route requires `Authorization: Bearer <token>` else 401 `{"error":"token required"}`. Sessions older than 10 min are dropped on each request.
  - `pub fn lan_addresses() -> Vec<String>` (non-loopback IPv4 of this machine via `local-ip-address`).
  - One request handled at a time per connection; the server uses a small thread pool (4 workers); engine access serialises on the shared mutex.

- [ ] **Step 1: Write the failing tests** in `host.rs` (fake engine returning `"host:<n>"`, temp store, a `Polisher` over `FakeLlm`, real `Vad`)

```rust
#[test] fn status_and_token() { /* start on 127.0.0.1:0 with token "t"; HostClient without token → Unauthorized; with token → status.gpu.name == whatever deps.status returns */ }
#[test] fn dictation_stores_one_row_per_session_when_final_without_llm() { /* two chunks session "s", mode raw, second final → one row, raw_text "host:1 host:2", engine_kind "host", app_name "Mac" */ }
#[test] fn polish_stores_the_row_with_polished_text() { /* chunk (not final) then /v1/polish mode clean → reply polished, row has polished_text and polish_status ok */ }
#[test] fn bad_body_is_400() { /* raw POST with pcm "!!!" → ClientError::Server(msg) containing "base64" */ }
#[test] fn silent_audio_returns_empty_text_and_stores_nothing() { /* pcm of zeros, final → text "", no rows */ }
#[test] fn concurrent_requests_serialise_on_the_engine() { /* fake engine sleeps 200 ms; fire 3 dictations from 3 threads; all succeed; fake engine's in-flight counter never exceeds 1 */ }
#[test] fn stale_sessions_are_dropped_after_ten_minutes() { /* inject session with a timestamp 11 min old; next request drops it; assert via HostServer::session_count() */ }
#[test] fn history_lists_host_rows() { /* after the row test, /v1/history returns 1 row with engine_kind "host" */ }
```

- [ ] **Step 2: Run to verify they fail** — compile error, `HostServer` missing.
- [ ] **Step 3: Implement** `host.rs` and the store column plumbing.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox host -- --test-threads=1` and `cargo test -p privatevox-core` → all pass.
- [ ] **Step 5: Commit** — `git commit -m "feat(app): host server: status, dictations with sessions, polish, history, models; engine_kind on rows"`

---

### Task 5: Settings and app wiring (host on/off, remote state, health loop)

**Files:**
- Modify: `crates/privatevox-core/src/settings.rs` (`host: HostSettings { enabled: bool, port: u16, token: Option<String> }`, `remote: RemoteSettings { enabled: bool, base_url: Option<String>, token: Option<String>, label: String /* default "PC" */ }`, `machine_name: Option<String>` with `validate` checking the port range and the url when enabled)
- Create: `src-tauri/src/remote.rs` (owns `Arc<RemoteState>`, the remote health loop, `apply_settings(&Settings)`; builds `RoutingEngine`)
- Modify: `src-tauri/src/lib.rs` (engine = `RoutingEngine` around the worker; host server started/stopped from settings; `AppState` gets `remote: Arc<RemoteState>` and `host: Mutex<Option<HostServer>>`), `src-tauri/src/state.rs`, `src-tauri/src/commands.rs` (`set_settings` calls `remote::apply_settings` and `host::apply_settings`)

**Interfaces:**
- Produces in `remote.rs`: `pub fn apply_settings(state: &RemoteState, s: &Settings)` (configure or clear; `enabled` from settings), `pub fn spawn_health_loop(state: Arc<RemoteState>, app: AppHandle)` (every 10 s when enabled: `status()` with 1 s timeout → `healthy`, `last_status`, `last_error`; emits `remote-changed` on flips; backoff like LLM), `pub fn machine_name(s: &Settings) -> String` (setting, else the hostname via `hostname` sysctl/`COMPUTERNAME`, else "PrivateVox").
- Produces in `host.rs`: `pub fn apply_settings(app: &AppHandle, state: &AppState, s: &Settings)` (start when enabled and not running / port changed; stop when disabled).

- [ ] **Step 1: Write the failing tests** — settings: `host_and_remote_defaults_are_off`, `validate_rejects_bad_remote_url_when_enabled`; remote.rs: `machine_name_falls_back_to_hostname` (non-empty).
- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement** the wiring; the local worker engine is wrapped: `RoutingEngine::new(Box::new(worker_engine), RemoteEngine::new(remote.clone(), deadline), remote.clone(), model_path_slot, flash)`; the pipeline's `Deps.model_path` is also written into `model_path_slot` on `ModelReady`/switch (one shared `Arc<Mutex<Option<PathBuf>>>`).
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox -- --test-threads=1`, `cargo test -p privatevox-core`.
- [ ] **Step 5: Commit** — `git commit -m "feat(app): host and remote settings, routing engine wiring, remote health loop"`

---

### Task 6: Pipeline: remote polish, session boundaries, notices, row marker

**Files:**
- Modify: `src-tauri/src/pipeline.rs`, `src-tauri/src/lib.rs` (Deps closures)

**Interfaces:**
- `Deps` gains `pub remote: Arc<RemoteState>`.
- Behaviour pinned:
  - On `Cmd::Start`: `remote.new_session()` (so chunk indexes restart), and when `remote.in_use()` the local preload is skipped (`RoutingEngine::load` already handles it; the pipeline must not emit "loading model").
  - Polish: the `polish` closure in `lib.rs` checks `remote.in_use()`: if so → `HostClient::polish(PolishRequest { client, session, mode, text })` with the LLM timeout + grace; on error → local polisher and `notice = "PC unreachable, polished locally"`. For modes without the LLM when the host is in use, the pipeline sends a final empty dictation? No: the last chunk is sent with `final_: true` — the pipeline tells the engine the last chunk through `DecodeOptions.initial_prompt`? Ruling: add `pub final_chunk: bool` to `DecodeOptions` (default false); the chunk worker sets it on the remainder chunk in `finish_chunked` and `decode_and_paste`.
  - Pill: `PillState::Polishing` gains `where_: String` ("on PC" or ""); the pill shows "Polishing on PC…".
  - Row: `engine_kind = "remote:<label>"` when the dictation used the host (every chunk went remote and no fallback); `"local"` otherwise. Notice from `remote.take_notice()` prefixes the pasted preview like the polish notice.
- [ ] **Step 1: Write the failing tests** — `remote_dictation_marks_row_and_skips_local_load` (RemoteState configured against a FakeHost; harness engine is a RoutingEngine over the fake local; row.engine_kind == "remote:PC"; local loads == 0), `final_chunk_flag_set_on_last_chunk` (fake local engine records `final_chunk`), `fallback_notice_prefixes_preview` (host dropped before Stop → preview starts with "PC unreachable, transcribed locally"; row.engine_kind == "local").
- [ ] **Step 2: Run to verify they fail**.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run to verify they pass** — `cargo test -p privatevox -- --test-threads=1`.
- [ ] **Step 5: Commit** — `git commit -m "feat(app): remote polish, session boundaries, fallback notices, engine_kind marker"`

---

### Task 7: Commands: network status, remote history, host address, test host; strip from host status

**Files:**
- Create: `src-tauri/src/net_commands.rs`; Modify: `src-tauri/src/lib.rs` (register), `src-tauri/src/commands.rs` (`memory_report` returns host figures when `remote.in_use()`), `crates/privatevox-core/src/memory.rs` (`MemoryReport.source_label: Option<String>`, `budget_from_host(status: &StatusReply, selected: &str) -> MemoryReport`)

**Interfaces:**
- Commands: `network_status() -> NetworkStatus { host: HostStatus { enabled, running, port, addresses: Vec<String>, clients }, remote: RemoteStatus { enabled, label, base_url, healthy, in_use, last_error, status: Option<StatusReply> } }`; `test_host(url, token) -> Result<StatusReply, String>` (async + spawn_blocking; error copy "Nothing answered at {url}. Is PrivateVox running there with 'Act as host' on?"; token rejected → "The host wants a token; enter the one set on the PC"); `remote_history(limit, offset, search: Option<String>) -> Result<HistoryReply, String>` (spawn_blocking); `set_remote(enabled, base_url, token, label)`, `set_host(enabled, port, token)`; `remote_models() / select_remote_model(id)`.
- `budget_from_host`: total = gpu.total, used_by_others = gpu.used − whisper.resident − polish.resident, selected_bytes = whisper.resident (or estimate), llm_bytes = polish.resident, verdict rules as §9, `source_label = Some(label)`.

- [ ] **Step 1: Write the failing tests** — `budget_from_host_maps_status` (16 GB total, 5 GB used, whisper 1.3 GB, llm 2 GB → others 1.7 GB, projected 5 GB, percent 31, good, source_label "PC"), `status_explains_a_rejected_token` (helper `test_host_error(ClientError) -> String`), `unreachable_copy_names_the_url`.
- [ ] **Step 2: Run to verify they fail**. **Step 3: Implement.** **Step 4: Run** `cargo test -p privatevox -- --test-threads=1` and core. **Step 5: Commit** — `git commit -m "feat(app): network commands, remote history, host figures in the memory report"`

---

### Task 8: Panel and pill UI

**Files:** `ui/panel.html`, `ui/privatevox.css`, `ui/pill.html`

**Behaviour pinned:**
- Settings → **Network** card: "Act as host for other machines" toggle, port, token, and a live line "Serving on 192.168.1.5:8765 · 1 client" or "Off". Below: "Use a host on this network" toggle, `ip:port`, token, label, **Test** button showing the host's name, GPU and memory or the error copy; status dot like the LLM endpoints.
- History header: a segmented toggle "This Mac | <label>" (label from settings; hidden when remote is off). The second view calls `remote_history` with the search box; rows render read-only (Copy only), an info line "Showing <label>'s history. Edit, pin and delete work on that machine." Search works.
- Row meta: "via <label>" chip (accent colour) when `engine_kind` starts with `remote:`; "host" chip when `engine_kind == "host"` (the host's own panel shows rows it processed for clients, with `app_name` = client name).
- Memory strip: when the report has `source_label`, the heading reads "GPU memory · on <label>" and the machine box shows the host's adapter name; the Whisper model dropdown drives `select_remote_model` while the host is in use.
- Pill: "Polishing on PC…" when `where_` is set; the fallback notice shows via the pasted preview as today.
- Status bar: `remote-changed` flashes "PC reachable, dictation goes there" / "PC unreachable, dictating locally".

- [ ] **Step 1: Implement.** **Step 2: `node --check` both inline scripts.** **Step 3: Smoke** (Task 9). **Step 4: Commit** — `git commit -m "feat(ui): Network settings, history source toggle, via-PC marker, host figures in the strip"`

---

### Task 9: Live check on one Mac (host and client in one process) and docs

- [ ] **Step 1:** Start the debug app with host mode on (port 8765) and remote on pointing at `127.0.0.1:8765` (the app talks to itself; the engine lock serialises). Dictate via `PRIVATEVOX_DEBUG_SCRIPT` with `say`. Expected: log shows the chunk going to `/v1/dictations`, `/v1/polish` called, two rows: one `engine_kind = remote:PC` (client copy) and one `engine_kind = host` with `app_name` = machine name; pill preview without a fallback notice.
- [ ] **Step 2:** Stop host mode while remote stays on → next dictation pastes with "PC unreachable, transcribed locally" and `engine_kind = local`; turn host mode on → within 20 s `remote-changed` reports reachable.
- [ ] **Step 3:** Record numbers in `docs/superpowers/plans/2026-10-28-privatevox-host-remote-ledger.md`; README "Use your PC as the host" section; CHANGELOG Unreleased; roadmap 4a rows to done. Commit — `git commit -m "docs: host mode and the remote interface"`.
