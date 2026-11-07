//! Settings panel (egui) and window-level shortcuts. All text comes from `i18n`.

use crate::api::{Api, ApiStatus};
use crate::audio::{AudioHandle, SoundFontState, SoundFontStatus};
use crate::colors::{
    ColorScheme, MAX_GRADIENT_STOPS, NoteColoring, Rgb, builtin_presets, sample_gradient,
};
use crate::homography;
use crate::i18n::{Language, Tr};
use crate::keyboard::Layout;
use crate::midi_live::{LiveCmd, MidiLive};
use crate::persist::{Library, Session};
use crate::settings::{Background, Settings};
use crate::shared::LoadSoundFont;
use crate::webcam::{self, Calibration, CameraMode, Corners, Webcam};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};
use bevy_egui::input::egui_wants_any_keyboard_input;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use std::path::PathBuf;

/// The settings panel and keyboard shortcuts.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(EguiPrimaryContextPass, panel).add_systems(
            Update,
            (
                function_keys,
                letter_keys.run_if(not(egui_wants_any_keyboard_input)),
            ),
        );
    }
}

/// Function keys always work, even while a panel widget has keyboard focus.
fn function_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<Settings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
) {
    if keys.just_pressed(KeyCode::F1) {
        settings.show_panel = !settings.show_panel;
    }
    if keys.just_pressed(KeyCode::F11) {
        window.mode = match window.mode {
            WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
            _ => WindowMode::Windowed,
        };
    }
}

/// Letter shortcuts are ignored while typing in a text field.
fn letter_keys(
    keys: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<Settings>,
    mut calibration: ResMut<Calibration>,
) {
    if keys.just_pressed(KeyCode::KeyH) {
        settings.show_panel = !settings.show_panel;
    }
    if keys.any_just_pressed([KeyCode::Enter, KeyCode::NumpadEnter, KeyCode::Escape]) {
        let mut finished = calibration.clone();
        finished.finish();
        calibration.set_if_neq(finished);
    }
}

#[derive(SystemParam)]
/// Resources the panel reads and edits.
struct PanelState<'w> {
    /// Edited through a copy, written back if changed.
    settings: ResMut<'w, Settings>,
    /// Profiles and color presets.
    library: ResMut<'w, Library>,
    /// MIDI ports and connection status.
    live: Res<'w, MidiLive>,
    /// SoundFont path and loading state.
    soundfont: Res<'w, SoundFontStatus>,
    /// Whether an audio output exists.
    audio: Res<'w, AudioHandle>,
    /// For the fps counter.
    time: Res<'w, Time>,
    /// Camera choice and points, edited through a copy.
    session: ResMut<'w, Session>,
    /// Placement of the camera points, edited through a copy.
    calibration: ResMut<'w, Calibration>,
    /// Camera devices, formats and status; opens the driver's settings window.
    webcam: ResMut<'w, Webcam>,
    /// Screen geometry, for the camera grid.
    layout: Res<'w, Layout>,
    /// Remote control status.
    api: Res<'w, Api>,
}

#[derive(SystemParam)]
/// Messages the panel sends.
struct PanelOutputs<'w> {
    /// Load a SoundFont.
    load_soundfont: MessageWriter<'w, LoadSoundFont>,
    /// Connect, disconnect, refresh MIDI ports.
    live: MessageWriter<'w, LiveCmd>,
}

/// Fixed width of the panel, so it does not move when a text changes.
const PANEL_WIDTH: f32 = 330.0;
/// Choices for the frame rate cap; 0 means the display's refresh rate.
const FPS_LIMITS: [u32; 7] = [0, 30, 60, 90, 120, 144, 240];

