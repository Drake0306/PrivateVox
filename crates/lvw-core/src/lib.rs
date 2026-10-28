//! LVW core: pure logic shared by the app and the engine worker. No Tauri dependency.

pub mod audio;
pub mod download;
pub mod hotkeys;
pub mod lifecycle;
pub mod memory;
pub mod models;
pub mod placement;
pub mod settings;
pub mod store;
pub mod textnorm;
pub mod vad;

/// Crate version, used by the panel's About line.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_semver() {
        assert_eq!(super::version().matches('.').count(), 2);
    }
}
