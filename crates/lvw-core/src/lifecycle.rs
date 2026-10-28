//! Lock file (single instance + unclean-exit detection) and the idle-unload timer (design §10, §4.5).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Written at start with our pid, removed on clean exit. Present at start means the last run died.
pub struct LockFile {
    path: PathBuf,
}

impl LockFile {
    /// Returns the lock and whether a stale lock was found.
    pub fn acquire(path: &Path) -> Result<(LockFile, bool)> {
        let stale = path.exists();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, std::process::id().to_string()).with_context(|| format!("write {}", path.display()))?;
        Ok((LockFile { path: path.to_path_buf() }, stale))
    }

    pub fn release(self) {
        drop(self);
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Unloads the model after N minutes without a dictation. `None` means never.
#[derive(Debug, Clone)]
pub struct IdleTimer {
    minutes: Option<u32>,
    last_touch: Option<Instant>,
}

impl IdleTimer {
    pub fn new(minutes: Option<u32>) -> Self {
        IdleTimer { minutes, last_touch: None }
    }

    /// Changes the delay; an armed timer restarts from `now`.
    pub fn set_minutes(&mut self, minutes: Option<u32>, now: Instant) {
        self.minutes = minutes;
        if self.last_touch.is_some() {
            self.last_touch = Some(now);
        }
    }

    pub fn minutes(&self) -> Option<u32> {
        self.minutes
    }

    /// A model was used (or loaded): restart the countdown.
    pub fn touch(&mut self, now: Instant) {
        self.last_touch = Some(now);
    }

    /// Nothing is loaded any more: disarm.
    pub fn clear(&mut self) {
        self.last_touch = None;
    }

    pub fn due(&self, now: Instant) -> bool {
        match (self.minutes, self.last_touch) {
            (Some(m), Some(t)) => now.saturating_duration_since(t) >= Duration::from_secs(m as u64 * 60),
            _ => false,
        }
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match (self.minutes, self.last_touch) {
            (Some(m), Some(t)) => Some(Duration::from_secs(m as u64 * 60).saturating_sub(now.saturating_duration_since(t))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn lock_file_reports_stale_on_second_acquire_without_release() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lvw.lock");
        let (lock, stale) = LockFile::acquire(&p).unwrap();
        assert!(!stale);
        assert!(p.exists());
        std::mem::forget(lock); // simulate a crash: no Drop
        let (_lock2, stale2) = LockFile::acquire(&p).unwrap();
        assert!(stale2, "a lock left behind means the last run ended uncleanly");
    }

    #[test]
    fn lock_file_removed_on_drop() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lvw.lock");
        let (lock, _) = LockFile::acquire(&p).unwrap();
        drop(lock);
        assert!(!p.exists());
        let (_lock, stale) = LockFile::acquire(&p).unwrap();
        assert!(!stale);
    }

    #[test]
    fn idle_timer_due_after_minutes() {
        let t0 = Instant::now();
        let mut timer = IdleTimer::new(Some(2));
        timer.touch(t0);
        assert!(!timer.due(t0 + Duration::from_secs(119)));
        assert!(timer.due(t0 + Duration::from_secs(121)));
        assert_eq!(timer.remaining(t0 + Duration::from_secs(60)), Some(Duration::from_secs(60)));
    }

    #[test]
    fn idle_timer_never_when_none() {
        let t0 = Instant::now();
        let mut timer = IdleTimer::new(None);
        timer.touch(t0);
        assert!(!timer.due(t0 + Duration::from_secs(100_000)));
        assert_eq!(timer.remaining(t0), None);
    }

    #[test]
    fn idle_timer_set_minutes_reschedules() {
        let t0 = Instant::now();
        let mut timer = IdleTimer::new(Some(10));
        timer.touch(t0);
        timer.set_minutes(Some(1), t0 + Duration::from_secs(30));
        assert!(timer.due(t0 + Duration::from_secs(91)));
        timer.set_minutes(None, t0 + Duration::from_secs(92));
        assert!(!timer.due(t0 + Duration::from_secs(10_000)));
    }

    #[test]
    fn idle_timer_not_due_before_first_touch_when_unloaded() {
        let t0 = Instant::now();
        let timer = IdleTimer::new(Some(1));
        assert!(!timer.due(t0 + Duration::from_secs(3600)), "nothing to unload until something was loaded");
    }
}