/// Text fields and selections that live only in the panel.
#[derive(Default)]
struct PanelLocal {
    /// SoundFont path being typed.
    soundfont_path: String,
    /// MIDI port selected in the list (id).
    port: Option<String>,
    /// Frame rate shown in the footer.
    fps: FpsCounter,
    /// Profile name being typed.
    profile_name: String,
    /// The background to restore when the camera is turned off.
    background_before_camera: Background,
    /// The camera whose driver settings window was asked for this frame (Windows only).
    #[cfg_attr(not(windows), allow(dead_code))]
    open_camera_settings: Option<String>,
    /// Profile selected in the list.
    selected_profile: Option<String>,
    /// Preset name being typed.
    preset_name: String,
    /// The built-in color presets.
    builtins: Vec<(&'static str, ColorScheme)>,
}

/// Changes to the saved library, applied after the panel is drawn.
enum LibraryEdit {
    /// Save the current settings under this name.
    SaveProfile(String),
    /// Delete this profile.
    DeleteProfile(String),
    /// Save the current colors as a preset under this name.
    SavePreset(String),
    /// Delete this color preset.
    DeletePreset(String),
}

/// Frame rate refreshed twice a second, so the text stays readable and steady.
#[derive(Default)]
struct FpsCounter {
    /// Time of the last frame counted.
    last_time: f64,
    /// Frames counted since the last refresh.
    frames: u32,
    /// Seconds since the last refresh.
    elapsed: f64,
    /// The rate displayed.
    shown: u32,
}

impl FpsCounter {
    /// Counts a frame at time `now` (once per frame), refreshing the shown rate every half second.
    fn tick(&mut self, now: f64) {
        // The panel can run twice in one frame (egui layout passes): count each frame once.
        if now == self.last_time {
            return;
        }
        self.elapsed += now - self.last_time;
        self.last_time = now;
        self.frames += 1;
        if self.elapsed >= 0.5 {
            self.shown = (self.frames as f64 / self.elapsed).round() as u32;
            self.frames = 0;
            self.elapsed = 0.0;
        }
    }
}

/// A collapsible section whose open/closed state survives a language change.
fn section(ui: &mut egui::Ui, id: &str, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(title).id_salt(id).show(ui, add);
}

/// Draws the calibration overlay and the settings panel, then writes the edited resources back only
/// if they changed.
fn panel(
    mut contexts: EguiContexts,
    mut state: PanelState,
    mut out: PanelOutputs,
    mut local: Local<PanelLocal>,
) -> Result {
    local.fps.tick(state.time.elapsed_secs_f64());
    let ctx = contexts.ctx_mut()?;
    // Edit copies so the resources only report a change when a value really changed.
    let mut session = state.session.clone();
    let mut calibration = state.calibration.clone();
    if state.settings.background != Background::Camera {
        calibration.finish();
    }
    if calibration.active {
        let t = Tr::new(state.settings.language);
        calibration_overlay(ctx, t, &state, &mut session.camera_points, &mut calibration);
    }
    if !state.settings.show_panel {
        state.session.set_if_neq(session);
        state.calibration.set_if_neq(calibration);
        return Ok(());
    }
    if local.soundfont_path.is_empty() && !state.soundfont.path.is_empty() {
        local.soundfont_path = state.soundfont.path.clone();
    }
    if local.builtins.is_empty() {
        local.builtins = builtin_presets();
    }
    let mut s = state.settings.clone();
    if s.background != Background::Camera {
        local.background_before_camera = s.background;
    }
    let t = Tr::new(s.language);
    let mut edits = Vec::new();
    let max_height = ctx.content_rect().height() - 60.0;

    // Fixed width: otherwise the window follows its content and shifts whenever a text changes.
    egui::Window::new("PiViz")
        .default_pos([16.0, 16.0])
        .min_width(PANEL_WIDTH)
        .max_width(PANEL_WIDTH)
        .resizable(false)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(t.tr("panel.hide")).clicked() {
                            s.show_panel = false;
                        }
                    });
                    egui::ComboBox::from_label(t.tr("panel.language"))
                        .selected_text(language_name(s.language, t))
                        .show_ui(ui, |ui| {
                            let choices =
                                std::iter::once(Language::Auto).chain(Language::AVAILABLE);
                            for lang in choices {
                                ui.selectable_value(&mut s.language, lang, language_name(lang, t));
                            }
                        });
                    ui.separator();
                    live_section(ui, t, &state, &mut out, &mut local);
                    ui.separator();

                    section(ui, "profiles", t.tr("profiles.title"), |ui| {
                        profiles_section(ui, t, &state, &mut s, &mut local, &mut edits);
                    });

                    section(ui, "display", t.tr("display.title"), |ui| {
                        // Over the camera, the real keyboard is shown instead.
                        ui.add_enabled(
                            s.background != Background::Camera,
                            egui::Checkbox::new(
                                &mut s.show_keyboard,
                                t.tr("display.show_keyboard"),
                            ),
                        );
                        ui.horizontal_wrapped(|ui| {
                            ui.label(t.tr("display.background"));
                            ui.radio_value(
                                &mut s.background,
                                Background::Theme,
                                t.tr("display.bg_color"),
                            );
                            ui.radio_value(
                                &mut s.background,
                                Background::Black,
                                t.tr("display.bg_black"),
                            );
                            ui.radio_value(
                                &mut s.background,
                                Background::Transparent,
                                t.tr("display.bg_transparent"),
                            )
                            .on_hover_text(t.tr("display.bg_transparent_hint"));
                            ui.radio_value(
                                &mut s.background,
                                Background::Camera,
                                t.tr("display.bg_camera"),
                            )
                            .on_hover_text(t.tr("display.bg_camera_hint"));
                        });
                        ui.add(
                            egui::Slider::new(&mut s.fall_speed, 80.0..=1500.0)
                                .text(t.tr("display.rise_speed")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.bloom, 0.0..=1.0).text(t.tr("display.bloom")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.glow, 0.5..=4.0)
                                .text(t.tr("display.note_glow")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.sparks, 0.0..=3.0)
                                .text(t.tr("display.sparks")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.relief, 0.0..=2.0)
                                .text(t.tr("display.relief")),
                        );
                        ui.add(
                            egui::Slider::new(&mut s.halo, 0.0..=3.0).text(t.tr("display.halo")),
                        );
                        egui::ComboBox::from_label(t.tr("display.fps_limit"))
                            .selected_text(fps_limit_name(s.fps_limit, t))
                            .show_ui(ui, |ui| {
                                for limit in FPS_LIMITS {
                                    ui.selectable_value(
                                        &mut s.fps_limit,
                                        limit,
                                        fps_limit_name(limit, t),
                                    );
                                }
                            });
                    });

                    section(ui, "colors", t.tr("colors.title"), |ui| {
                        colors_section(ui, t, &state, &mut s.colors, &mut local, &mut edits);
                    });

                    section(ui, "camera", t.tr("camera.title"), |ui| {
                        camera_section(
                            ui,
                            t,
                            &state,
                            &mut s,
                            &mut local,
                            &mut session,
                            &mut calibration,
                        );
                    });

                    section(ui, "audio", t.tr("audio.title"), |ui| {
                        if !state.audio.available {
                            ui.colored_label(egui::Color32::LIGHT_RED, t.tr("audio.no_output"));
                        }
                        ui.add(
                            egui::Slider::new(&mut s.volume, 0.0..=1.5).text(t.tr("audio.volume")),
                        );
                        ui.label(t.tr("audio.soundfont"));
                        ui.horizontal(|ui| {
                            ui.text_edit_singleline(&mut local.soundfont_path);
                            let loading = matches!(state.soundfont.state, SoundFontState::Loading);
                            if ui
                                .add_enabled(!loading, egui::Button::new(t.tr("common.load")))
                                .clicked()
                            {
                                out.load_soundfont.write(LoadSoundFont(PathBuf::from(
                                    local.soundfont_path.trim(),
                                )));
                            }
                        });
                        let message = t.soundfont_status(&state.soundfont.state);
                        if !message.is_empty() {
                            ui.small(message);
                        }
                    });

                    section(ui, "api", t.tr("api.title"), |ui| {
                        api_section(ui, t, &state.api.status, &mut session);
                    });

                    ui.separator();
                    ui.small(t.trf("panel.footer", &[("fps", &local.fps.shown.to_string())]));
                });
        });

    for edit in edits {
        match edit {
            LibraryEdit::SaveProfile(name) => {
                state.library.profiles.insert(name, s.clone());
            }
            LibraryEdit::DeleteProfile(name) => {
                state.library.profiles.remove(&name);
            }
            LibraryEdit::SavePreset(name) => {
                state.library.color_presets.insert(name, s.colors.clone());
            }
            LibraryEdit::DeletePreset(name) => {
                state.library.color_presets.remove(&name);
            }
        }
    }
    #[cfg(windows)]
    if let Some(name) = local.open_camera_settings.take() {
        state.webcam.open_properties(&name);
    }
    if s.background != Background::Camera {
        calibration.finish();
    }
    state.settings.set_if_neq(s);
    state.session.set_if_neq(session);
    state.calibration.set_if_neq(calibration);
    Ok(())
}

