//! `RoutingEngine`: local or remote by a shared `RemoteState`, with fallback to local and a one-shot notice (design §8.6).

use crate::protocol::DecodeOptions;
use crate::remote::{RemoteEngine, RemoteState, FALLBACK_NOTICE};
use crate::supervisor::{Engine, LoadInfo, Transcript};
use anyhow::Result;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

thread_local! {
    static FORCE_LOCAL: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with the host ignored on this thread: a host serving a client must do the work itself, never forward
/// it (to itself, when one machine plays both roles, or to a third machine).
pub fn with_local_only<R>(f: impl FnOnce() -> R) -> R {
    let before = FORCE_LOCAL.with(|c| c.replace(true));
    let r = f();
    FORCE_LOCAL.with(|c| c.set(before));
    r
}

fn local_only() -> bool {
    FORCE_LOCAL.with(|c| c.get())
}

pub struct RoutingEngine {
    local: Box<dyn Engine>,
    remote: RemoteEngine,
    state: Arc<RemoteState>,
    /// The model the local engine would load if the host disappears (kept current by the app).
    model_path: Arc<Mutex<Option<PathBuf>>>,
    flash: bool,
}

impl RoutingEngine {
    pub fn new(local: Box<dyn Engine>, remote: RemoteEngine, state: Arc<RemoteState>, model_path: Arc<Mutex<Option<PathBuf>>>, flash: bool) -> Self {
        RoutingEngine { local, remote, state, model_path, flash }
    }

    fn ensure_local_loaded(&mut self) -> Result<()> {
        if self.local.is_loaded() {
            return Ok(());
        }
        let path = self.model_path.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("no local model available to fall back to"))?;
        self.local.load(&path, self.flash).map(|_| ())
    }
}

impl Engine for RoutingEngine {
    fn load(&mut self, model: &Path, flash_attn: bool) -> Result<LoadInfo> {
        *self.model_path.lock().unwrap() = Some(model.to_path_buf());
        self.flash = flash_attn;
        if self.state.in_use() && !local_only() {
            return self.remote.load(model, flash_attn);
        }
        self.local.load(model, flash_attn)
    }

    fn set_use_gpu(&mut self, gpu: bool) {
        self.local.set_use_gpu(gpu);
    }

    fn unload(&mut self) -> Result<()> {
        self.local.unload()
    }

    fn is_loaded(&self) -> bool {
        (self.state.in_use() && !local_only()) || self.local.is_loaded()
    }

    fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> Result<Transcript> {
        if self.state.in_use() && !local_only() {
            match self.remote.transcribe(pcm, opts) {
                Ok(t) => return Ok(t),
                Err(e) => {
                    log::warn!("remote transcription failed, falling back to local: {e}");
                    self.state.set_notice(FALLBACK_NOTICE);
                }
            }
        }
        self.ensure_local_loaded()?;
        self.local.transcribe(pcm, opts)
    }

    fn cancel(&mut self) {
        self.remote.cancel();
        self.local.cancel();
    }

