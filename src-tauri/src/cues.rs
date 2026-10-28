//! Sound and visual cues on start, stop and "nothing heard" (phase 5, A5). Sounds play on a helper thread; never on
//! the pipeline thread, and never when the setting is off.

use privatevox_core::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Start,
    Stop,
    NothingHeard,
    Error,
}

/// The one gate: every cue respects the setting.
pub fn enabled(settings: &Settings, _cue: Cue) -> bool {
    settings.sound_cues
}

/// macOS system sounds, so nothing has to be bundled.
pub fn sound_file(cue: Cue) -> &'static str {
    match cue {
        Cue::Start => "/System/Library/Sounds/Tink.aiff",
        Cue::Stop => "/System/Library/Sounds/Pop.aiff",
        Cue::NothingHeard => "/System/Library/Sounds/Funk.aiff",
        Cue::Error => "/System/Library/Sounds/Basso.aiff",
    }
}

/// Play on a detached thread; a missing player or file is silently ignored.
pub fn play(cue: Cue) {
    #[cfg(target_os = "macos")]
    {
        let file = sound_file(cue);
        std::thread::Builder::new()
            .name("privatevox-cue".into())
            .spawn(move || {
                let _ = std::process::Command::new("afplay").arg("-v").arg("0.6").arg(file).status();
            })
            .ok();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cue;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use privatevox_core::settings::Settings;

    #[test]
    fn cues_are_off_able() {
        let mut s = Settings::default();
        assert!(s.sound_cues, "on by default");
        for c in [Cue::Start, Cue::Stop, Cue::NothingHeard, Cue::Error] {
            assert!(enabled(&s, c));
        }
        s.sound_cues = false;
        for c in [Cue::Start, Cue::Stop, Cue::NothingHeard, Cue::Error] {
            assert!(!enabled(&s, c));
        }
    }

    #[test]
    fn every_cue_maps_to_a_system_sound() {
        for c in [Cue::Start, Cue::Stop, Cue::NothingHeard, Cue::Error] {
            assert!(sound_file(c).ends_with(".aiff"));
        }
        assert_ne!(sound_file(Cue::Start), sound_file(Cue::Stop));
    }
}
