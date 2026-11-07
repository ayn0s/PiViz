//! PiViz: a real-time visualizer for a live MIDI piano.
//!
//! Notes rise as glowing bars from the keys being played, over an animated flame line, with
//! sparks, bloom, SoundFont sound, an optional webcam background and a WebSocket remote control.
//!
//! The app is one Bevy `App` made of small plugins, one per module. They share state through
//! resources and messages, and every frame runs in the order set by [`shared::Frame`]: input,
//! clock, gather, visuals, draw.
//!
//! # Modules
//!
//! | Area | Modules |
//! |---|---|
//! | Frame structure | [`shared`] |
//! | Input and sound | [`midi_live`], [`audio`] |
//! | Visuals | [`keyboard`] (screen [`Layout`](keyboard::Layout)), [`notes`], [`effects`], [`draw`], [`compose`] |
//! | Camera mode | [`webcam`], [`homography`], `camera_properties` (Windows) |
//! | Settings and interface | [`settings`], [`persist`], [`colors`], [`i18n`], [`ui`], [`api`] |
//! | Platform | [`frame_limit`], [`icon`], [`resize_fix`] |
//!
//! # Threads
//!
//! Besides Bevy's, a few threads keep slow or real-time work off the frame: the MIDI callback
//! (it sends notes straight to the audio thread, so sound never waits for a frame), the audio
//! callback (never blocks), the SoundFont loader, the webcam capture and the remote control
//! server. They only talk to Bevy through channels or a mutex-protected slot.
//!
//! # Rendering
//!
//! The scene camera renders notes, flames and particles with HDR and bloom into an offscreen
//! image; a second camera draws it to the window through `shaders/compose.wgsl` (border fade,
//! transparency, webcam blending), then the egui panel on top. See [`compose`].
//!
//! The remote control protocol is documented in `docs/API.md`.

// Release builds on Windows are GUI apps: no console window when launched from the Start menu.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod audio;
#[cfg(windows)]
mod camera_properties;
mod colors;
mod compose;
mod draw;
mod effects;
mod frame_limit;
mod homography;
mod i18n;
mod icon;
mod keyboard;
mod midi_live;
mod notes;
mod persist;
mod resize_fix;
mod settings;
mod shared;
mod ui;
mod webcam;

use bevy::diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};
use bevy_egui::EguiPlugin;

/// When started from a terminal, print there (for `--help` and `--bench`) despite the GUI subsystem.
fn attach_parent_console() {
    #[cfg(windows)]
    // SAFETY: plain Win32 call without pointers; failing (no parent console) is harmless.
    unsafe {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Starts maximized; 1600x900 is the size the window goes back to when restored.
fn maximize_window(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.set_maximized(true);
}

fn main() {
    attach_parent_console();
    let cli = settings::CliArgs::parse();
    let mut app = App::new();
    app.add_plugins(
        // The graphics backend is wgpu's choice (Vulkan first on Windows and Linux); the
        // WGPU_BACKEND environment variable overrides it, e.g. WGPU_BACKEND=dx12.
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "PiViz".into(),
                resolution: (1600, 900).into(),
                // Without vsync in bench mode, so the real frame cost shows.
                present_mode: if cli.bench {
                    PresentMode::AutoNoVsync
                } else {
                    PresentMode::AutoVsync
                },
                ..default()
            }),
            ..default()
        }),
    );
    if cli.bench {
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            LogDiagnosticsPlugin::default(),
        ));
    }
    // Bindless egui textures made glyphs differ from frame to frame (shimmering text) with DX12.
    app.add_plugins(EguiPlugin {
        bindless_mode_array_size: None,
    })
    .insert_resource(cli)
    .add_systems(Startup, maximize_window)
    .add_plugins((
        shared::SharedPlugin,
        compose::ComposePlugin,
        settings::SettingsPlugin,
        audio::AudioPlugin,
        midi_live::MidiLivePlugin,
        keyboard::KeyboardPlugin,
        notes::NotesPlugin,
        draw::DrawPlugin,
        effects::EffectsPlugin,
        ui::UiPlugin,
        persist::PersistPlugin,
        resize_fix::ResizeFixPlugin,
        webcam::WebcamPlugin,
        icon::IconPlugin,
        frame_limit::FrameLimitPlugin,
    ))
    // A tuple of plugins holds at most 15.
    .add_plugins(api::ApiPlugin)
    .run();
}
