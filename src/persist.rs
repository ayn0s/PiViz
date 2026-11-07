//! Remembers the settings, the last MIDI port, SoundFont and camera, the reference points in the
//! camera image, the saved profiles and the color presets between runs.
//!
//! Stored as TOML in the user's config directory. Saving is debounced (one write a second
//! after the last change) and flushed when the app exits.

use crate::colors::ColorScheme;
use crate::settings::Settings;
use crate::webcam::{CameraMode, Corners, DEFAULT_CORNERS};
use bevy::app::AppExit;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Seconds after the last change before the settings are written.
const SAVE_DELAY: f64 = 1.0;

/// What was used last time, beyond the settings themselves.
#[derive(Resource, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Name of the last MIDI port that connected successfully.
    pub midi_port: Option<String>,
    /// Last SoundFont that loaded successfully.
    pub soundfont: Option<PathBuf>,
    /// Last camera chosen or opened.
    pub camera: Option<String>,
    /// Capture format picked in the panel; `None` lets PiViz choose.
    pub camera_mode: Option<CameraMode>,
    /// WebSocket remote control (see `docs/API.md`): on or off, and its port on 127.0.0.1.
    pub api_enabled: bool,
    /// Port of the remote control server.
    pub api_port: u16,
    /// Reference points in the camera image (they depend on where the camera stands, not on
    /// the profile).
    pub camera_points: Corners,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            midi_port: None,
            soundfont: None,
            camera: None,
            camera_mode: None,
            api_enabled: true,
            api_port: crate::api::DEFAULT_PORT,
            camera_points: DEFAULT_CORNERS,
        }
    }
}

/// Named snapshots created by the user.
#[derive(Resource, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    /// Complete settings, by name.
    pub profiles: BTreeMap<String, Settings>,
    /// Color schemes, by name (in addition to the built-in presets).
    pub color_presets: BTreeMap<String, ColorScheme>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
/// Everything written to `settings.toml`.
struct SavedState {
    /// The `[settings]` section.
    settings: Settings,
    /// The `[session]` section.
    session: Session,
    /// The `[library]` section.
    library: Library,
}

/// `%APPDATA%\piviz\settings.toml` on Windows, `$XDG_CONFIG_HOME/piviz/settings.toml` elsewhere.
fn config_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    dir.map(|d| d.join("piviz").join("settings.toml"))
}

/// Reads the saved state; a missing or unreadable file just means defaults.
pub fn load() -> (Settings, Session, Library) {
    let Some(path) = config_path() else {
        return Default::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    match toml::from_str::<SavedState>(&text) {
        Ok(state) => (state.settings, state.session, state.library),
        Err(e) => {
            warn!("ignoring unreadable settings {}: {e}", path.display());
            Default::default()
        }
    }
}

/// Writes the state to [`config_path`], through a temporary file renamed over the old one, so a
/// crash mid-write never leaves a truncated file.
fn save(state: &SavedState) {
    let Some(path) = config_path() else { return };
    let result = toml::to_string_pretty(state)
        .map_err(|e| e.to_string())
        .and_then(|text| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            // Write then rename, so a crash mid-write never leaves a truncated file.
            let tmp = path.with_extension("toml.tmp");
            std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
        });
    if let Err(e) = result {
        warn!("cannot save settings to {}: {e}", path.display());
    }
}

/// Saves the settings, session and library automatically.
pub struct PersistPlugin;

impl Plugin for PersistPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Last, autosave);
    }
}

/// Saves one second after the last change to the settings, session or library, and immediately when
/// the app exits.
fn autosave(
    time: Res<Time<Real>>,
    settings: Res<Settings>,
    session: Res<Session>,
    library: Res<Library>,
    mut exits: MessageReader<AppExit>,
    mut due: Local<Option<f64>>,
) {
    let now = time.elapsed_secs_f64();
    let changed = (settings.is_changed() && !settings.is_added())
        || (session.is_changed() && !session.is_added())
        || (library.is_changed() && !library.is_added());
    if changed {
        *due = Some(now + SAVE_DELAY);
    }
    let exiting = exits.read().count() > 0;
    if let Some(at) = *due
        && (now >= at || exiting)
    {
        save(&SavedState {
            settings: settings.clone(),
            session: session.clone(),
            library: library.clone(),
        });
        *due = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colors::NoteColoring;
    use crate::settings::Background;

    #[test]
    fn round_trip_keeps_everything_but_the_panel() {
        let mut settings = Settings {
            fall_speed: 640.0,
            background: Background::Transparent,
            show_keyboard: false,
            show_panel: false,
            ..Settings::default()
        };
        settings.colors.coloring = NoteColoring::Gradient;
        settings.colors.gradient = vec![[10, 20, 30], [200, 100, 0], [255, 255, 255]];
        let mut library = Library::default();
        library.profiles.insert("Stream".into(), settings.clone());
        library
            .color_presets
            .insert("Bleu".into(), settings.colors.clone());
        let session = Session {
            midi_port: Some("Digital Piano".into()),
            soundfont: Some(PathBuf::from("C:/sf/piano.sf2")),
            camera: Some("USB Camera".into()),
            camera_mode: Some(CameraMode {
                width: 1920,
                height: 1080,
                fps: 60,
                format: "MJPEG".into(),
            }),
            api_enabled: false,
            api_port: 9000,
            camera_points: [[0.1, 0.5], [0.9, 0.52], [0.95, 0.7], [0.05, 0.68]],
        };
        let text = toml::to_string_pretty(&SavedState {
            settings: settings.clone(),
            session: session.clone(),
            library: library.clone(),
        })
        .unwrap();
        let back: SavedState = toml::from_str(&text).unwrap();
        assert!(back.session == session);
        assert!(
            back.settings.show_panel,
            "the panel must always show at startup"
        );
        settings.show_panel = true;
        assert!(back.settings == settings);
        assert_eq!(back.library.color_presets, library.color_presets);
        assert!(back.library.profiles["Stream"].colors == settings.colors);
    }

    #[test]
    fn former_per_channel_coloring_reads_as_gradient() {
        let text = "[settings.colors]\ncoloring = \"channel\"\nchannels = [[255, 0, 0]]\n";
        let back: SavedState = toml::from_str(text).unwrap();
        assert_eq!(back.settings.colors.coloring, NoteColoring::Gradient);
    }

    #[test]
    fn colors_saved_before_the_glow_gradient_keep_a_solid_glow() {
        let text = "[settings.colors]
accent = [255, 0, 0]
";
        let back: SavedState = toml::from_str(text).unwrap();
        assert_eq!(back.settings.colors.accent_coloring, NoteColoring::Solid);
        assert_eq!(back.settings.colors.accent, [255, 0, 0]);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let back: SavedState = toml::from_str("[settings]\nfall_speed = 500.0\n").unwrap();
        assert_eq!(back.settings.fall_speed, 500.0);
        assert!(back.settings.show_keyboard);
        assert!(back.session.midi_port.is_none());
        assert_eq!(back.session.camera_points, DEFAULT_CORNERS);
        assert!(back.library.profiles.is_empty());
    }
}