    fn resident_bytes(&self) -> u64 {
        self.local.resident_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DecodeOptions;
    use crate::remote::{RemoteEngine, RemoteState};
    use crate::supervisor::{Engine, LoadInfo, Transcript};
    use privatevox_core::net::fake_host::{FakeHost, HostBehaviour};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering::SeqCst;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct FakeLocal {
        loads: Arc<Mutex<u32>>,
        calls: Arc<Mutex<u32>>,
        finals: Arc<Mutex<Vec<bool>>>,
        loaded: bool,
    }
    impl Engine for FakeLocal {
        fn load(&mut self, _m: &Path, _f: bool) -> anyhow::Result<LoadInfo> {
            *self.loads.lock().unwrap() += 1;
            self.loaded = true;
            Ok(LoadInfo { duration: Duration::ZERO, resident_bytes: 700 })
        }
        fn unload(&mut self) -> anyhow::Result<()> {
            self.loaded = false;
            Ok(())
        }
        fn is_loaded(&self) -> bool {
            self.loaded
        }
        fn transcribe(&mut self, _pcm: &[f32], opts: &DecodeOptions) -> anyhow::Result<Transcript> {
            *self.calls.lock().unwrap() += 1;
            self.finals.lock().unwrap().push(opts.final_chunk);
            Ok(Transcript { text: format!("local:{}", self.calls.lock().unwrap()), language: Some("en".into()), decode_ms: 1, audio_ctx: 768, reran: false })
        }
        fn cancel(&mut self) {}
        fn resident_bytes(&self) -> u64 {
            if self.loaded { 700 } else { 0 }
        }
    }

    fn in_use_state(h: &FakeHost) -> Arc<RemoteState> {
        let st = RemoteState::new();
        st.configure("PC", &h.base_url, None, "Mac");
        st.enabled.store(true, SeqCst);
        st.healthy.store(true, SeqCst);
        st
    }

    fn routing(local: FakeLocal, st: &Arc<RemoteState>, timeout_ms: u64, path: Option<&str>) -> RoutingEngine {
        RoutingEngine::new(Box::new(local), RemoteEngine::new(st.clone(), Duration::from_millis(timeout_ms)), st.clone(), Arc::new(Mutex::new(path.map(PathBuf::from))), true)
    }

    #[test]
    fn routes_to_host_when_in_use_and_never_loads_local() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "remote".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let local = FakeLocal::default();
        let loads = local.loads.clone();
        let mut r = routing(local, &st, 5000, Some("/m.bin"));
        r.load(Path::new("/m.bin"), true).unwrap();
        assert_eq!(*loads.lock().unwrap(), 0);
        assert!(r.is_loaded());
        assert_eq!(r.resident_bytes(), 0, "nothing resident on this machine while the host is in use");
        assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "remote");
        let req = h.requests.lock().unwrap()[0].clone();
        assert_eq!(req.client, "Mac");
        assert!(!req.final_);
        assert_eq!(*loads.lock().unwrap(), 0);
    }

    #[test]
    fn final_chunk_flag_reaches_the_host() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "r".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let mut r = routing(FakeLocal::default(), &st, 2000, None);
        let opts = DecodeOptions { final_chunk: true, language: Some("de".into()), translate: true, ..DecodeOptions::default() };
        r.transcribe(&[0.0; 160], &opts).unwrap();
        let req = h.requests.lock().unwrap()[0].clone();
        assert!(req.final_);
        assert_eq!(req.language.as_deref(), Some("de"));
        assert!(req.translate);
    }

    #[test]
    fn falls_back_mid_dictation_and_keeps_order() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "remote".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let local = FakeLocal::default();
        let loads = local.loads.clone();
        let mut r = routing(local, &st, 500, Some("/m.bin"));
        assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "remote");
        drop(h);
        let t = r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap();
        assert_eq!(t.text, "local:1");
        assert_eq!(*loads.lock().unwrap(), 1, "local model loaded lazily from the known path");
        assert!(!st.healthy.load(SeqCst));
        assert!(!st.in_use());
        assert_eq!(st.take_notice().as_deref(), Some("PC unreachable, transcribed locally"));
        assert!(st.take_notice().is_none(), "notice is consumed once");
        assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "local:2", "stays local until the health loop says otherwise");
    }

    #[test]
    fn disabled_remote_is_plain_local() {
        let st = RemoteState::new();
        let mut r = routing(FakeLocal::default(), &st, 1000, None);
        assert!(!r.is_loaded());
        r.load(Path::new("/m.bin"), true).unwrap();
        assert!(r.is_loaded());
        assert_eq!(r.resident_bytes(), 700);
        assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "local:1");
        r.unload().unwrap();
        assert!(!r.is_loaded());
    }

    #[test]
    fn session_indexes_chunks_in_order() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "r".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let s1 = st.new_session();
        let mut e = RemoteEngine::new(st.clone(), Duration::from_secs(2));
        e.transcribe(&[0.0; 160], &DecodeOptions::default()).unwrap();
        e.transcribe(&[0.0; 160], &DecodeOptions::default()).unwrap();
        let reqs = h.requests.lock().unwrap().clone();
        assert_eq!((reqs[0].session.clone(), reqs[0].index, reqs[1].index), (s1.clone(), 0, 1));
        let s2 = st.new_session();
        assert_ne!(s1, s2);
        e.transcribe(&[0.0; 160], &DecodeOptions::default()).unwrap();
        assert_eq!(h.requests.lock().unwrap()[2].index, 0, "index restarts with the session");
    }

    #[test]
    fn remote_without_a_client_is_not_in_use() {
        let st = RemoteState::new();
        st.enabled.store(true, SeqCst);
        st.healthy.store(true, SeqCst);
        assert!(!st.in_use(), "no client configured");
        st.configure("PC", "http://127.0.0.1:1", None, "Mac");
        assert!(st.in_use());
        st.clear();
        assert!(!st.in_use());
    }

    #[test]
    fn with_local_only_bypasses_the_host() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "remote".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let mut r = routing(FakeLocal::default(), &st, 2000, Some("/m.bin"));
        assert!(r.is_loaded());
        with_local_only(|| {
            assert!(!r.is_loaded(), "the local model is not loaded yet");
            assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "local:1");
        });
        assert!(h.requests.lock().unwrap().is_empty(), "nothing went to the host");
        assert_eq!(r.transcribe(&[0.1; 1600], &DecodeOptions::default()).unwrap().text, "remote", "back to routing outside the scope");
    }

    #[test]
    fn dictionary_prompt_reaches_the_host() {
        let h = FakeHost::start(HostBehaviour::Ok { text: "r".into(), polished: "".into(), token: None });
        let st = in_use_state(&h);
        let mut r = routing(FakeLocal::default(), &st, 2000, None);
        let opts = DecodeOptions { initial_prompt: Some("ChargeBee, Roy".into()), ..DecodeOptions::default() };
        r.transcribe(&[0.0; 160], &opts).unwrap();
        assert_eq!(h.requests.lock().unwrap()[0].initial_prompt.as_deref(), Some("ChargeBee, Roy"));
    }
}
