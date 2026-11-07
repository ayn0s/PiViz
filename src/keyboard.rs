//! The 88-key keyboard: layout computed from the window size, key sprites and their lighting.

use crate::colors::Palette;
use crate::effects::make_texture;
use crate::settings::{Background, Settings, glow};
use crate::shared::{FIRST_KEY, Frame, KeyLights, LAST_KEY, is_black};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

/// White keys on an 88-key piano.
const WHITE_KEYS: f32 = 52.0;
/// Z of the white keys.
const Z_WHITE: f32 = 10.0;
/// Z of the shadows cast by black keys.
const Z_SHADOW: f32 = 10.5;
/// Z of the black keys.
const Z_BLACK: f32 = 11.0;
/// Z of the line on top of the keys.
const Z_HIT_LINE: f32 = 12.0;

/// Screen geometry shared by every visual plugin (world units = logical pixels, origin at center).
#[derive(Resource, Clone, PartialEq)]
pub struct Layout {
    /// Window width (logical pixels).
    pub width: f32,
    /// Window height (logical pixels).
    pub height: f32,
    /// Top edge of the keyboard, where falling notes hit.
    pub hit_y: f32,
    /// Whether the keyboard is shown (and reserves space at the bottom).
    pub keyboard: bool,
    /// Bottom of the window.
    pub bottom_y: f32,
    /// Top of the window.
    pub top_y: f32,
    /// Width covered by the 88 columns (narrower than the window when the keyboard is hidden).
    pub span_w: f32,
    /// Width of the soft fade at the window border (0 when the keyboard is shown).
    pub edge_fade: f32,
    /// Width of a white key.
    pub white_w: f32,
    /// Height of a white key.
    pub white_h: f32,
    /// Width of a black key.
    pub black_w: f32,
    /// Height of a black key.
    pub black_h: f32,
    /// Center x of each MIDI key.
    pub key_x: [f32; 128],
    /// Width of the notes falling on each key.
    pub note_w: [f32; 128],
}

impl Layout {
    /// Without the keyboard, the playing area is inset from the window edges so glows,
    /// flames and halos fade out instead of being cut by the border.
    pub fn new(width: f32, height: f32, keyboard: bool) -> Self {
        let (pad_x, pad_bottom) = if keyboard {
            (0.0, 0.0)
        } else {
            (
                (width * 0.035).clamp(24.0, 90.0),
                (height * 0.09).clamp(40.0, 110.0),
            )
        };
        let span_w = width - 2.0 * pad_x;
        let white_w = span_w / WHITE_KEYS;
        let white_h = (white_w * 5.2).min(height * 0.22);
        let black_w = white_w * 0.58;
        let black_h = white_h * 0.62;
        let left = -span_w / 2.0;
        let bottom_y = -height / 2.0;

        let mut key_x = [0.0; 128];
        let mut note_w = [0.0; 128];
        let mut white_index = 0.0;
        for key in FIRST_KEY..=LAST_KEY {
            let k = key as usize;
            if is_black(key) {
                // Black keys sit on the boundary after the previous white key, nudged like a real piano.
                let nudge = match key % 12 {
                    1 | 6 => -0.10,
                    3 | 10 => 0.10,
                    _ => 0.0,
                };
                key_x[k] = left + (white_index + nudge) * white_w;
                note_w[k] = black_w * 0.9;
            } else {
                key_x[k] = left + (white_index + 0.5) * white_w;
                note_w[k] = white_w * 0.78;
                white_index += 1.0;
            }
        }
        Self {
            width,
            height,
            hit_y: bottom_y + if keyboard { white_h } else { pad_bottom },
            keyboard,
            bottom_y,
            top_y: height / 2.0,
            span_w,
            edge_fade: pad_x.min(pad_bottom) * 0.9,
            white_w,
            white_h,
            black_w,
            black_h,
            key_x,
            note_w,
        }
    }
}