/// "1920×1080 · 60 fps · MJPEG".
fn mode_name(mode: &CameraMode, t: Tr) -> String {
    t.trf(
        "camera.mode",
        &[
            ("width", &mode.width.to_string()),
            ("height", &mode.height.to_string()),
            ("fps", &mode.fps.to_string()),
            ("format", &mode.format),
        ],
    )
}

/// Name of a frame rate limit in the menu.
fn fps_limit_name(limit: u32, t: Tr) -> String {
    match limit {
        0 => t.tr("display.fps_display").to_owned(),
        n => t.trf("display.fps_value", &[("fps", &n.to_string())]),
    }
}

/// "Auto" in the current language; other languages by their own name.
fn language_name(lang: Language, t: Tr) -> &'static str {
    match lang {
        Language::Auto => t.tr("panel.language_auto"),
        other => Tr::language_name(other),
    }
}

/// MIDI input: port list, refresh / connect / disconnect, status.
fn live_section(
    ui: &mut egui::Ui,
    t: Tr,
    state: &PanelState,
    out: &mut PanelOutputs,
    local: &mut PanelLocal,
) {
    let selected_name = local
        .port
        .as_ref()
        .and_then(|id| state.live.ports.iter().find(|p| &p.id == id))
        .map_or(t.tr("midi.choose_port"), |p| p.name.as_str());
    egui::ComboBox::from_label(t.tr("midi.input"))
        .selected_text(selected_name)
        .show_ui(ui, |ui| {
            for port in &state.live.ports {
                ui.selectable_value(&mut local.port, Some(port.id.clone()), &port.name);
            }
        });
    ui.horizontal(|ui| {
        if ui.button(t.tr("midi.refresh")).clicked() {
            out.live.write(LiveCmd::Refresh);
        }
        if let Some(id) = &local.port
            && ui.button(t.tr("midi.connect")).clicked()
        {
            out.live.write(LiveCmd::Connect(id.clone()));
        }
        if state.live.connected.is_some() && ui.button(t.tr("midi.disconnect")).clicked() {
            out.live.write(LiveCmd::Disconnect);
        }
    });
    if state.live.ports.is_empty() {
        ui.small(t.tr("midi.no_ports"));
    }
    let status = t.midi_status(&state.live.status);
    if !status.is_empty() {
        ui.small(status);
    }
}

