//! PrivateVox core: pure logic shared by the app and the engine worker. No Tauri dependency.

pub mod appmodes;
pub mod audio;
pub mod chunking;
pub mod commands;
pub mod dictionary;
pub mod download;
pub mod export;
pub mod hotkeys;
pub mod language;
pub mod lifecycle;
pub mod memory;
pub mod models;
pub mod net;
pub mod pastemethod;
pub mod placement;
pub mod settings;
pub mod silence;
pub mod store;
pub mod polish;
pub mod preview;
pub mod textnorm;
pub mod undo;
pub mod vad;

/// Crate version, used by the panel's About line.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// "windows 10.0.26300" / "macos 15.1" style label for logs and crash reports.
pub fn os_label() -> String {
    let name = std::env::consts::OS;
    match sysinfo::System::os_version() {
        Some(v) => format!("{name} {v}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_semver() {
        assert_eq!(super::version().matches('.').count(), 2);
    }
}