impl Layout {
    /// Position of world `x` across the 88 keys: 0 at the left end, 1 at the right end
    /// (clamped), for colors that run along the keyboard.
    pub fn keyboard_t(&self, x: f32) -> f32 {
        (x / self.span_w + 0.5).clamp(0.0, 1.0)
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(1600.0, 900.0, true)
    }
}

#[derive(Component)]
/// A key sprite, with its MIDI note number.
struct Key(u8);

/// One of the segments of the line on top of the keys (segments, so it can follow a gradient).
#[derive(Component)]
struct HitLine(usize);

/// Number of segments of the line on top of the keys.
const HIT_LINE_SEGMENTS: usize = 32;

/// Shadow cast by a black key on the white keys.
#[derive(Component)]
struct KeyShadow(u8);

/// Everything hidden by the "show keyboard" setting.
#[derive(Component)]
struct KeyboardPart;

/// The screen layout and the virtual keyboard.
pub struct KeyboardPlugin;

impl Plugin for KeyboardPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Layout>()
            .add_systems(Startup, spawn_keys)
            .add_systems(Update, update_layout.in_set(Frame::Input))
            .add_systems(
                Update,
                (
                    place_keys,
                    color_keys,
                    toggle_keyboard.run_if(resource_changed::<Settings>),
                )
                    .in_set(Frame::Visuals),
            );
    }
}

/// Rebuilds the [`Layout`] when the window size or the keyboard mode changes (the keyboard is never
/// shown on the camera background).
fn update_layout(
    window: Single<&Window, With<PrimaryWindow>>,
    settings: Res<Settings>,
    mut layout: ResMut<Layout>,
) {
    let (w, h) = (window.width(), window.height());
    // Over the camera, the scene is laid out as without a keyboard: margins to fade out in, and
    // flames mirrored below the line.
    let keyboard = settings.show_keyboard && settings.background != Background::Camera;
    if w > 1.0
        && h > 1.0
        && (w != layout.width || h != layout.height || keyboard != layout.keyboard)
    {
        *layout = Layout::new(w, h, keyboard);
    }
}