/// Device choice, on/off, brightness, the driver's settings window (Windows) and the button that
/// starts placing the reference points.
fn camera_section(
    ui: &mut egui::Ui,
    t: Tr,
    state: &PanelState,
    s: &mut Settings,
    local: &mut PanelLocal,
    session: &mut Session,
    calibration: &mut Calibration,
) {
    let camera_mode = s.background == Background::Camera;
    egui::ComboBox::from_label(t.tr("camera.device"))
        .selected_text(session.camera.as_deref().unwrap_or(t.tr("common.choose")))
        .show_ui(ui, |ui| {
            for name in &state.webcam.devices {
                // Another device has other formats: back to the automatic choice.
                if ui
                    .selectable_value(&mut session.camera, Some(name.clone()), name)
                    .changed()
                {
                    session.camera_mode = None;
                }
            }
        });
    if camera_mode && !state.webcam.modes.is_empty() {
        let auto = match &state.webcam.mode {
            Some(mode) => t.trf("camera.format_auto_using", &[("mode", &mode_name(mode, t))]),
            None => t.tr("camera.format_auto").to_owned(),
        };
        let selected = session
            .camera_mode
            .as_ref()
            .map_or(auto.clone(), |m| mode_name(m, t));
        egui::ComboBox::from_label(t.tr("camera.format"))
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut session.camera_mode, None, auto);
                for mode in &state.webcam.modes {
                    ui.selectable_value(
                        &mut session.camera_mode,
                        Some(mode.clone()),
                        mode_name(mode, t),
                    );
                }
            });
    }
    ui.horizontal(|ui| {
        if ui.button(t.tr("camera.refresh")).clicked() {
            state.webcam.refresh();
        }
        if !camera_mode && ui.button(t.tr("camera.turn_on")).clicked() {
            s.background = Background::Camera;
        }
        if camera_mode && ui.button(t.tr("camera.turn_off")).clicked() {
            s.background = local.background_before_camera;
        }
    });
    if state.webcam.devices.is_empty() {
        ui.small(t.tr("camera.no_devices"));
    } else if camera_mode {
        let status = t.camera_status(&state.webcam.status);
        if !status.is_empty() {
            ui.small(status);
        }
        if let Some(rate) = state.webcam.rate {
            ui.small(t.trf(
                "camera.rate",
                &[
                    ("fps", &format!("{:.0}", rate.fps)),
                    ("ms", &format!("{:.1}", rate.decode_ms)),
                ],
            ))
            .on_hover_text(t.tr("camera.rate_hint"));
        }
    }
    ui.add_enabled(
        camera_mode,
        egui::Slider::new(&mut s.camera_brightness, 0.1..=1.0).text(t.tr("camera.brightness")),
    );
    #[cfg(windows)]
    {
        let device = session.camera.clone();
        let enabled = device.is_some() && !state.webcam.properties_open;
        if ui
            .add_enabled(enabled, egui::Button::new(t.tr("camera.properties")))
            .on_hover_text(t.tr("camera.properties_hint"))
            .clicked()
            && let Some(name) = device
        {
            local.open_camera_settings = Some(name);
        }
        if let Some(error) = &state.webcam.properties_error {
            ui.small(t.camera_properties_error(error));
        }
    }
    let label = if calibration.active {
        t.tr("camera.calibrate_done")
    } else {
        t.tr("camera.calibrate")
    };
    if ui
        .add_enabled(
            camera_mode,
            egui::Button::new(label).selected(calibration.active),
        )
        .clicked()
    {
        calibration.active = !calibration.active;
        calibration.placing = None;
    }
}

