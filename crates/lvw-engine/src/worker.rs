//! The engine worker process: the only place native Whisper code runs. Reads requests on stdin, writes
//! responses on stdout, logs on stderr, and exits as soon as stdin closes (the app quit or died).
//!
//! `LVW_WORKER_FAKE=1` turns it into a fake that answers without a model (used by the supervisor tests);
//! `LVW_WORKER_SLEEP_MS` makes the fake slow.

use crate::backend::WhisperBackend;
use crate::policy::audio_ctx_for;
use crate::protocol::{read_frame, write_frame, Request, Response};
use std::io::Write;
use std::time::{Duration, Instant};

pub fn run_worker() -> i32 {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).try_init();
    spawn_parent_watch();
    let fake = std::env::var("LVW_WORKER_FAKE").is_ok();
    let sleep_ms: u64 = std::env::var("LVW_WORKER_SLEEP_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut backend: Option<WhisperBackend> = None;
    let mut fake_loaded = false;

    loop {
        let req: Request = match read_frame(&mut input) {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                log::warn!("worker: stdin error, exiting: {e}");
                break;
            }
        };
        let resp = match req {
            Request::Ping => Response::Pong,
            Request::Load { model_path, flash_attn, use_gpu } => {
                if fake {
                    fake_loaded = true;
                    Response::Loaded { load_ms: 1, resident_bytes: 123_456_789 }
                } else {
                    let before = resident_bytes();
                    match WhisperBackend::load(&model_path, flash_attn, use_gpu) {
                        Ok((b, d)) => {
                            backend = Some(b);
                            let after = resident_bytes();
                            Response::Loaded { load_ms: d.as_millis() as u64, resident_bytes: after.saturating_sub(before) }
                        }
                        Err(e) => Response::Error { id: None, message: e.to_string() },
                    }
                }
            }
            Request::Unload => {
                backend = None;
                fake_loaded = false;
                Response::Unloaded
            }
            Request::Transcribe { id, pcm, opts } => {
                let seconds = pcm.len() as f32 / 16_000.0;
                if fake {
                    if sleep_ms > 0 {
                        std::thread::sleep(Duration::from_millis(sleep_ms));
                    }
                    if fake_loaded {
                        Response::Transcript { id, text: "fake".into(), language: opts.language.clone(), decode_ms: sleep_ms, audio_ctx: audio_ctx_for(seconds), reran: false }
                    } else {
                        Response::Error { id: Some(id), message: "no model loaded".into() }
                    }
                } else {
                    match backend.as_mut() {
                        None => Response::Error { id: Some(id), message: "no model loaded".into() },
                        Some(b) => {
                            let t = Instant::now();
                            match b.transcribe(&pcm, &opts) {
                                Ok(d) => Response::Transcript { id, text: d.text, language: d.language, decode_ms: t.elapsed().as_millis() as u64, audio_ctx: d.audio_ctx, reran: d.reran },
                                Err(e) => Response::Error { id: Some(id), message: e.to_string() },
                            }
                        }
                    }
                }
            }
            Request::Shutdown => break,
        };
        let mut out = stdout.lock();
        if write_frame(&mut out, &resp).is_err() || out.flush().is_err() {
            break;
        }
    }
    // Skip native destructors on the way out: ggml-metal teardown can assert, and the OS reclaims everything.
    std::mem::forget(backend);
    0
}

/// Resident set size of this process in bytes (0 if unavailable). On unified-memory Macs this includes
/// the model's GPU buffers, which is what the memory strip wants.
pub fn resident_bytes() -> u64 {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &std::process::id().to_string()]).output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().map(|kb| kb * 1024).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Exits the worker as soon as the parent is gone, even in the middle of a decode (the stdin EOF is only
/// noticed between frames). On Unix a reparented child sees `getppid() == 1`.
fn spawn_parent_watch() {
    #[cfg(unix)]
    {
        let parent = unsafe { libc::getppid() };
        std::thread::Builder::new()
            .name("lvw-parent-watch".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let now = unsafe { libc::getppid() };
                if now != parent || now == 1 {
                    log::warn!("worker: parent gone, exiting");
                    std::process::exit(0);
                }
            })
            .ok();
    }
}
