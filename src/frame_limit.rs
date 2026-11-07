//! Optional frame rate cap, below the display's refresh rate (which vsync already enforces).

use crate::settings::Settings;
use bevy::prelude::*;
use std::time::{Duration, Instant};

/// Sleeping is only accurate to about a millisecond: the rest of the wait is spun.
const SPIN: Duration = Duration::from_millis(1);

/// Adds [`limit_frame_rate`] at the end of each frame.
pub struct FrameLimitPlugin;

impl Plugin for FrameLimitPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Last, limit_frame_rate);
    }
}

/// Waits at the end of the frame until the next one is due.
fn limit_frame_rate(settings: Res<Settings>, mut due: Local<Option<Instant>>) {
    if settings.fps_limit == 0 {
        *due = None;
        return;
    }
    let frame = Duration::from_secs_f64(1.0 / f64::from(settings.fps_limit));
    let now = Instant::now();
    // A late frame restarts the schedule instead of being followed by a burst to catch up.
    let target = due.map_or(now, |d| d + frame).max(now);
    if let Some(sleep) = target.checked_duration_since(now + SPIN) {
        std::thread::sleep(sleep);
    }
    while Instant::now() < target {
        std::hint::spin_loop();
    }
    *due = Some(target);
}
