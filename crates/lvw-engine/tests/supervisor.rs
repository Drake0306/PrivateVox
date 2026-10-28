//! Supervisor tests against the fake worker. Run with `--test-threads=1`: they count live worker processes
//! by exact process name (`pgrep -x`), so other tests must not have workers alive at the same time.
use lvw_engine::protocol::DecodeOptions;
use lvw_engine::supervisor::{Engine, WorkerEngine};
use std::process::Command;
use std::time::Duration;

fn fake_worker(sleep_ms: u64) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_lvw-worker"));
    c.env("LVW_WORKER_FAKE", "1").env("LVW_WORKER_SLEEP_MS", sleep_ms.to_string());
    c
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

fn workers() -> Vec<String> {
    let out = Command::new("pgrep").args(["-xl", "lvw-worker"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).map(|l| l.to_string()).collect()
}

fn worker_count() -> usize {
    let w = workers();
    eprintln!("workers: {w:?}");
    w.len()
}

#[test]
fn supervisor_loads_and_transcribes_through_fake_worker() {
    let mut e = WorkerEngine::new(fake_worker(0));
    assert!(!e.is_loaded());
    e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    assert!(e.is_loaded());
    let t = e.transcribe(&vec![0.0; 16_000], &DecodeOptions::default()).unwrap();
    assert_eq!(t.text, "fake");
    assert_eq!(worker_count(), 1);
    e.unload().unwrap();
    assert!(!e.is_loaded());
}

#[test]
fn supervisor_times_out_and_respawns() {
    let mut e = WorkerEngine::new(fake_worker(3000)).with_deadline_override(Duration::from_millis(500));
    e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    let first_pid = e.child_pid().unwrap();
    let err = e.transcribe(&vec![0.0; 16_000], &DecodeOptions::default()).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("timed out"), "{err}");
    assert!(e.last_failure().is_some());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!alive(first_pid), "timed-out worker must be killed");
    let mut e2 = WorkerEngine::new(fake_worker(0));
    e2.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    assert_eq!(e2.transcribe(&vec![0.0; 16_000], &DecodeOptions::default()).unwrap().text, "fake");
}

#[test]
fn supervisor_respawns_after_worker_killed() {
    let mut e = WorkerEngine::new(fake_worker(0));
    e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    let pid = e.child_pid().unwrap();
    Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let first = e.transcribe(&vec![0.0; 16_000], &DecodeOptions::default());
    let second = e.transcribe(&vec![0.0; 16_000], &DecodeOptions::default()).unwrap();
    assert_eq!(second.text, "fake");
    assert_ne!(e.child_pid().unwrap(), pid);
    assert_eq!(worker_count(), 1, "exactly one worker alive after respawn (first call: {:?})", first.map(|t| t.text));
}

#[test]
fn dropping_supervisor_ends_worker() {
    let mut e = WorkerEngine::new(fake_worker(0));
    e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    let pid = e.child_pid().unwrap();
    drop(e);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "worker must exit when the supervisor is dropped");
}
