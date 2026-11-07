//! Note colors: a user-editable color scheme, built-in presets, and the linear palette derived from it.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// An sRGB color as edited in the panel and stored in settings.toml.
pub type Rgb = [u8; 3];

/// Most colors a gradient can have in the panel.
pub const MAX_GRADIENT_STOPS: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// How a set of colors is applied across the keyboard: notes, or the glow.
pub enum NoteColoring {
    /// Every note the same color.
    Solid,
    /// A gradient from the lowest to the highest key.
    /// (`channel`, the former per-channel mode, is read as a gradient.)
    #[default]
    #[serde(alias = "channel")]
    Gradient,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
/// The colors the user edits: notes, glow and background. Saved in the settings, in profiles and in
/// color presets.
pub struct ColorScheme {
    /// Notes: one color or a gradient.
    pub coloring: NoteColoring,
    /// Note color in solid mode.
    pub solid: Rgb,
    /// Low to high keys, 2 to `MAX_GRADIENT_STOPS` colors.
    pub gradient: Vec<Rgb>,
    /// The glow: flame line, ambient light and lines. Solid (`accent`) or a gradient across the
    /// keyboard (`accent_gradient`), like the notes. Settings saved before it existed keep the
    /// solid glow they had.
    #[serde(default = "solid_glow")]
    pub accent_coloring: NoteColoring,
    /// Glow color in solid mode.
    pub accent: Rgb,
    /// Low to high keys, 2 to `MAX_GRADIENT_STOPS` colors.
    pub accent_gradient: Vec<Rgb>,
    /// Window background when the background is set to its color.
    pub background: Rgb,
}

/// Default of [`ColorScheme::accent_coloring`] for files saved before it existed.
fn solid_glow() -> NoteColoring {
    NoteColoring::Solid
}

impl Default for ColorScheme {
    fn default() -> Self {
        builtin_presets().swap_remove(0).1
    }
}

impl ColorScheme {
    /// Keeps the gradients within the sizes the panel can edit.
    pub fn sanitize(&mut self) {
        let default = Self::default();
        for (stops, fallback) in [
            (&mut self.gradient, default.gradient),
            (&mut self.accent_gradient, default.accent_gradient),
        ] {
            if stops.len() < 2 {
                *stops = fallback;
            }
            stops.truncate(MAX_GRADIENT_STOPS);
        }
    }
}

/// Converts 0–1 floats to an sRGB byte color (to write the presets readably).
fn rgb(c: [f32; 3]) -> Rgb {
    c.map(|v| (v * 255.0).round() as u8)
}

/// Presets shipped with PiViz; their names are the same in every language.
/// They cannot be deleted.
pub fn builtin_presets() -> Vec<(&'static str, ColorScheme)> {
    let preset = |coloring, solid, gradient: &[[f32; 3]], accent, background| ColorScheme {
        coloring,
        solid: rgb(solid),
        gradient: gradient.iter().map(|&c| rgb(c)).collect(),
        // The glow follows the notes: the same gradient, or solid with solid notes.
        accent_coloring: coloring,
        accent: rgb(accent),
        accent_gradient: gradient.iter().map(|&c| rgb(c)).collect(),
        background: rgb(background),
    };
    vec![
        (
            "Ember",
            preset(
                NoteColoring::Gradient,
                [1.0, 0.45, 0.10],
                &[[0.85, 0.08, 0.18], [1.0, 0.42, 0.08], [1.0, 0.86, 0.35]],
                [1.0, 0.42, 0.12],
                [0.020, 0.012, 0.016],
            ),
        ),
        (
            "Aurora",
            preset(
                NoteColoring::Gradient,
                [0.12, 0.92, 0.76],
                &[[0.45, 0.20, 1.0], [0.20, 0.55, 1.0], [0.20, 1.0, 0.82]],
                [0.25, 0.95, 0.80],
                [0.008, 0.016, 0.035],
            ),
        ),
        (
            "Neon",
            preset(
                NoteColoring::Gradient,
                [1.0, 0.16, 0.62],
                &[[0.18, 0.35, 1.0], [0.72, 0.22, 1.0], [1.0, 0.18, 0.55]],
                [1.0, 0.20, 0.75],
                [0.004, 0.0, 0.012],
            ),
        ),
        (
            "Glacier",
            preset(
                NoteColoring::Gradient,
                [0.55, 0.82, 1.0],
                &[[0.15, 0.30, 0.85], [0.45, 0.75, 1.0], [0.92, 0.97, 1.0]],
                [0.60, 0.85, 1.0],
                [0.012, 0.018, 0.028],
            ),
        ),
        (
            "Gold",
            preset(
                NoteColoring::Solid,
                [1.0, 0.72, 0.25],
                &[[1.0, 0.55, 0.15], [1.0, 0.85, 0.45]],
                [1.0, 0.62, 0.20],
                [0.015, 0.010, 0.004],
            ),
        ),
    ]
}

/// Linear colors derived from the scheme, rebuilt only when it changes.
#[derive(Resource)]
pub struct Palette {
    /// Clear color of the Theme background.
    pub background: Color,
    /// Glow color when solid.
    accent: LinearRgba,
    /// Glow gradient stops, when it is a gradient.
    accent_gradient: Option<Vec<Rgb>>,
    /// Color of each key's notes (solid or sampled from the gradient).
    keys: [LinearRgba; 128],
}

/// Converts an sRGB byte color to linear.
fn linear(c: Rgb) -> LinearRgba {
    LinearRgba::from(Color::srgb_u8(c[0], c[1], c[2]))
}

impl Palette {
    /// Builds the palette from a color scheme: precomputes every key's note color.
    pub fn from_scheme(scheme: &ColorScheme) -> Self {
        let mut keys = [linear(scheme.solid); 128];
        if scheme.coloring == NoteColoring::Gradient && scheme.gradient.len() >= 2 {
            for (key, color) in keys.iter_mut().enumerate() {
                *color = sample_gradient(
                    &scheme.gradient,
                    ((key as f32 - 21.0) / 87.0).clamp(0.0, 1.0),
                );
            }
        }
        Self {
            background: Color::srgb_u8(
                scheme.background[0],
                scheme.background[1],
                scheme.background[2],
            ),
            accent: linear(scheme.accent),
            accent_gradient: (scheme.accent_coloring == NoteColoring::Gradient
                && scheme.accent_gradient.len() >= 2)
                .then(|| scheme.accent_gradient.clone()),
            keys,
        }
    }

