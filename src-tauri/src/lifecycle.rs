//! Exit path, idle unload and crash detection for the app (design §10, §18.1).

use crate::pipeline::Cmd;
use lvw_core::lifecycle::IdleTimer;
use lvw_engine::supervisor::Engine;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type SharedEngine = Arc<Mutex<Box<dyn Engine>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    Clean,
    /// The engine was busy past the deadline; the process exits anyway and the worker dies with its pipe.
    TimedOut,
}

/// Unloads the engine with a deadline. Called from the exit path; never blocks longer than `deadline`.
pub fn shutdown(engine: SharedEngine, deadline: Duration) -> ShutdownOutcome {
    let (tx, rx) = channel::<()>();
    std::thread::spawn(move || {
        if let Ok(mut e) = engine.lock() {
            if let Err(err) = e.unload() {
                log::warn!("unload on exit: {err}");
            }
        }
        let _ = tx.send(());
    });
    match rx.recv_timeout(deadline) {
        Ok(()) => ShutdownOutcome::Clean,
        Err(_) => {
            log::warn!("engine busy at exit; leaving the worker to die with its pipe");
            ShutdownOutcome::TimedOut
        }
    }
}

/// Background tick: unloads the model when the idle timer is due and tells the pipeline.
pub fn spawn_idle_watch(engine: SharedEngine, timer: Arc<Mutex<IdleTimer>>, tx: Sender<Cmd>, on_status: Box<dyn Fn(Option<Duration>) + Send>) {
    std::thread::Builder::new()
        .name("lvw-idle".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(15));
            let now = Instant::now();
            let due = timer.lock().map(|t| t.due(now)).unwrap_or(false);
            if due {
                if let Ok(mut e) = engine.try_lock() {
                    if e.is_loaded() {
                        log::info!("idle timer: unloading model");
                        let _ = e.unload();
                    }
                    if let Ok(mut t) = timer.lock() {
                        t.clear();
                    }
                    let _ = tx.send(Cmd::ModelUnloaded);
                }
            }
            let remaining = timer.lock().ok().and_then(|t| t.remaining(now));
            on_status(remaining);
        })
        .expect("idle thread");
}

#[cfg(test)]
mod tests {
    use super::*;
    use lvw_engine::protocol::DecodeOptions;
    use lvw_engine::supervisor::{Engine, Transcript};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct SlowEngine {
        unloaded: Arc<Mutex<bool>>,
    }
    impl Engine for SlowEngine {
        fn load(&mut self, _m: &Path, _f: bool) -> anyhow::Result<Duration> {
            Ok(Duration::ZERO)
        }
        fn unload(&mut self) -> anyhow::Result<()> {
            *self.unloaded.lock().unwrap() = true;
            Ok(())
        }
        fn is_loaded(&self) -> bool {
            true
        }
        fn transcribe(&mut self, _p: &[f32], _o: &DecodeOptions) -> anyhow::Result<Transcript> {
            std::thread::sleep(Duration::from_secs(5));
            Ok(Transcript { text: "late".into(), language: None, decode_ms: 5000, audio_ctx: 768, reran: false })
        }
        fn cancel(&mut self) {}
    }

    #[test]
    fn exit_path_completes_with_worker_busy() {
        let unloaded = Arc::new(Mutex::new(false));
        let engine: Arc<Mutex<Box<dyn Engine>>> = Arc::new(Mutex::new(Box::new(SlowEngine { unloaded: unloaded.clone() })));
        // Simulate a decode in flight on another thread holding the engine.
        let busy = engine.clone();
        std::thread::spawn(move || {
            let mut e = busy.lock().unwrap();
            let _ = e.transcribe(&[0.0; 16_000], &DecodeOptions::default());
        });
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        let outcome = shutdown(engine.clone(), Duration::from_secs(2));
        assert!(t.elapsed() < Duration::from_millis(2500), "took {:?}", t.elapsed());
        assert!(matches!(outcome, ShutdownOutcome::TimedOut), "{outcome:?}");
        // With nothing in flight the unload runs and is reported.
        let unloaded2 = Arc::new(Mutex::new(false));
        let engine2: Arc<Mutex<Box<dyn Engine>>> = Arc::new(Mutex::new(Box::new(SlowEngine { unloaded: unloaded2.clone() })));
        assert!(matches!(shutdown(engine2, Duration::from_secs(2)), ShutdownOutcome::Clean));
        assert!(*unloaded2.lock().unwrap());
    }
}
