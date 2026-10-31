//! The `Engine` trait and `WorkerEngine`, the single owner of the child worker process (design §18.1).
//!
//! One worker at a time, by construction. A hung or crashed worker is killed and respawned on the next call,
//! reloading the remembered model. Dropping the engine closes the worker's stdin, which makes it exit.

use crate::policy::deadline_for;
use crate::protocol::{read_frame, write_frame, DecodeOptions, Request, Response};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub decode_ms: u64,
    pub audio_ctx: i32,
    pub reran: bool,
}

/// What a load cost: wall time and the worker's resident-memory growth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadInfo {
    pub duration: Duration,
    pub resident_bytes: u64,
}

pub trait Engine: Send {
    fn load(&mut self, model: &Path, flash_attn: bool) -> Result<LoadInfo>;
    fn unload(&mut self) -> Result<()>;
    fn is_loaded(&self) -> bool;
    fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> Result<Transcript>;
    fn cancel(&mut self);
    /// Bytes the loaded model holds (0 when nothing is loaded).
    fn resident_bytes(&self) -> u64 {
        0
    }
}

const LOAD_DEADLINE: Duration = Duration::from_secs(180);
const STDERR_TAIL: usize = 50;

struct Worker {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Response>,
}

pub struct WorkerEngine {
    cmd: Command,
    worker: Option<Worker>,
    model: Option<(PathBuf, bool)>,
    loaded: bool,
    next_id: u64,
    deadline_override: Option<Duration>,
    last_failure: Option<String>,
    last_load: Option<LoadInfo>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// Current worker pid (0 when none), readable without the engine lock so an exit path can kill a busy worker.
    pid: Arc<AtomicU32>,
}

/// A mutex-free handle to the worker's pid.
#[derive(Clone)]
pub struct PidHandle(Arc<AtomicU32>);

impl PidHandle {
    pub fn pid(&self) -> Option<u32> {
        match self.0.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p),
        }
    }

    /// Kills the worker outright (SIGKILL). For the exit path when the engine lock is held by a decode.
    pub fn kill(&self) -> bool {
        match self.pid() {
            Some(pid) => {
                #[cfg(unix)]
                {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL) == 0 }
                }
                #[cfg(not(unix))]
                {
                    std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
                }
            }
            None => false,
        }
    }
}

