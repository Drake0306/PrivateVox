//! Crash reports (design §10): a panic anywhere in the app writes `crash-<unix ts>.txt` next to the log, with the
//! version, OS, model and the last pill state, so the owner can read what happened without a debugger. The Setup
//! tab offers "Show report" when one exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The report body. `last_state` is the pill state the coordinator last emitted (JSON), when known.
pub fn report_text(version: &str, os: &str, model: &str, last_state: Option<&str>, panic: &str) -> String {
    let when = chrono_free_timestamp();
    format!(
        "PrivateVox crash report\n\nversion: {version}\nos: {os}\nmodel: {model}\nlast state: {}\nwhen (unix seconds): {when}\n\n{panic}\n",
        last_state.unwrap_or("unknown")
    )
}

/// Writes `text` to `dir/crash-<unix ts>.txt` and returns the path. Creates `dir` when missing. Never panics: a
/// failure to write is returned as the path that was attempted, so the hook can still log it.
pub fn write(dir: &Path, text: &str) -> PathBuf {
    let _ = std::fs::create_dir_all(dir);
    let mut path = dir.join(format!("crash-{}.txt", chrono_free_timestamp()));
    // Two panics in the same second (one per thread) must not overwrite each other.
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("crash-{}-{n}.txt", chrono_free_timestamp()));
        n += 1;
    }
    let _ = std::fs::write(&path, text);
    path
}

/// The newest `crash-*.txt` in `dir`: by the timestamp in its name, then by the same-second suffix (`-1`, `-2`, …
/// are later than the bare name), or `None`.
pub fn latest(dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<(u64, u64, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let stem = name.strip_prefix("crash-")?.strip_suffix(".txt")?;
            let (ts, suffix) = stem.split_once('-').unwrap_or((stem, "0"));
            Some((ts.parse::<u64>().ok()?, suffix.parse::<u64>().ok()?, p))
        })
        .collect();
    reports.sort();
    reports.pop().map(|(_, _, p)| p)
}

/// Installs the panic hook: every panic writes a report to `dir`, then the default hook prints it as before.
/// `context()` is asked at panic time for (model id, last pill state as JSON), so the report reflects the moment.
pub fn install_hook(dir: PathBuf, version: &'static str, context: Arc<dyn Fn() -> (String, Option<String>) + Send + Sync>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let (model, last_state) = context();
        let text = report_text(version, &os_label(), &model, last_state.as_deref(), &info.to_string());
        let path = write(&dir, &text);
        log::error!("panic: report written to {}", path.display());
        default(info);
    }));
}

/// "windows 10.0.26300" / "macos 15.1" style label (sysinfo lives in the core crate).
pub fn os_label() -> String {
    privatevox_core::os_label()
}

/// Unix seconds; the report does not need a calendar library.
fn chrono_free_timestamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_text_has_version_os_model_state() {
        let t = report_text("0.6.0", "windows 10.0.26300", "large-v3-turbo", Some(r#"{"state":"listening"}"#), "panicked at src/x.rs:1:1:\nboom");
        for needle in ["version: 0.6.0", "os: windows 10.0.26300", "model: large-v3-turbo", r#"last state: {"state":"listening"}"#, "boom", "when (unix seconds): "] {
            assert!(t.contains(needle), "missing {needle:?} in {t:?}");
        }
        assert!(report_text("0.6.0", "macos", "m", None, "p").contains("last state: unknown"));
    }

    #[test]
    fn write_and_latest_pick_the_newest() {
        let d = tempfile::tempdir().unwrap();
        assert!(latest(d.path()).is_none(), "no reports yet");
        std::fs::write(d.path().join("crash-1700000000.txt"), "old").unwrap();
        std::fs::write(d.path().join("crash-1700000500.txt"), "newer").unwrap();
        std::fs::write(d.path().join("privatevox.log"), "not a report").unwrap();
        std::fs::write(d.path().join("crash-notanumber.txt"), "ignored").unwrap();
        assert_eq!(latest(d.path()).unwrap().file_name().unwrap(), "crash-1700000500.txt");
        let written = write(d.path(), "fresh");
        assert!(written.file_name().unwrap().to_str().unwrap().starts_with("crash-"));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "fresh");
        assert_eq!(latest(d.path()).unwrap(), written, "the one just written is the newest");
        let second = write(d.path(), "same second");
        assert_ne!(second, written, "a second report in the same second gets its own file");
        assert_eq!(latest(d.path()).unwrap(), second, "the suffixed same-second report is the newer one");
        std::fs::write(d.path().join("crash-1800000000.txt"), "later").unwrap();
        std::fs::write(d.path().join("crash-1800000000-2.txt"), "later still").unwrap();
        assert_eq!(latest(d.path()).unwrap().file_name().unwrap(), "crash-1800000000-2.txt");
        let missing = write(&d.path().join("nested").join("deeper"), "made the dir");
        assert!(missing.exists());
    }

    #[test]
    fn os_label_names_the_platform() {
        assert!(os_label().starts_with(std::env::consts::OS));
    }
}
