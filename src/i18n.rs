//! Translations: one flat key/value JSON file per language in `locales/`, embedded in the binary.
//!
//! Code refers to keys (`t.tr("midi.connect")`); values may contain `{name}` placeholders filled by
//! `t.trf`. A key missing in a language falls back to English, then to the key itself.
//! To add a language: add `locales/<code>.json` and a `Language` variant below.

use crate::audio::SoundFontState;
use crate::midi_live::MidiStatus;
use crate::webcam::{PropertiesError, WebcamStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Language of the interface.
pub enum Language {
    /// Follow the system language.
    #[default]
    Auto,
    /// English.
    En,
    /// French.
    Fr,
}

impl Language {
    /// Languages that have a translation file, in menu order.
    pub const AVAILABLE: [Language; 2] = [Language::En, Language::Fr];

    /// The translation file of this language, embedded in the binary.
    fn source(self) -> &'static str {
        match self {
            Language::En | Language::Auto => include_str!("../locales/en.json"),
            Language::Fr => include_str!("../locales/fr.json"),
        }
    }

    /// ISO code of the language, matched against the system locale.
    fn code(self) -> &'static str {
        match self {
            Language::En | Language::Auto => "en",
            Language::Fr => "fr",
        }
    }

    /// `Auto` resolved to the system language (detected once), when a translation exists.
    fn resolve(self) -> Language {
        if self != Language::Auto {
            return self;
        }
        static SYSTEM: OnceLock<Language> = OnceLock::new();
        *SYSTEM.get_or_init(|| {
            let locale = sys_locale::get_locale().unwrap_or_default().to_lowercase();
            Language::AVAILABLE
                .into_iter()
                .find(|l| locale.starts_with(l.code()))
                .unwrap_or(Language::En)
        })
    }
}

/// Translations of one language: key to text.
type Table = HashMap<String, String>;

/// Parsed tables, built on first use.
fn table(lang: Language) -> &'static Table {
    static TABLES: OnceLock<HashMap<&'static str, Table>> = OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        Language::AVAILABLE
            .into_iter()
            .map(|l| {
                let table = serde_json::from_str(l.source()).unwrap_or_else(|e| {
                    panic!("locales/{}.json is not a valid string map: {e}", l.code())
                });
                (l.code(), table)
            })
            .collect()
    });
    &tables[lang.code()]
}

/// Translator for one language.
#[derive(Clone, Copy)]
pub struct Tr(Language);

impl Tr {
    /// A translator for `language`; `Auto` is resolved to the system language.
    pub fn new(language: Language) -> Self {
        Self(language.resolve())
    }

    /// The text of `key` in this language, else in English, else the key itself.
    pub fn tr(self, key: &str) -> &str {
        table(self.0)
            .get(key)
            .or_else(|| table(Language::En).get(key))
            .map_or(key, String::as_str)
    }

    /// Translates and fills `{name}` placeholders.
    pub fn trf(self, key: &str, args: &[(&str, &str)]) -> String {
        let mut text = self.tr(key).to_owned();
        for (name, value) in args {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }

    /// Name of a language in that language, for the language menu.
    pub fn language_name(language: Language) -> &'static str {
        Tr::new(language).tr("language.name")
    }

    /// Text of a MIDI connection status (empty when idle).
    pub fn midi_status(self, status: &MidiStatus) -> String {
        match status {
            MidiStatus::Idle => String::new(),
            MidiStatus::Unavailable(e) => self.trf("midi.status.unavailable", &[("error", e)]),
            MidiStatus::NoMatch(name) => self.trf("midi.status.no_match", &[("name", name)]),
            MidiStatus::Connecting => self.tr("midi.status.connecting").to_owned(),
            MidiStatus::Connected(name) => self.trf("midi.status.connected", &[("name", name)]),
            MidiStatus::Disconnected => self.tr("midi.status.disconnected").to_owned(),
            MidiStatus::PortNotFound => self.tr("midi.status.port_not_found").to_owned(),
            MidiStatus::ConnectFailed(e) => self.trf("midi.status.connect_failed", &[("error", e)]),
        }
    }