/// Handles on the reference points over the camera image: click the four points in order, or
/// drag them. A grid shows the whole scene through the projection.
fn calibration_overlay(
    ctx: &egui::Context,
    t: Tr,
    state: &PanelState,
    corners: &mut Corners,
    calibration: &mut Calibration,
) {
    let screen = ctx.content_rect();
    let window = Vec2::new(screen.width(), screen.height());
    let fit = webcam::fit_rect(window, state.webcam.size_or(window));
    let origin = Vec2::new(screen.min.x, screen.min.y) + fit.min;
    let to_screen = |p: Vec2| {
        let q = origin + p * fit.size();
        egui::pos2(q.x, q.y)
    };
    let to_camera = |p: egui::Pos2| {
        ((Vec2::new(p.x, p.y) - origin) / fit.size())
            .clamp(Vec2::ZERO, Vec2::ONE)
            .to_array()
    };
    let accent = egui::Color32::from_rgb(255, 196, 64);

    egui::Area::new(egui::Id::new("calibration"))
        .order(egui::Order::Background)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let background = ui.allocate_rect(screen, egui::Sense::click());
            if let Some(n) = calibration.placing
                && background.clicked()
                && let Some(pos) = background.interact_pointer_pos()
            {
                corners[n] = to_camera(pos);
                calibration.placing = (n < 3).then_some(n + 1);
            }
            let placed = calibration.placing.unwrap_or(4);
            let points: Vec<egui::Pos2> = corners[..placed]
                .iter()
                .map(|c| to_screen(Vec2::from(*c)))
                .collect();

            if placed == 4
                && let Some(h) = webcam::scene_to_camera(&state.layout, corners)
            {
                scene_grid(ui.painter(), &h, &state.layout, &to_screen, accent);
            }
            let edge = egui::Stroke::new(2.0, accent);
            for pair in points.windows(2) {
                ui.painter().line_segment([pair[0], pair[1]], edge);
            }
            if placed == 4 {
                ui.painter().line_segment([points[3], points[0]], edge);
            }

            for (i, center) in points.iter().enumerate() {
                let rect = egui::Rect::from_center_size(*center, egui::vec2(30.0, 30.0));
                let response = ui.interact(rect, ui.id().with(("corner", i)), egui::Sense::drag());
                if response.dragged() {
                    corners[i] = to_camera(*center + response.drag_delta());
                }
                let radius = if response.hovered() || response.dragged() {
                    10.0
                } else {
                    7.0
                };
                let painter = ui.painter();
                painter.circle(
                    *center,
                    radius,
                    accent.gamma_multiply(0.35),
                    egui::Stroke::new(2.0, accent),
                );
                painter.text(
                    *center + egui::vec2(12.0, -12.0),
                    egui::Align2::LEFT_BOTTOM,
                    (i + 1).to_string(),
                    egui::FontId::proportional(16.0),
                    egui::Color32::WHITE,
                );
            }
        });

    egui::Area::new(egui::Id::new("calibration_hint"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 16.0])
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(420.0);
                match calibration.placing {
                    Some(n) => {
                        let corner = match n {
                            0 => t.tr("camera.corner.top_left"),
                            1 => t.tr("camera.corner.top_right"),
                            2 => t.tr("camera.corner.bottom_right"),
                            _ => t.tr("camera.corner.bottom_left"),
                        };
                        ui.label(t.trf("camera.place_corner", &[("corner", corner)]));
                    }
                    None => {
                        ui.label(t.tr("camera.calibrate_hint"));
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button(t.tr("camera.restart")).clicked() {
                        calibration.placing = Some(0);
                    }
                    if ui.button(t.tr("camera.calibrate_done")).clicked() {
                        calibration.finish();
                    }
                });
            });
        });
}

