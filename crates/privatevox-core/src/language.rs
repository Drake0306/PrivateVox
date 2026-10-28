//! Spoken languages: one means a fixed decode, several means detect and restrict to the list (design §18.7).

use serde::Serialize;

/// Whisper's language codes with English names, for the picker and for validation.
pub const WHISPER_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("zh", "Chinese"),
    ("de", "German"),
    ("es", "Spanish"),
    ("ru", "Russian"),
    ("ko", "Korean"),
    ("fr", "French"),
    ("ja", "Japanese"),
    ("pt", "Portuguese"),
    ("tr", "Turkish"),
    ("pl", "Polish"),
    ("ca", "Catalan"),
    ("nl", "Dutch"),
    ("ar", "Arabic"),
    ("sv", "Swedish"),
    ("it", "Italian"),
    ("id", "Indonesian"),
    ("hi", "Hindi"),
    ("fi", "Finnish"),
    ("vi", "Vietnamese"),
    ("he", "Hebrew"),
    ("uk", "Ukrainian"),
    ("el", "Greek"),
    ("ms", "Malay"),
    ("cs", "Czech"),
    ("ro", "Romanian"),
    ("da", "Danish"),
    ("hu", "Hungarian"),
    ("ta", "Tamil"),
    ("no", "Norwegian"),
    ("th", "Thai"),
    ("ur", "Urdu"),
    ("hr", "Croatian"),
    ("bg", "Bulgarian"),
    ("lt", "Lithuanian"),
    ("la", "Latin"),
    ("mi", "Maori"),
    ("ml", "Malayalam"),
    ("cy", "Welsh"),
    ("sk", "Slovak"),
    ("te", "Telugu"),
    ("fa", "Persian"),
    ("lv", "Latvian"),
    ("bn", "Bengali"),
    ("sr", "Serbian"),
    ("az", "Azerbaijani"),
    ("sl", "Slovenian"),
    ("kn", "Kannada"),
    ("et", "Estonian"),
    ("mk", "Macedonian"),
    ("br", "Breton"),
    ("eu", "Basque"),
    ("is", "Icelandic"),
    ("hy", "Armenian"),
    ("ne", "Nepali"),
    ("mn", "Mongolian"),
    ("bs", "Bosnian"),
    ("kk", "Kazakh"),
    ("sq", "Albanian"),
    ("sw", "Swahili"),
    ("gl", "Galician"),
    ("mr", "Marathi"),
    ("pa", "Punjabi"),
    ("si", "Sinhala"),
    ("km", "Khmer"),
    ("sn", "Shona"),
    ("yo", "Yoruba"),
    ("so", "Somali"),
    ("af", "Afrikaans"),
    ("oc", "Occitan"),
    ("ka", "Georgian"),
    ("be", "Belarusian"),
    ("tg", "Tajik"),
    ("sd", "Sindhi"),
    ("gu", "Gujarati"),
    ("am", "Amharic"),
    ("yi", "Yiddish"),
    ("lo", "Lao"),
    ("uz", "Uzbek"),
    ("fo", "Faroese"),
    ("ht", "Haitian Creole"),
    ("ps", "Pashto"),
    ("tk", "Turkmen"),
    ("nn", "Nynorsk"),
    ("mt", "Maltese"),
    ("sa", "Sanskrit"),
    ("lb", "Luxembourgish"),
    ("my", "Myanmar"),
    ("bo", "Tibetan"),
    ("tl", "Tagalog"),
    ("mg", "Malagasy"),
    ("as", "Assamese"),
    ("tt", "Tatar"),
    ("haw", "Hawaiian"),
    ("ln", "Lingala"),
    ("ha", "Hausa"),
    ("ba", "Bashkir"),
    ("jw", "Javanese"),
    ("su", "Sundanese"),
    ("yue", "Cantonese"),
];

pub fn is_known(code: &str) -> bool {
    WHISPER_LANGUAGES.iter().any(|(c, _)| *c == code)
}

pub fn name_of(code: &str) -> Option<&'static str> {
    WHISPER_LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// How the row's language was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageSource {
    /// One spoken language: decoded with it fixed, no detection.
    Fixed,
    /// Several spoken languages: the model's detection was on the list.
    Detected,
    /// Several spoken languages: detection missed the list (or failed), so the first language was used.
    Fallback,
}

impl LanguageSource {
    pub fn id(self) -> &'static str {
        match self {
            LanguageSource::Fixed => "fixed",
            LanguageSource::Detected => "detected",
            LanguageSource::Fallback => "fallback",
        }
    }
}

/// What to hand Whisper: a fixed language when exactly one is spoken, else auto-detect.
pub fn decode_language(spoken: &[String]) -> Option<String> {
    match spoken {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// After a decode: the language to record and whether a re-run with the first language is due (`Fallback`).
pub fn settle(spoken: &[String], detected: Option<&str>) -> (Option<String>, LanguageSource) {
    match spoken {
        [] => (detected.map(|d| d.to_string()), LanguageSource::Detected),
        [one] => (Some(one.clone()), LanguageSource::Fixed),
        many => match detected {
            Some(d) if many.iter().any(|l| l == d) => (Some(d.to_string()), LanguageSource::Detected),
            _ => (Some(many[0].clone()), LanguageSource::Fallback),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_language_is_fixed() {
        assert_eq!(decode_language(&l(&["en"])).as_deref(), Some("en"));
        assert_eq!(settle(&l(&["en"]), Some("fr")), (Some("en".into()), LanguageSource::Fixed));
        assert_eq!(settle(&l(&["de"]), None), (Some("de".into()), LanguageSource::Fixed));
    }

    #[test]
    fn several_languages_auto_detect() {
        assert_eq!(decode_language(&l(&["en", "de"])), None);
        assert_eq!(settle(&l(&["en", "de"]), Some("de")), (Some("de".into()), LanguageSource::Detected));
    }

    #[test]
    fn detected_language_off_the_list_falls_back_to_the_first() {
        assert_eq!(settle(&l(&["en", "de"]), Some("fr")), (Some("en".into()), LanguageSource::Fallback));
        assert_eq!(settle(&l(&["en", "de"]), None), (Some("en".into()), LanguageSource::Fallback));
        assert_eq!(settle(&[], Some("fr")), (Some("fr".into()), LanguageSource::Detected), "no list: the model decides and its pick is recorded");
    }

    #[test]
    fn whisper_language_table() {
        assert!(WHISPER_LANGUAGES.len() >= 95);
        assert!(is_known("en"));
        assert!(is_known("hi"));
        assert!(!is_known("xx"));
        assert_eq!(name_of("de"), Some("German"));
    }

    #[test]
    fn validate_rejects_unknown_language_code() {
        let mut s = crate::settings::Settings::default();
        s.spoken_languages = vec!["en".into(), "xx".into()];
        assert!(s.validate().unwrap_err().contains("language"));
        s.spoken_languages = vec!["en".into(), "de".into()];
        assert!(s.validate().is_ok());
    }
}