    /// Text of a camera status (empty when off).
    pub fn camera_status(self, status: &WebcamStatus) -> String {
        match status {
            WebcamStatus::Off => String::new(),
            WebcamStatus::Opening(name) => self.trf("camera.status.opening", &[("name", name)]),
            WebcamStatus::Running {
                name,
                width,
                height,
                fps,
            } => self.trf(
                "camera.status.running",
                &[
                    ("name", name),
                    ("width", &width.to_string()),
                    ("height", &height.to_string()),
                    ("fps", &fps.to_string()),
                ],
            ),
            WebcamStatus::NoDevice => self.tr("camera.no_devices").to_owned(),
            WebcamStatus::NotFound(name) => self.trf("camera.status.not_found", &[("name", name)]),
            WebcamStatus::Failed(e) => self.trf("camera.status.failed", &[("error", e)]),
            WebcamStatus::Lost => self.tr("camera.status.lost").to_owned(),
        }
    }

    // Only the Windows camera settings window can fail.
    #[cfg_attr(not(windows), allow(dead_code))]
    /// Why the camera settings window could not be shown.
    pub fn camera_properties_error(self, error: &PropertiesError) -> String {
        match error {
            PropertiesError::NotFound => self.tr("camera.properties_not_found").to_owned(),
            PropertiesError::NoSettings => self.tr("camera.properties_none").to_owned(),
            PropertiesError::Failed(e) => self.trf("camera.properties_failed", &[("error", e)]),
        }
    }

    /// Text of a SoundFont loading state (empty when idle).
    pub fn soundfont_status(self, state: &SoundFontState) -> String {
        match state {
            SoundFontState::Idle => String::new(),
            SoundFontState::NotFound(paths) => self.trf("soundfont.not_found", &[("paths", paths)]),
            SoundFontState::Loading => self.tr("soundfont.loading").to_owned(),
            SoundFontState::Loaded(name) => self.trf("soundfont.loaded", &[("name", name)]),
            SoundFontState::ReadError(e) => self.trf("soundfont.read_error", &[("error", e)]),
            SoundFontState::Invalid(e) => self.trf("soundfont.invalid", &[("error", e)]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<String> {
        text.split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name.to_owned()))
            .collect()
    }

    #[test]
    fn every_language_has_the_same_keys_and_placeholders() {
        let english = table(Language::En);
        for lang in Language::AVAILABLE {
            let other = table(lang);
            let missing: Vec<_> = english.keys().filter(|k| !other.contains_key(*k)).collect();
            let extra: Vec<_> = other.keys().filter(|k| !english.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{}: missing {missing:?}", lang.code());
            assert!(extra.is_empty(), "{}: unknown {extra:?}", lang.code());
            for (key, text) in english {
                assert_eq!(
                    placeholders(text),
                    placeholders(&other[key]),
                    "{}: placeholders differ for {key}",
                    lang.code()
                );
            }
        }
    }

    /// Catches typos: every literal key passed to `tr` or `trf` in the sources must exist.
    #[test]
    fn every_key_used_in_the_code_exists() {
        let english = table(Language::En);
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            // Application code only: tests may use unknown keys on purpose.
            let source = source.split("#[cfg(test)]").next().unwrap_or_default();
            for pattern in [".tr(\"", ".trf(\""] {
                for (start, _) in source.match_indices(pattern) {
                    let rest = &source[start + pattern.len()..];
                    let key = &rest[..rest.find('"').unwrap()];
                    assert!(
                        english.contains_key(key),
                        "{}: unknown translation key {key:?}",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn missing_keys_fall_back_to_the_key() {
        assert_eq!(Tr::new(Language::Fr).tr("no.such.key"), "no.such.key");
        assert_eq!(
            Tr::new(Language::En).trf("midi.status.connected", &[("name", "Piano")]),
            "Connected: Piano"
        );
    }

    #[test]
    fn language_round_trips_through_settings() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            language: Language,
        }
        let text = toml::to_string(&Holder {
            language: Language::Fr,
        })
        .unwrap();
        assert_eq!(text.trim(), "language = \"fr\"");
        let back: Holder = toml::from_str(&text).unwrap();
        assert_eq!(back.language, Language::Fr);
    }
}