impl WorkerEngine {
    /// `worker` is the command that starts the worker process, e.g. the app binary with `--engine-worker`.
    pub fn new(worker: Command) -> Self {
        WorkerEngine {
            cmd: worker,
            worker: None,
            model: None,
            loaded: false,
            next_id: 1,
            deadline_override: None,
            last_failure: None,
            last_load: None,
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            pid: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn last_load(&self) -> Option<LoadInfo> {
        self.last_load
    }

    pub fn pid_handle(&self) -> PidHandle {
        PidHandle(self.pid.clone())
    }

    /// Fixed decode deadline instead of the clip-length policy (tests).
    pub fn with_deadline_override(mut self, d: Duration) -> Self {
        self.deadline_override = Some(d);
        self
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.worker.as_ref().map(|w| w.child.id())
    }

    pub fn last_failure(&self) -> Option<String> {
        self.last_failure.clone()
    }

    pub fn last_stderr(&self) -> Vec<String> {
        self.stderr_tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
    }

    fn spawn(&mut self) -> Result<()> {
        let mut child = self
            .cmd
            // The worker waits on this process and exits when it is gone (Windows has no getppid-style reparenting).
            .env("PRIVATEVOX_PARENT_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn engine worker")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("worker stdout missing"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("worker stderr missing"))?;
        let (tx, rx) = channel::<Response>();
        std::thread::Builder::new().name("privatevox-worker-reader".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame::<_, Response>(&mut reader) {
                    Ok(Some(resp)) => {
                        if tx.send(resp).is_err() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        })?;
        let tail = self.stderr_tail.clone();
        std::thread::Builder::new().name("privatevox-worker-stderr".into()).spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::debug!("worker: {line}");
                if let Ok(mut t) = tail.lock() {
                    if t.len() >= STDERR_TAIL {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            }
        })?;
        log::info!("engine worker started (pid {})", child.id());
        self.pid.store(child.id(), Ordering::SeqCst);
        self.worker = Some(Worker { child, stdin, responses: rx });
        Ok(())
    }

    fn kill_worker(&mut self, reason: &str) {
        if let Some(mut w) = self.worker.take() {
            drop(w.stdin.take());
            let _ = w.child.kill();
            let _ = w.child.wait();
            log::warn!("engine worker killed: {reason}");
        }
        self.pid.store(0, Ordering::SeqCst);
        self.loaded = false;
        self.last_failure = Some(format!("{reason}; worker stderr tail: {}", self.last_stderr().join(" | ")));
    }

    fn worker_alive(&mut self) -> bool {
        match self.worker.as_mut() {
            Some(w) => matches!(w.child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Makes sure a live worker exists with the remembered model loaded.
    fn ensure_ready(&mut self) -> Result<()> {
        if !self.worker_alive() {
            if self.worker.is_some() {
                self.kill_worker("worker exited unexpectedly");
            }
            self.spawn()?;
            if let Some((path, flash)) = self.model.clone() {
                self.send_load(&path, flash)?;
            }
        }
        Ok(())
    }

    fn send(&mut self, req: &Request) -> Result<()> {
        let w = self.worker.as_mut().ok_or_else(|| anyhow!("no worker"))?;
        let stdin = w.stdin.as_mut().ok_or_else(|| anyhow!("worker stdin closed"))?;
        write_frame(stdin, req).context("write to worker")
    }

    fn recv(&mut self, deadline: Duration) -> Result<Response> {
        let w = self.worker.as_mut().ok_or_else(|| anyhow!("no worker"))?;
        match w.responses.recv_timeout(deadline) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => bail!("timed out after {:.1} s", deadline.as_secs_f32()),
            Err(RecvTimeoutError::Disconnected) => bail!("worker exited unexpectedly"),
        }
    }

    fn send_load(&mut self, path: &Path, flash_attn: bool) -> Result<LoadInfo> {
        let t = Instant::now();
        self.send(&Request::Load { model_path: path.to_path_buf(), flash_attn, use_gpu: true })?;
        match self.recv(LOAD_DEADLINE) {
            Ok(Response::Loaded { resident_bytes, .. }) => {
                self.loaded = true;
                let info = LoadInfo { duration: t.elapsed(), resident_bytes };
                self.last_load = Some(info);
                Ok(info)
            }
            Ok(Response::Error { message, .. }) => {
                self.loaded = false;
                bail!("model load failed: {message}")
            }
            Ok(other) => bail!("unexpected worker reply to load: {other:?}"),
            Err(e) => {
                self.kill_worker(&format!("load: {e}"));
                Err(e)
            }
        }
    }
}

impl Engine for WorkerEngine {
    fn load(&mut self, model: &Path, flash_attn: bool) -> Result<LoadInfo> {
        self.model = Some((model.to_path_buf(), flash_attn));
        if !self.worker_alive() {
            if self.worker.is_some() {
                self.kill_worker("worker exited unexpectedly");
            }
            self.spawn()?;
        }
        self.send_load(model, flash_attn)
    }

    fn unload(&mut self) -> Result<()> {
        if self.worker_alive() {
            let _ = self.send(&Request::Unload);
            let _ = self.recv(Duration::from_secs(10));
        }
        // Dropping the worker frees everything the model held; a fresh one is spawned on the next load.
        if let Some(mut w) = self.worker.take() {
            drop(w.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && matches!(w.child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = w.child.kill();
            let _ = w.child.wait();
        }
        self.pid.store(0, Ordering::SeqCst);
        self.loaded = false;
        Ok(())
    }

    fn is_loaded(&self) -> bool {
        self.loaded && self.worker.is_some()
    }

    fn transcribe(&mut self, pcm: &[f32], opts: &DecodeOptions) -> Result<Transcript> {
        self.ensure_ready()?;
        if !self.loaded {
            bail!("no model loaded");
        }
        let id = self.next_id;
        self.next_id += 1;
        let seconds = pcm.len() as f32 / 16_000.0;
        let deadline = self.deadline_override.unwrap_or_else(|| deadline_for(seconds));
        if let Err(e) = self.send(&Request::Transcribe { id, pcm: pcm.to_vec(), opts: opts.clone() }) {
            self.kill_worker(&format!("send failed: {e}"));
            return Err(e);
        }
        loop {
            match self.recv(deadline) {
                Ok(Response::Transcript { id: rid, text, language, decode_ms, audio_ctx, reran }) if rid == id => {
                    return Ok(Transcript { text, language, decode_ms, audio_ctx, reran });
                }
                Ok(Response::Error { id: Some(rid), message }) if rid == id => bail!("decode failed: {message}"),
                Ok(_) => continue, // a stale reply from a cancelled request
                Err(e) => {
                    self.kill_worker(&format!("decode {e}"));
                    return Err(e);
                }
            }
        }
    }

    fn cancel(&mut self) {
        self.kill_worker("cancelled");
    }

    fn resident_bytes(&self) -> u64 {
        if self.is_loaded() {
            self.last_load.map(|l| l.resident_bytes).unwrap_or(0)
        } else {
            0
        }
    }
}

impl Drop for WorkerEngine {
    fn drop(&mut self) {
        self.pid.store(0, Ordering::SeqCst);
        if let Some(mut w) = self.worker.take() {
            drop(w.stdin.take());
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && matches!(w.child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = w.child.kill();
            let _ = w.child.wait();
        }
    }
}