/// The whole scene as seen through the projection: one column per white key (octaves, starting
/// at each C, drawn stronger) and a row every tenth of the height, the flame line highlighted.
fn scene_grid(
    painter: &egui::Painter,
    h: &Mat3,
    layout: &Layout,
    to_screen: &impl Fn(Vec2) -> egui::Pos2,
    accent: egui::Color32,
) {
    const WHITE_KEYS: usize = 52;
    let [_, _, br, bl] = webcam::reference_quad(layout);
    let (left, width) = (bl.x, br.x - bl.x);
    let line = |a: Vec2, b: Vec2, stroke: egui::Stroke| {
        if let Some((a, b)) = homography::clip_segment(h, a, b) {
            painter.line_segment([to_screen(a), to_screen(b)], stroke);
        }
    };
    let faint = egui::Stroke::new(1.0, accent.gamma_multiply(0.3));
    let strong = egui::Stroke::new(1.5, accent.gamma_multiply(0.75));
    for i in 0..=WHITE_KEYS {
        let x = left + width * i as f32 / WHITE_KEYS as f32;
        // The white keys run A0, B0, C1...: a C starts every 7 keys from the third.
        let stroke = if i % 7 == 2 || i == 0 || i == WHITE_KEYS {
            strong
        } else {
            faint
        };
        line(Vec2::new(x, 0.0), Vec2::new(x, 1.0), stroke);
    }
    for i in 0..=10 {
        let y = i as f32 / 10.0;
        line(Vec2::new(left, y), Vec2::new(left + width, y), faint);
    }
    line(
        Vec2::new(left, br.y),
        Vec2::new(left + width, br.y),
        egui::Stroke::new(2.5, accent),
    );
}

