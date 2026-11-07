//! Types shared between plugins: key range, per-frame key lighting, messages and system ordering.

use bevy::prelude::*;
use std::path::PathBuf;

/// Lowest and highest keys of an 88-key piano (A0..C8).
pub const FIRST_KEY: u8 = 21;
/// Highest key of an 88-key piano (C8).
pub const LAST_KEY: u8 = 108;

/// Whether `key` (a MIDI note number) is a black key: C#, D#, F#, G# or A#.
pub fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Keys lit this frame. Reset every frame, then filled from live input.
#[derive(Resource)]
pub struct KeyLights {
    /// Whether each MIDI key is lit this frame.
    pub lit: [bool; 128],
    /// Color of each lit key (linear, before any glow multiplier); only meaningful where `lit` is
    /// true.
    pub color: [LinearRgba; 128],
}

impl Default for KeyLights {
    fn default() -> Self {
        Self {
            lit: [false; 128],
            color: [LinearRgba::BLACK; 128],
        }
    }
}

impl KeyLights {
    /// Lights `key` in `color` for this frame.
    pub fn light(&mut self, key: u8, color: LinearRgba) {
        let k = key as usize;
        self.lit[k] = true;
        self.color[k] = color;
    }
}

/// A key was struck: drives sparks and flashes.
#[derive(Message, Clone, Copy)]
pub struct KeyHit {
    /// MIDI note number.
    pub key: u8,
    /// How hard the key was struck, 1–127.
    pub velocity: u8,
    /// The note's base color, from the [`Palette`](crate::colors::Palette).
    pub color: LinearRgba,
}

#[derive(Message, Clone)]
/// Load this SoundFont file (sent by the startup, drag and drop, and the panel).
pub struct LoadSoundFont(pub PathBuf);

/// Per-frame ordering shared by all plugins.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Frame {
    /// Drain channels, handle user input and messages.
    Input,
    /// Reset per-frame state.
    Clock,
    /// Fill `KeyLights` and push the notes.
    Gather,
    /// Push state to sprites and to the draw list.
    Visuals,
    /// Upload the draw list to the GPU mesh.
    Draw,
}

/// Registers the shared resources, messages and the [`Frame`] ordering.
pub struct SharedPlugin;

impl Plugin for SharedPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<KeyLights>()
            .add_message::<KeyHit>()
            .add_message::<LoadSoundFont>()
            .configure_sets(
                Update,
                (
                    Frame::Input,
                    Frame::Clock,
                    Frame::Gather,
                    Frame::Visuals,
                    Frame::Draw,
                )
                    .chain(),
            )
            .add_systems(Update, reset_lights.in_set(Frame::Clock));
    }
}

/// Clears every key's lit flag at the start of each frame.
fn reset_lights(mut lights: ResMut<KeyLights>) {
    lights.lit = [false; 128];
}