/// Over the camera the real keyboard is visible: the virtual one is never drawn (the corners are
/// aligned with the flame line).
fn toggle_keyboard(settings: Res<Settings>, mut parts: Query<&mut Visibility, With<KeyboardPart>>) {
    let shown = settings.show_keyboard && settings.background != Background::Camera;
    let visibility = if shown {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    for mut vis in &mut parts {
        vis.set_if_neq(visibility);
    }
}

/// Creates the key textures (procedural, no image files) and spawns the 88 keys, the black keys'
/// shadows and the line segments; [`place_keys`] sizes them.
fn spawn_keys(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    // Shading baked into textures (v = 0 at the top): light shadow at the top, front lip, side falloff.
    let white_tex = images.add(make_texture(16, 128, |u, v| {
        let side = 0.88 + 0.12 * smoothstep(0.0, 0.18, u.min(1.0 - u));
        let top_shadow = 0.85 + 0.15 * smoothstep(0.0, 0.05, v);
        let body = 1.0 - 0.08 * v;
        let lip = if v > 0.93 {
            1.04 - 90.0 * (v - 0.93).powi(2)
        } else {
            1.0
        };
        let s = (side * top_shadow * body * lip).clamp(0.0, 1.0);
        [s, s, s, 1.0]
    }));
    let black_tex = images.add(make_texture(16, 128, |u, v| {
        let edge = u.min(1.0 - u);
        // Bevelled sides catch a little light; the front face (bottom) is lighter.
        let sides = 0.35 + 0.3 * (1.0 - smoothstep(0.0, 0.2, edge));
        let top = 0.3 + 0.1 * v;
        let front = smoothstep(0.84, 0.9, v) * 0.35;
        let gloss = (1.0 - ((u - 0.38) / 0.08).powi(2)).max(0.0) * 0.25 * (1.0 - v);
        let s = (sides.max(top) + front + gloss).clamp(0.0, 1.0);
        [s, s, s, 1.0]
    }));
    for key in FIRST_KEY..=LAST_KEY {
        let (z, image) = if is_black(key) {
            (Z_BLACK, black_tex.clone())
        } else {
            (Z_WHITE, white_tex.clone())
        };
        commands.spawn((
            Key(key),
            KeyboardPart,
            Sprite::from_image(image),
            Transform::from_xyz(0.0, 0.0, z),
        ));
        if is_black(key) {
            commands.spawn((
                KeyShadow(key),
                KeyboardPart,
                Sprite::from_color(Color::srgba(0.0, 0.0, 0.0, 0.4), Vec2::ONE),
                Transform::from_xyz(0.0, 0.0, Z_SHADOW),
            ));
        }
    }
    for i in 0..HIT_LINE_SEGMENTS {
        commands.spawn((
            HitLine(i),
            KeyboardPart,
            Sprite::default(),
            Transform::from_xyz(0.0, 0.0, Z_HIT_LINE),
        ));
    }
}

/// Smooth 0 → 1 ramp between `e0` and `e1`.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Repositions keys only when the window size changes.
#[allow(clippy::type_complexity)]
fn place_keys(
    layout: Res<Layout>,
    mut keys: Query<(&Key, &mut Sprite, &mut Transform), (Without<HitLine>, Without<KeyShadow>)>,
    mut shadows: Query<(&KeyShadow, &mut Sprite, &mut Transform), (Without<Key>, Without<HitLine>)>,
    mut line: Query<(&HitLine, &mut Sprite, &mut Transform), (Without<Key>, Without<KeyShadow>)>,
) {
    if !layout.is_changed() {
        return;
    }
    for (Key(key), mut sprite, mut tf) in &mut keys {
        tf.translation.x = layout.key_x[*key as usize];
        if is_black(*key) {
            sprite.custom_size = Some(Vec2::new(layout.black_w, layout.black_h));
            tf.translation.y = layout.hit_y - layout.black_h / 2.0;
        } else {
            // A small gap between white keys draws the separators.
            sprite.custom_size = Some(Vec2::new(layout.white_w - 1.5, layout.white_h));
            tf.translation.y = layout.bottom_y + layout.white_h / 2.0;
        }
    }
    for (KeyShadow(key), mut sprite, mut tf) in &mut shadows {
        sprite.custom_size = Some(Vec2::new(layout.black_w * 1.12, layout.black_h * 1.05));
        tf.translation.x = layout.key_x[*key as usize] + layout.black_w * 0.14;
        tf.translation.y = layout.hit_y - layout.black_h * 0.525 - 3.0;
    }
    let segment = layout.width / HIT_LINE_SEGMENTS as f32;
    for (HitLine(i), mut sprite, mut tf) in &mut line {
        sprite.custom_size = Some(Vec2::new(segment, 2.0));
        tf.translation.x = -layout.width / 2.0 + segment * (*i as f32 + 0.5);
        tf.translation.y = layout.hit_y;
    }
}

#[allow(clippy::type_complexity)]
/// Tints the keys (lit keys take their note color) and the line segments (the glow color at their
/// position).
fn color_keys(
    lights: Res<KeyLights>,
    palette: Res<Palette>,
    settings: Res<Settings>,
    mut keys: Query<(&Key, &mut Sprite), Without<HitLine>>,
    mut line: Query<(&HitLine, &mut Sprite), Without<Key>>,
) {
    let white = LinearRgba::from(Color::srgb(0.92, 0.91, 0.94));
    let black = LinearRgba::from(Color::srgb(0.2, 0.19, 0.22));
    for (Key(key), mut sprite) in &mut keys {
        let k = *key as usize;
        let color = match (lights.lit[k], is_black(*key)) {
            (true, false) => white.mix(&glow(lights.color[k], 1.5), 0.85),
            (true, true) => glow(lights.color[k], 2.4),
            (false, false) => white,
            (false, true) => black,
        };
        sprite.color = Color::from(color);
    }
    for (HitLine(i), mut sprite) in &mut line {
        let t = (*i as f32 + 0.5) / HIT_LINE_SEGMENTS as f32;
        sprite.color = Color::from(glow(palette.accent_at(t), 1.0 + settings.glow));
    }
}
