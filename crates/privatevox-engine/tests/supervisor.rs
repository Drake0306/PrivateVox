//! Supervisor tests against the fake worker. Run with `--test-threads=1`: they count live worker processes
//! by exact process name (`pgrep -x`), so other tests must not have workers alive at the same time.
use privatevox_engine::protocol::DecodeOptions;
use privatevox_engine::supervisor::{Engine, WorkerEngine};
use std::process::Command;
use std::time::Duration;

fn fake_worker(sleep_ms: u64) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_privatevox-worker"));
    c.env("PRIVATEVOX_WORKER_FAKE", "1").env("PRIVATEVOX_WORKER_SLEEP_MS", sleep_ms.to_string());
    c
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false)
}

fn workers() -> Vec<String> {
    let out = Command::new("pgrep").args(["-xl", "privatevox-worker"]).output().unwrap();
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

#[test]
fn pid_handle_kills_a_busy_worker_without_the_lock() {
    use std::sync::{Arc, Mutex};
    let mut e = WorkerEngine::new(fake_worker(3000));
    e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    let handle = e.pid_handle();
    let pid = handle.pid().unwrap();
    let shared: Arc<Mutex<WorkerEngine>> = Arc::new(Mutex::new(e));
    let busy = shared.clone();
    let t = std::thread::spawn(move || {
        let mut e = busy.lock().unwrap();
        e.transcribe(&vec![0.0; 16_000], &DecodeOptions::default()).map(|t| t.text)
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(shared.try_lock().is_err(), "the decode must be holding the lock");
    assert!(handle.kill(), "kill must reach the worker");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(pid), "worker must be gone right after the kill");
    let result = t.join().unwrap();
    assert!(result.is_err(), "the in-flight decode must fail, not hang");
}

#[test]
fn worker_exits_when_its_parent_dies_mid_decode() {
    // A throwaway parent shell starts the fake worker with a slow decode queued through a pipe, then the
    // shell is killed; the worker must notice the reparenting and exit on its own.
    let worker = env!("CARGO_BIN_EXE_privatevox-worker");
    let script = format!(
        "exec 3< <(sleep 60); PRIVATEVOX_WORKER_FAKE=1 PRIVATEVOX_WORKER_SLEEP_MS=20000 {worker} <&3 >/dev/null 2>&1 & echo $!; wait"
    );
    let mut parent = Command::new("bash").arg("-c").arg(&script).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(parent.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let worker_pid: u32 = line.trim().parse().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(worker_pid));
    Command::new("kill").args(["-9", &parent.id().to_string()]).status().unwrap();
    let _ = parent.wait();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while alive(worker_pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(worker_pid), "worker must exit within 3 s of its parent dying");
}

#[test]
fn load_reports_resident_bytes_from_worker() {
    let mut e = WorkerEngine::new(fake_worker(0));
    let info = e.load(std::path::Path::new("/nonexistent/model.bin"), true).unwrap();
    assert_eq!(info.resident_bytes, 123_456_789);
    assert_eq!(e.last_load().map(|l| l.resident_bytes), Some(123_456_789));
}
