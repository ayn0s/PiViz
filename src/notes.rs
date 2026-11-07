//! Bars rising from the keys being played, pushed to the shared `DrawList`.

use crate::colors::Palette;
use crate::draw::DrawList;
use crate::keyboard::Layout;
use crate::midi_live::LiveNote;
use crate::settings::{Settings, glow};
use crate::shared::Frame;
use bevy::prelude::*;

/// Shortest bar drawn, in pixels, so a very short tap still shows.
const MIN_HEIGHT: f32 = 4.0;
/// Brightness multiplier of a bar while its key is held.
const HELD_BOOST: f32 = 1.3;
/// Bars keep moving this far past the top edge before being dropped (they are faded out by then).
const OFFSCREEN: f32 = 24.0;

/// A note bar: grows while its key is held, then floats up.
struct Bar {
    /// MIDI note number.
    key: u8,
    /// When the key went down (seconds since startup).
    start: f64,
    /// When the key went up; `None` while it is held.
    end: Option<f64>,
    /// Base color, taken from the palette when the note started.
    color: LinearRgba,
}

#[derive(Resource, Default)]
/// Every bar on screen.
struct LiveBars(Vec<Bar>);

/// Turns played notes into rising bars in the [`DrawList`].
pub struct NotesPlugin;

impl Plugin for NotesPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LiveBars(Vec::with_capacity(512)))
            .add_systems(
                Update,
                (track_notes, push_bars).chain().in_set(Frame::Gather),
            );
    }
}

/// Opens a bar on note-on and closes it on note-off.
fn track_notes(
    mut notes: MessageReader<LiveNote>,
    time: Res<Time>,
    palette: Res<Palette>,
    mut bars: ResMut<LiveBars>,
) {
    let now = time.elapsed_secs_f64();
    for note in notes.read() {
        if let Some(open) = bars
            .0
            .iter_mut()
            .find(|b| b.key == note.key && b.end.is_none())
        {
            open.end = Some(now);
        }
        if note.on {
            bars.0.push(Bar {
                key: note.key,
                start: now,
                end: None,
                color: palette.note_color(note.key),
            });
        }
    }
}

/// Bars grow upward from the key while held, then float away at the rise speed.
fn push_bars(
    time: Res<Time>,
    layout: Res<Layout>,
    settings: Res<Settings>,
    mut bars: ResMut<LiveBars>,
    mut draw: ResMut<DrawList>,
) {
    let now = time.elapsed_secs_f64();
    let speed = settings.fall_speed as f64;
    let (hit_y, top_y) = (layout.hit_y, layout.top_y);
    bars.0.retain(|b| {
        b.end
            .is_none_or(|end| (hit_y + ((now - end) * speed) as f32) < top_y + OFFSCREEN)
    });
    for bar in &bars.0 {
        let held = bar.end.is_none();
        let bottom = hit_y + ((now - bar.end.unwrap_or(now)) * speed) as f32;
        let top = (hit_y + ((now - bar.start) * speed) as f32).min(top_y + OFFSCREEN);
        let boost = if held { HELD_BOOST } else { 1.0 };
        let k = bar.key as usize;
        draw.note(
            Vec2::new(layout.key_x[k], (bottom + top) / 2.0),
            Vec2::new(layout.note_w[k], (top - bottom).max(MIN_HEIGHT)),
            glow(bar.color, settings.glow * boost),
            held,
        );
    }
}