/// The WebSocket remote control: on/off, port, and the address to give to clients.
fn api_section(ui: &mut egui::Ui, t: Tr, status: &ApiStatus, session: &mut Session) {
    ui.checkbox(&mut session.api_enabled, t.tr("api.enabled"));
    ui.horizontal(|ui| {
        ui.label(t.tr("api.port"));
        // Typed, not dragged: every new port restarts the server.
        ui.add_enabled(
            session.api_enabled,
            egui::DragValue::new(&mut session.api_port)
                .range(1024..=65535)
                .speed(0.0),
        );
    });
    match status {
        ApiStatus::Off => {}
        ApiStatus::Listening(port) => {
            ui.horizontal(|ui| {
                ui.small(t.tr("api.address"));
                ui.add(egui::Label::new(
                    egui::RichText::new(format!("ws://127.0.0.1:{port}")).monospace(),
                ));
            });
        }
        ApiStatus::Failed(port, error) => {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                t.trf(
                    "api.failed",
                    &[("port", &port.to_string()), ("error", error)],
                ),
            );
        }
    }
    ui.small(t.tr("api.hint"));
}

/// Save, load and delete named snapshots of every setting.
fn profiles_section(
    ui: &mut egui::Ui,
    t: Tr,
    state: &PanelState,
    s: &mut Settings,
    local: &mut PanelLocal,
    edits: &mut Vec<LibraryEdit>,
) {
    let profiles = &state.library.profiles;
    if local
        .selected_profile
        .as_ref()
        .is_some_and(|name| !profiles.contains_key(name))
    {
        local.selected_profile = None;
    }
    egui::ComboBox::from_label(t.tr("profiles.profile"))
        .selected_text(
            local
                .selected_profile
                .as_deref()
                .unwrap_or(t.tr("common.choose")),
        )
        .show_ui(ui, |ui| {
            for name in profiles.keys() {
                ui.selectable_value(&mut local.selected_profile, Some(name.clone()), name);
            }
        });
    ui.horizontal(|ui| {
        let selected = local
            .selected_profile
            .as_ref()
            .and_then(|name| profiles.get(name).map(|p| (name, p)));
        if ui
            .add_enabled(selected.is_some(), egui::Button::new(t.tr("common.load")))
            .clicked()
            && let Some((_, profile)) = selected
        {
            *s = profile.clone();
            s.show_panel = true;
            s.sanitize();
        }
        if ui
            .add_enabled(selected.is_some(), egui::Button::new(t.tr("common.delete")))
            .clicked()
            && let Some((name, _)) = selected
        {
            edits.push(LibraryEdit::DeleteProfile(name.clone()));
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut local.profile_name)
                .hint_text(t.tr("profiles.name"))
                .desired_width(170.0),
        );
        let name = local.profile_name.trim().to_owned();
        let label = if profiles.contains_key(&name) {
            t.tr("common.replace")
        } else {
            t.tr("common.save")
        };
        if ui
            .add_enabled(!name.is_empty(), egui::Button::new(label))
            .clicked()
        {
            local.selected_profile = Some(name.clone());
            edits.push(LibraryEdit::SaveProfile(name));
        }
    });
    ui.small(t.tr("profiles.hint"));
}

