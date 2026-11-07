//! User settings and command-line options.

use crate::colors::{ColorScheme, Palette};
use crate::i18n::{Language, Tr};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Resource, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// Everything the settings panel edits. Saved in `settings.toml` and in profiles; read by almost
/// every plugin.
pub struct Settings {
    /// Rise speed in logical pixels per second.
    pub fall_speed: f32,
    /// Note, glow and background colors.
    pub colors: ColorScheme,
    /// Bloom intensity, 0–1.
    pub bloom: f32,
    /// HDR multiplier applied to notes; above 1 they feed the bloom.
    pub glow: f32,
    /// Spark density multiplier (0 disables sparks).
    pub sparks: f32,
    /// Bevel and specular strength of the notes (0 = flat).
    pub relief: f32,
    /// Light halo around the notes.
    pub halo: f32,
    /// Sound volume, 0–1.5.
    pub volume: f32,
    /// Webcam image brightness under the notes (camera background).
    pub camera_brightness: f32,
    /// Not saved: the panel always shows at startup so it can never be lost.
    #[serde(skip, default = "always")]
    pub show_panel: bool,
    /// Whether the virtual keyboard is shown (ignored on the camera background).
    pub show_keyboard: bool,
    /// Frames per second cap; 0 leaves it to the display (vsync).
    pub fps_limit: u32,
    /// What is behind the notes.
    pub background: Background,
    /// Language of the interface.
    pub language: Language,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// What is behind the notes.
pub enum Background {
    /// The scheme's background color, with ambient light and octave lanes.
    #[default]
    Theme,
    /// Pure #000000 with no ambient light, for luma keying or screen blending in OBS.
    Black,
    /// No background: the image carries an alpha channel for capture tools (OBS game capture with
    /// transparency). The window itself still looks black on the desktop.
    Transparent,
    /// The webcam image, with the scene projected onto the real keyboard and screen-blended.
    Camera,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fall_speed: 320.0,
            colors: ColorScheme::default(),
            bloom: 0.28,
            glow: 1.6,
            sparks: 1.0,
            relief: 1.0,
            halo: 1.0,
            volume: 0.8,
            camera_brightness: 1.0,
            show_panel: true,
            show_keyboard: true,
            fps_limit: 0,
            background: Background::Theme,
            language: Language::Auto,
        }
    }
}

/// Serde default of [`Settings::show_panel`].
fn always() -> bool {
    true
}

impl Settings {
    /// Brings values from a (possibly hand-edited) file back into the panel's ranges.
    pub fn sanitize(&mut self) {
        self.fall_speed = self.fall_speed.clamp(80.0, 1500.0);
        self.bloom = self.bloom.clamp(0.0, 1.0);
        self.glow = self.glow.clamp(0.5, 4.0);
        self.sparks = self.sparks.clamp(0.0, 3.0);
        self.relief = self.relief.clamp(0.0, 2.0);
        self.halo = self.halo.clamp(0.0, 3.0);
        self.volume = self.volume.clamp(0.0, 1.5);
        self.camera_brightness = self.camera_brightness.clamp(0.1, 1.0);
        if self.fps_limit != 0 {
            self.fps_limit = self.fps_limit.clamp(10, 1000);
        }
        self.colors.sanitize();
    }
}

/// Scales RGB only (`LinearRgba * f32` would also scale alpha). Values above 1 feed the bloom.
pub fn glow(c: LinearRgba, k: f32) -> LinearRgba {
    LinearRgba::new(c.red * k, c.green * k, c.blue * k, c.alpha)
}

/// Command-line options: `piviz [--port NOM] [--soundfont F.sf2] [--black | --transparent]`.
#[derive(Resource, Default, Clone)]
pub struct CliArgs {
    /// SoundFont to load first (`--soundfont`).
    pub soundfont: Option<PathBuf>,
    /// Connect to the first MIDI port whose name contains this; otherwise the first instrument found.
    pub port: Option<String>,
    /// Background for this run (`--black`, `--transparent`), overriding the saved one.
    pub background: Option<Background>,
    /// Diagnostic mode: simulated playing, no vsync, frame times logged every second.
    pub bench: bool,
}

impl CliArgs {
    /// Reads the command line. Prints the help and exits for `--help`; warns about unknown options.
    pub fn parse() -> Self {
        let mut out = Self::default();
        // Runs before the settings are loaded, so it follows the system language.
        let t = Tr::new(Language::Auto);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    println!("{}", t.tr("cli.help"));
                    std::process::exit(0);
                }
                "-s" | "--soundfont" => out.soundfont = args.next().map(PathBuf::from),
                "-p" | "--port" => out.port = args.next(),
                "-b" | "--black" => out.background = Some(Background::Black),
                "-t" | "--transparent" => out.background = Some(Background::Transparent),
                "--bench" => out.bench = true,
                other => eprintln!("{}", t.trf("cli.unknown_option", &[("option", other)])),
            }
        }
        out
    }
}

/// Where the SoundFont is looked for when none is given on the command line.
pub fn soundfont_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(p) = std::env::var_os("PIVIZ_SOUNDFONT") {
        paths.push(PathBuf::from(p));
    }
    let data_dir = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    if let Some(dir) = data_dir {
        paths.push(dir.join("piviz/default.sf2"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        paths.push(PathBuf::from(appdata).join("piviz").join("default.sf2"));
    }
    paths.push(PathBuf::from("soundfonts/default.sf2"));
    paths.push(PathBuf::from("default.sf2"));
    // Next to the executable, e.g. in the install folder.
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(PathBuf::from))
    {
        paths.push(dir.join("default.sf2"));
    }
    paths
}

/// Loads the saved state and keeps the palette in sync with the settings.
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        let (mut settings, session, library) = crate::persist::load();
        let world = app.world();
        // The command line wins over the saved background.
        if let Some(background) = world
            .get_resource::<CliArgs>()
            .and_then(|cli| cli.background)
        {
            settings.background = background;
        }
        settings.sanitize();
        // The real palette and clear color are set by `rebuild_palette` on the first frame.
        app.insert_resource(Palette::from_scheme(&settings.colors))
            .insert_resource(ClearColor(Color::BLACK))
            .insert_resource(settings)
            .insert_resource(session)
            .insert_resource(library)
            .add_systems(
                Update,
                rebuild_palette
                    .run_if(resource_changed::<Settings>)
                    .in_set(crate::shared::Frame::Input),
            );
    }
}

/// Rebuilds the [`Palette`] and the clear color when the colors or the background change.
fn rebuild_palette(
    settings: Res<Settings>,
    mut palette: ResMut<Palette>,
    mut clear: ResMut<ClearColor>,
    mut current: Local<Option<(ColorScheme, Background)>>,
) {
    let key = (settings.colors.clone(), settings.background);
    if current.as_ref() == Some(&key) {
        return;
    }
    *palette = Palette::from_scheme(&settings.colors);
    clear.0 = match settings.background {
        Background::Theme => palette.background,
        // The camera mode screen-blends the scene: black adds nothing over the camera image.
        Background::Black | Background::Camera => Color::BLACK,
        Background::Transparent => Color::NONE,
    };
    *current = Some(key);
}