    /// Base glow color at `t` across the keyboard (0 = lowest key, 1 = highest).
    pub fn accent_at(&self, t: f32) -> LinearRgba {
        match &self.accent_gradient {
            Some(stops) => sample_gradient(stops, t),
            None => self.accent,
        }
    }

    /// Base glow color above a key.
    pub fn accent_key(&self, key: u8) -> LinearRgba {
        self.accent_at((key as f32 - 21.0) / 87.0)
    }

    /// Base (non-HDR) color of a note.
    pub fn note_color(&self, key: u8) -> LinearRgba {
        self.keys[key as usize & 127]
    }
}

/// Samples evenly spaced stops at `t` in 0..1, mixing in Oklab for perceptually even blends.
pub fn sample_gradient(stops: &[Rgb], t: f32) -> LinearRgba {
    let Some(&first) = stops.first() else {
        return LinearRgba::WHITE;
    };
    if stops.len() == 1 {
        return linear(first);
    }
    let pos = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (pos.floor() as usize).min(stops.len() - 2);
    let a = Oklaba::from(linear(stops[i]));
    let b = Oklaba::from(linear(stops[i + 1]));
    LinearRgba::from(a.mix(&b, pos - i as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glow_follows_its_gradient_across_the_keyboard() {
        let close = |a: LinearRgba, b: LinearRgba| {
            (Vec4::from_array(a.to_f32_array()) - Vec4::from_array(b.to_f32_array())).length()
                < 1e-3
        };
        let mut scheme = ColorScheme {
            accent_coloring: NoteColoring::Solid,
            accent_gradient: vec![[255, 0, 0], [0, 0, 255]],
            ..ColorScheme::default()
        };
        let solid = Palette::from_scheme(&scheme);
        assert_eq!(solid.accent_key(21), solid.accent_key(108));

        scheme.accent_coloring = NoteColoring::Gradient;
        let palette = Palette::from_scheme(&scheme);
        // Ends of the keyboard take the end stops (up to the Oklab round trip).
        assert!(close(palette.accent_key(21), linear([255, 0, 0])));
        assert!(close(palette.accent_key(108), linear([0, 0, 255])));
        assert_eq!(palette.accent_at(-1.0), palette.accent_at(0.0));
    }

    #[test]
    fn short_glow_gradients_are_repaired() {
        let mut scheme = ColorScheme {
            accent_gradient: vec![[1, 2, 3]],
            ..ColorScheme::default()
        };
        scheme.sanitize();
        assert!(scheme.accent_gradient.len() >= 2);
    }
}