/// Note and glow coloring (each solid or gradient), background, with presets.
fn colors_section(
    ui: &mut egui::Ui,
    t: Tr,
    state: &PanelState,
    colors: &mut ColorScheme,
    local: &mut PanelLocal,
    edits: &mut Vec<LibraryEdit>,
) {
    let user_presets = &state.library.color_presets;
    egui::ComboBox::from_label(t.tr("colors.preset"))
        .selected_text(t.tr("colors.apply"))
        .show_ui(ui, |ui| {
            for (name, scheme) in &local.builtins {
                if ui.selectable_label(false, *name).clicked() {
                    *colors = scheme.clone();
                }
            }
            if !user_presets.is_empty() {
                ui.separator();
            }
            for (name, scheme) in user_presets {
                if ui.selectable_label(false, name).clicked() {
                    *colors = scheme.clone();
                    local.preset_name = name.clone();
                }
            }
        });

    ui.horizontal(|ui| {
        ui.label(t.tr("colors.notes"));
        ui.radio_value(
            &mut colors.coloring,
            NoteColoring::Solid,
            t.tr("colors.solid"),
        );
        ui.radio_value(
            &mut colors.coloring,
            NoteColoring::Gradient,
            t.tr("colors.gradient"),
        );
    });
    match colors.coloring {
        NoteColoring::Solid => {
            ui.horizontal(|ui| {
                ui.label(t.tr("colors.color"));
                ui.color_edit_button_srgb(&mut colors.solid);
            });
        }
        NoteColoring::Gradient => {
            color_list(ui, &mut colors.gradient, 2, MAX_GRADIENT_STOPS);
            gradient_preview(ui, &colors.gradient);
            ui.small(t.tr("colors.gradient_hint"));
        }
    }
    ui.horizontal(|ui| {
        ui.label(t.tr("colors.accent"));
        ui.radio_value(
            &mut colors.accent_coloring,
            NoteColoring::Solid,
            t.tr("colors.solid"),
        );
        ui.radio_value(
            &mut colors.accent_coloring,
            NoteColoring::Gradient,
            t.tr("colors.gradient"),
        );
    });
    match colors.accent_coloring {
        NoteColoring::Solid => {
            ui.horizontal(|ui| {
                ui.label(t.tr("colors.color"));
                ui.color_edit_button_srgb(&mut colors.accent);
            });
        }
        NoteColoring::Gradient => {
            color_list(ui, &mut colors.accent_gradient, 2, MAX_GRADIENT_STOPS);
            gradient_preview(ui, &colors.accent_gradient);
        }
    }
    ui.horizontal(|ui| {
        ui.label(t.tr("colors.background"));
        ui.color_edit_button_srgb(&mut colors.background);
    });

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut local.preset_name)
                .hint_text(t.tr("colors.preset_name"))
                .desired_width(130.0),
        );
        let name = local.preset_name.trim().to_owned();
        let is_builtin = local.builtins.iter().any(|(builtin, _)| *builtin == name);
        let exists = user_presets.contains_key(&name);
        if ui
            .add_enabled(
                !name.is_empty() && !is_builtin,
                egui::Button::new(if exists {
                    t.tr("common.replace")
                } else {
                    t.tr("common.save")
                }),
            )
            .on_disabled_hover_text(t.tr("colors.preset_name_invalid"))
            .clicked()
        {
            edits.push(LibraryEdit::SavePreset(name.clone()));
        }
        if ui
            .add_enabled(exists, egui::Button::new(t.tr("common.delete")))
            .clicked()
        {
            edits.push(LibraryEdit::DeletePreset(name));
        }
    });
}

/// A row of color buttons with "+" / "−" to change how many there are.
fn color_list(ui: &mut egui::Ui, list: &mut Vec<Rgb>, min: usize, max: usize) {
    ui.horizontal_wrapped(|ui| {
        for color in list.iter_mut() {
            ui.color_edit_button_srgb(color);
        }
        if list.len() < max && ui.small_button("+").clicked() {
            let last = list.last().copied().unwrap_or([255, 255, 255]);
            list.push(last);
        }
        if list.len() > min && ui.small_button("−").clicked() {
            list.pop();
        }
    });
}

/// A strip showing the gradient as the notes will display it.
fn gradient_preview(ui: &mut egui::Ui, stops: &[Rgb]) {
    const SLICES: usize = 48;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(240.0, 12.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let w = rect.width() / SLICES as f32;
    for i in 0..SLICES {
        let c = Srgba::from(sample_gradient(stops, (i as f32 + 0.5) / SLICES as f32)).to_u8_array();
        let slice = egui::Rect::from_min_size(
            rect.min + egui::vec2(i as f32 * w, 0.0),
            egui::vec2(w + 0.5, rect.height()),
        );
        painter.rect_filled(slice, 0.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
    }
}
