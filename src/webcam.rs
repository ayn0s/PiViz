//! Webcam background: the notes are drawn over the real keyboard seen by a camera.
//!
//! A worker thread opens the device and decodes its frames to RGBA; the render world copies the
//! newest one into a texture that lives as long as the frame size does, and `compose` shows it
//! under the scene. The scene is projected with a homography that maps the flame line and a band
//! above it onto four reference points placed by the user.

use crate::homography;
use crate::keyboard::Layout;
use crate::persist::Session;
use crate::settings::{Background, Settings};
use crate::shared::Frame;
use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDimension, TextureFormat,
};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraInfo, FrameFormat, RequestedFormat, RequestedFormatType,
    Resolution,
};
use nokhwa::{Buffer, Camera, FormatDecoder};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Reference points in the camera image (0..1, origin top-left), in `reference_quad` order:
/// 1 and 2 above the flame line, 3 and 4 at its right and left ends (the far edge of the keyboard).
pub type Corners = [[f32; 2]; 4];

/// Where the points start before any placement.
pub const DEFAULT_CORNERS: Corners = [[0.13, 0.42], [0.87, 0.42], [0.9, 0.62], [0.1, 0.62]];

/// Height of points 1 and 2 above the flame line, as a fraction of the rising area.
const REFERENCE_HEIGHT: f32 = 0.25;

#[derive(Clone, Debug, PartialEq)]
/// State of the camera, shown in the panel.
pub enum WebcamStatus {
    /// Not running (not the camera background).
    Off,
    /// Being opened.
    Opening(String),
    /// Streaming.
    Running {
        /// Device name.
        name: String,
        /// Frame width, in pixels.
        width: u32,
        /// Frame height, in pixels.
        height: u32,
        /// Requested frame rate (the camera may deliver fewer, see [`CameraRate`]).
        fps: u32,
    },
    /// No camera found.
    NoDevice,
    /// No camera has this name.
    NotFound(String),
    /// The camera could not be opened.
    Failed(String),
    /// The stream stopped delivering frames (device unplugged or taken by another app).
    Lost,
}

/// A capture format the camera offers: resolution, frame rate and pixel format. Saved in the
/// session when the user picks one instead of the automatic choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraMode {
    /// Frame width, in pixels.
    pub width: u32,
    /// Frame height, in pixels.
    pub height: u32,
    /// Frames per second.
    pub fps: u32,
    /// Pixel format as nokhwa names it: "MJPEG", "YUYV", "NV12", …
    pub format: String,
}

/// The pixel formats PiViz can decode.
const FRAME_FORMATS: [FrameFormat; 6] = [
    FrameFormat::MJPEG,
    FrameFormat::YUYV,
    FrameFormat::NV12,
    FrameFormat::GRAY,
    FrameFormat::RAWRGB,
    FrameFormat::RAWBGR,
];

impl CameraMode {
    /// The mode of a nokhwa format.
    fn from_format(f: &CameraFormat) -> Self {
        Self {
            width: f.resolution().width(),
            height: f.resolution().height(),
            fps: f.frame_rate(),
            format: f.format().to_string(),
        }
    }

    /// The nokhwa format of this mode; `None` for an unknown pixel format.
    fn to_format(&self) -> Option<CameraFormat> {
        let format = FRAME_FORMATS
            .into_iter()
            .find(|f| f.to_string() == self.format)?;
        Some(CameraFormat::new(
            Resolution::new(self.width, self.height),
            format,
            self.fps,
        ))
    }
}

/// What the camera really delivers, measured by the capture thread (the format's frame rate is
/// only what was asked for: many cameras slow down in dim light).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraRate {
    /// Frames received per second.
    pub fps: f32,
    /// Average time to decode one frame, in milliseconds.
    pub decode_ms: f32,
}

/// Counts frames and decode time, and yields a `CameraRate` once a second.
struct RateMeter {
    /// Start of the current measure.
    since: Instant,
    /// Frames since then.
    frames: u32,
    /// Total decode time since then.
    decoding: Duration,
}

impl RateMeter {
    /// A meter starting now.
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            decoding: Duration::ZERO,
        }
    }

    /// Counts one frame; returns the rate once a second has passed, and starts over.
    fn frame(&mut self, decoding: Duration) -> Option<CameraRate> {
        self.frames += 1;
        self.decoding += decoding;
        let elapsed = self.since.elapsed();
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let rate = CameraRate {
            fps: self.frames as f32 / elapsed.as_secs_f32(),
            decode_ms: self.decoding.as_secs_f32() * 1000.0 / self.frames as f32,
        };
        *self = Self::new();
        Some(rate)
    }
}

/// Why the camera driver's settings window could not be shown.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum PropertiesError {
    /// No video device with this name was found.
    NotFound,
    /// The driver offers no settings window.
    NoSettings,
    /// Any other error, with the system's message.
    Failed(String),
}

#[derive(Resource)]
/// The camera background: devices, status, texture, formats.
pub struct Webcam {
    /// Names of the cameras found by the last scan.
    pub devices: Vec<String>,
    /// For the panel.
    pub status: WebcamStatus,
    /// Size of the last frame received, in pixels.
    pub size: Option<UVec2>,
    /// Frames really received per second and decode time, while running.
    pub rate: Option<CameraRate>,
    /// Formats the open camera offers, best first, and the one in use.
    pub modes: Vec<CameraMode>,
    /// The format in use.
    pub mode: Option<CameraMode>,
    /// The camera texture (1x1 black until the first frame arrives). Frames are written into it
    /// in place; it is replaced only when the frame size changes.
    pub image: Handle<Image>,
    /// Counts texture replacements, so the composition knows when to rebind it.
    pub generation: u64,
    /// The driver's settings window is open.
    #[cfg_attr(not(windows), allow(dead_code))] // Only the Windows settings window uses it.
    pub properties_open: bool,
    /// Why the settings window could not be shown, last time.
    #[cfg_attr(not(windows), allow(dead_code))] // Only the Windows settings window uses it.
    pub properties_error: Option<PropertiesError>,
    /// The device and format the worker was last asked to open.
    opened: Option<(String, Option<CameraMode>)>,
    /// Commands to the capture thread.
    cmds: Sender<Cmd>,
    /// Reports from the capture thread (and the settings window thread).
    events: Receiver<Event>,
    #[cfg_attr(not(windows), allow(dead_code))] // Only the Windows settings window uses it.
    /// For the settings window thread to report when it closes.
    event_tx: Sender<Event>,
}

/// The newest decoded frame, shared by the worker thread and the render world.
#[derive(Resource, Clone)]
struct LatestFrame(Arc<Mutex<Option<RgbaFrame>>>);

/// The render world's view of the camera texture.
#[derive(Resource)]
struct CameraTexture(AssetId<Image>);

impl Webcam {
    /// Asks the capture thread to list the cameras again.
    pub fn refresh(&self) {
        let _ = self.cmds.send(Cmd::Refresh);
    }

    /// Opens the driver's settings window for this camera (Windows only), on its own thread
    /// since the window is modal.
    #[cfg(windows)]
    pub fn open_properties(&mut self, name: &str) {
        if self.properties_open {
            return;
        }
        self.properties_open = true;
        self.properties_error = None;
        let (name, events) = (name.to_owned(), self.event_tx.clone());
        let spawned = std::thread::Builder::new()
            .name("camera-settings".into())
            .spawn(move || {
                let result = crate::camera_properties::show(&name);
                let _ = events.send(Event::PropertiesClosed(result.err()));
            });
        if let Err(e) = spawned {
            self.properties_open = false;
            self.properties_error = Some(PropertiesError::Failed(e.to_string()));
        }
    }

    /// The camera's aspect, or the window's while no frame has arrived.
    pub fn size_or(&self, fallback: Vec2) -> Vec2 {
        self.size.map_or(fallback, |s| s.as_vec2())
    }
}

/// Corner placement in progress (camera background only).
#[derive(Resource, Default, Clone, PartialEq)]
pub struct Calibration {
    /// The overlay is shown.
    pub active: bool,
    /// While placing the corners one click at a time: how many are placed so far.
    pub placing: Option<usize>,
}

impl Calibration {
    /// Ends placement.
    pub fn finish(&mut self) {
        self.active = false;
        self.placing = None;
    }
}

/// Commands to the capture thread.
enum Cmd {
    /// List the cameras.
    Refresh,
    /// A device, with the format to use (`None`: the best one).
    Open(String, Option<CameraMode>),
    /// Close the camera.
    Close,
}

/// Reports to Bevy.
enum Event {
    /// Camera names found.
    Devices(Vec<String>),
    /// New status.
    Status(WebcamStatus),
    /// The frames now have this size.
    Size(UVec2),
    /// Measured over the last second.
    Rate(CameraRate),
    /// The formats the camera offers, and the one it is using.
    Modes(Vec<CameraMode>, Option<CameraMode>),
    /// The driver's settings window closed, or could not open.
    #[cfg_attr(not(windows), allow(dead_code))]
    PropertiesClosed(Option<PropertiesError>),
}

/// A decoded frame.
struct RgbaFrame {
    /// Width, in pixels.
    width: u32,
    /// Height, in pixels.
    height: u32,
    /// RGBA, 4 bytes per pixel.
    data: Vec<u8>,
}

/// The camera background: capture thread, texture upload, projection.
pub struct WebcamPlugin;

impl Plugin for WebcamPlugin {
    fn build(&self, app: &mut App) {
        let (cmds, cmd_rx) = crossbeam_channel::unbounded();
        let (event_tx, events) = crossbeam_channel::unbounded();
        let latest = LatestFrame(Arc::new(Mutex::new(None)));
        let worker_latest = latest.clone();
        std::thread::Builder::new()
            .name("webcam".into())
            .spawn({
                let event_tx = event_tx.clone();
                move || worker(&cmd_rx, &event_tx, &worker_latest.0)
            })
            .expect("cannot start the webcam thread");
        let _ = cmds.send(Cmd::Refresh);

        let image = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(rgba_image(1, 1, Some(vec![0, 0, 0, 255])));
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(latest)
                .insert_resource(CameraTexture(image.id()))
                .add_systems(
                    Render,
                    write_frame
                        .in_set(RenderSystems::PrepareResources)
                        .run_if(resource_exists::<RenderQueue>),
                );
        }
        app.insert_resource(Webcam {
            devices: Vec::new(),
            status: WebcamStatus::Off,
            size: None,
            rate: None,
            modes: Vec::new(),
            mode: None,
            image,
            generation: 0,
            properties_open: false,
            properties_error: None,
            opened: None,
            cmds,
            events,
            event_tx,
        })
        .init_resource::<Calibration>()
        .add_systems(
            Update,
            (receive_events, follow_settings)
                .chain()
                .in_set(Frame::Input),
        );
    }
}

/// A GPU-only sRGB texture: with pixels, or uninitialized when `data` is `None`.
fn rgba_image(width: u32, height: u32, data: Option<Vec<u8>>) -> Image {
    let size = Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    // GPU only: frames are written straight into the texture, no CPU copy is kept.
    let usage = RenderAssetUsages::RENDER_WORLD;
    let format = TextureFormat::Rgba8UnormSrgb;
    match data {
        Some(data) => Image::new(size, TextureDimension::D2, data, format, usage),
        None => Image::new_uninit(size, TextureDimension::D2, format, usage),
    }
}

/// Applies the capture thread's reports: devices, status (remembering the camera in the session),
/// frame size (replacing the texture), rate, formats, settings window closed.
fn receive_events(
    mut webcam: ResMut<Webcam>,
    mut session: ResMut<Session>,
    mut images: ResMut<Assets<Image>>,
) {
    while let Ok(event) = webcam.events.try_recv() {
        match event {
            Event::Devices(devices) => webcam.devices = devices,
            Event::Rate(rate) => webcam.rate = Some(rate),
            Event::Modes(modes, mode) => {
                webcam.modes = modes;
                webcam.mode = mode;
            }
            Event::PropertiesClosed(error) => {
                webcam.properties_open = false;
                webcam.properties_error = error;
            }
            Event::Size(size) => {
                let _ = images.insert(&webcam.image, rgba_image(size.x, size.y, None));
                webcam.size = Some(size);
                webcam.generation += 1;
            }
            Event::Status(status) => {
                if let WebcamStatus::Running { name, .. } = &status
                    && session.camera.as_ref() != Some(name)
                {
                    session.camera = Some(name.clone());
                }
                if !matches!(status, WebcamStatus::Running { .. }) {
                    webcam.size = None;
                }
                webcam.rate = None;
                webcam.status = status;
            }
        }
    }
}

/// The camera runs only with the camera background: the remembered device, else the first one.
fn follow_settings(settings: Res<Settings>, session: Res<Session>, mut webcam: ResMut<Webcam>) {
    let wanted = (settings.background == Background::Camera)
        .then(|| {
            session
                .camera
                .clone()
                .or_else(|| webcam.devices.first().cloned())
        })
        .flatten()
        .map(|name| (name, session.camera_mode.clone()));
    if wanted == webcam.opened {
        return;
    }
    let cmd = match &wanted {
        Some((name, mode)) => Cmd::Open(name.clone(), mode.clone()),
        None => Cmd::Close,
    };
    let _ = webcam.cmds.send(cmd);
    webcam.opened = wanted;
}

/// Copies the newest frame into the camera texture. A frame of another size waits until the
/// texture of its size exists (it is replaced from the main world, one frame later).
fn write_frame(
    latest: Res<LatestFrame>,
    texture: Res<CameraTexture>,
    images: Res<RenderAssets<GpuImage>>,
    queue: Res<RenderQueue>,
) {
    let Some(gpu) = images.get(texture.0) else {
        return;
    };
    let size = gpu.texture_descriptor.size;
    let Some(frame) =
        latest.0.lock().ok().and_then(|mut slot| {
            slot.take_if(|f| f.width == size.width && f.height == size.height)
        })
    else {
        return;
    };
    queue.write_texture(
        TexelCopyTextureInfo {
            texture: &gpu.texture,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        &frame.data,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.width * 4),
            rows_per_image: Some(frame.height),
        },
        size,
    );
}

/// Where the camera image sits in the window (logical px, origin top-left): as large as
/// possible without cropping, centered.
pub fn fit_rect(window: Vec2, camera: Vec2) -> Rect {
    let scale = (window.x / camera.x.max(1.0)).min(window.y / camera.y.max(1.0));
    let size = camera * scale;
    Rect::from_center_size(window / 2.0, size)
}

/// The rectangle the reference points stand for, in scene coordinates (0..1, origin top-left):
/// the flame line across the 88 keys at the bottom, a band of the rising area above it.
pub fn reference_quad(layout: &Layout) -> [Vec2; 4] {
    let half = layout.span_w / layout.width / 2.0;
    let (left, right) = (0.5 - half, 0.5 + half);
    let bottom = 0.5 - layout.hit_y / layout.height;
    let rise = (layout.top_y - layout.hit_y) / layout.height;
    let top = bottom - rise * REFERENCE_HEIGHT;
    [
        Vec2::new(left, top),
        Vec2::new(right, top),
        Vec2::new(right, bottom),
        Vec2::new(left, bottom),
    ]
}

/// Scene → camera image mapping (both in 0..1 coordinates) for the given reference points.
pub fn scene_to_camera(layout: &Layout, corners: &Corners) -> Option<Mat3> {
    homography::from_quads(reference_quad(layout), corners.map(Vec2::from))
}

/// Camera image → scene mapping, used by the composition shader.
pub fn camera_to_scene(layout: &Layout, corners: &Corners) -> Option<Mat3> {
    homography::from_quads(corners.map(Vec2::from), reference_quad(layout))
}

// ---- Worker thread ----

/// The capture thread: runs commands, then while a camera is open reads, decodes and publishes
/// frames, measuring the rate. After 30 failed frames in a row, closes the camera and reports it
/// lost.
fn worker(cmds: &Receiver<Cmd>, events: &Sender<Event>, latest: &Mutex<Option<RgbaFrame>>) {
    let mut camera: Option<Camera> = None;
    let mut failures = 0;
    let mut size = None;
    let mut meter = RateMeter::new();
    let status = |s| {
        let _ = events.send(Event::Status(s));
    };
    loop {
        // Block while idle; poll between frames while streaming.
        let cmd = if camera.is_some() {
            match cmds.try_recv() {
                Ok(cmd) => Some(cmd),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        } else {
            match cmds.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => return,
            }
        };
        match cmd {
            Some(Cmd::Refresh) => {
                let _ = events.send(Event::Devices(names(&list_devices())));
            }
            Some(Cmd::Open(name, mode)) => {
                close(&mut camera, latest);
                size = None;
                status(WebcamStatus::Opening(name.clone()));
                let devices = list_devices();
                let _ = events.send(Event::Devices(names(&devices)));
                failures = 0;
                meter = RateMeter::new();
                match open(&devices, &name, mode.as_ref()) {
                    Ok((cam, running, modes, current)) => {
                        camera = Some(cam);
                        status(running);
                        let _ = events.send(Event::Modes(modes, current));
                    }
                    Err(e) => {
                        status(e);
                        let _ = events.send(Event::Modes(Vec::new(), None));
                    }
                }
            }
            Some(Cmd::Close) => {
                close(&mut camera, latest);
                status(WebcamStatus::Off);
            }
            None => {}
        }
        if let Some(cam) = camera.as_mut() {
            let decoded = cam.frame().ok().and_then(|buffer| {
                let start = Instant::now();
                decode(&buffer).map(|frame| (frame, start.elapsed()))
            });
            match decoded {
                Some((frame, decoding)) => {
                    failures = 0;
                    if let Some(rate) = meter.frame(decoding) {
                        let _ = events.send(Event::Rate(rate));
                    }
                    let frame_size = UVec2::new(frame.width, frame.height);
                    if size != Some(frame_size) {
                        size = Some(frame_size);
                        let _ = events.send(Event::Size(frame_size));
                    }
                    if let Ok(mut slot) = latest.lock() {
                        *slot = Some(frame);
                    }
                }
                None => failures += 1,
            }
            if failures > 30 {
                close(&mut camera, latest);
                status(WebcamStatus::Lost);
            }
        }
    }
}

/// The cameras the system reports.
fn list_devices() -> Vec<CameraInfo> {
    nokhwa::query(ApiBackend::Auto).unwrap_or_default()
}

/// Their display names.
fn names(devices: &[CameraInfo]) -> Vec<String> {
    devices.iter().map(CameraInfo::human_name).collect()
}

/// Stops the stream, drops the camera and empties the frame slot.
fn close(camera: &mut Option<Camera>, latest: &Mutex<Option<RgbaFrame>>) {
    if let Some(mut cam) = camera.take() {
        let _ = cam.stop_stream();
    }
    if let Ok(mut slot) = latest.lock() {
        *slot = None;
    }
}

/// An opened camera, its status, the formats it offers (best first) and the one in use.
type Opened = (Camera, WebcamStatus, Vec<CameraMode>, Option<CameraMode>);

/// Opens `name` with the `wanted` format if the camera offers it, otherwise the best one.
fn open(
    devices: &[CameraInfo],
    name: &str,
    wanted: Option<&CameraMode>,
) -> Result<Opened, WebcamStatus> {
    if devices.is_empty() {
        return Err(WebcamStatus::NoDevice);
    }
    let info = devices
        .iter()
        .find(|d| d.human_name() == name)
        .ok_or_else(|| WebcamStatus::NotFound(name.to_owned()))?;
    let failed = |e: nokhwa::NokhwaError| WebcamStatus::Failed(e.to_string());
    let any = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut cam = Camera::new(info.index().clone(), any).map_err(failed)?;
    // Some backends cannot list formats: the camera then keeps its first one.
    let formats = cam.compatible_camera_formats().unwrap_or_default();
    let chosen = wanted
        .and_then(CameraMode::to_format)
        .filter(|f| formats.contains(f))
        .or_else(|| pick_format(&formats));
    if let Some(format) = chosen {
        // Not fatal either: the camera keeps its first format otherwise.
        let exact =
            RequestedFormat::with_formats(RequestedFormatType::Exact(format), RgbAFormat::FORMATS);
        let _ = cam.set_camera_requset(exact);
    }
    cam.open_stream().map_err(failed)?;
    let format = cam.camera_format();
    info!("camera {name}: {format}");
    Ok((
        cam,
        WebcamStatus::Running {
            name: name.to_owned(),
            width: format.resolution().width(),
            height: format.resolution().height(),
            fps: format.frame_rate(),
        },
        list_modes(&formats),
        Some(CameraMode::from_format(&format)),
    ))
}

/// Smooth first (25 fps or more), then the largest resolution up to 1080p, then the highest
/// frame rate up to 60, then MJPEG, which needs far less USB bandwidth than raw formats.
fn format_rank(f: &CameraFormat) -> (bool, bool, u32, u32, bool) {
    let (w, h) = (f.resolution().width(), f.resolution().height());
    let fits = w <= 1920 && h <= 1080;
    let area = if fits { w * h } else { u32::MAX - w * h };
    (
        f.frame_rate() >= 25,
        fits,
        area,
        f.frame_rate().min(60),
        f.format() == FrameFormat::MJPEG,
    )
}

/// The best format according to [`format_rank`], among those PiViz can decode.
fn pick_format(formats: &[CameraFormat]) -> Option<CameraFormat> {
    formats
        .iter()
        .filter(|f| RgbAFormat::FORMATS.contains(&f.format()))
        .max_by_key(|f| format_rank(f))
        .copied()
}

/// The formats PiViz can decode, for the panel: largest first, then fastest, without duplicates.
fn list_modes(formats: &[CameraFormat]) -> Vec<CameraMode> {
    let mut sorted: Vec<&CameraFormat> = formats
        .iter()
        .filter(|f| RgbAFormat::FORMATS.contains(&f.format()))
        .collect();
    sorted.sort_by_key(|f| {
        let r = f.resolution();
        std::cmp::Reverse((
            r.width() * r.height(),
            f.frame_rate(),
            f.format() == FrameFormat::MJPEG,
        ))
    });
    let mut modes: Vec<CameraMode> = sorted.into_iter().map(CameraMode::from_format).collect();
    modes.dedup();
    modes
}

/// Decodes a frame to RGBA: MJPEG with zune-jpeg, other formats with nokhwa. `None` for a corrupt
/// frame.
fn decode(buffer: &Buffer) -> Option<RgbaFrame> {
    if buffer.source_frame_format() == FrameFormat::MJPEG {
        let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
        let mut decoder = JpegDecoder::new_with_options(ZCursor::new(buffer.buffer()), options);
        let data = decoder.decode().ok()?;
        let (width, height) = decoder.dimensions()?;
        return Some(RgbaFrame {
            width: width as u32,
            height: height as u32,
            data,
        });
    }
    let res = buffer.resolution();
    let mut data = vec![0; res.width() as usize * res.height() as usize * 4];
    buffer
        .decode_image_to_buffer::<RgbAFormat>(&mut data)
        .ok()?;
    Some(RgbaFrame {
        width: res.width(),
        height: res.height(),
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nokhwa::utils::Resolution;

    #[test]
    fn prefers_smooth_1080p_mjpeg() {
        let f = |w, h, fmt, fps| CameraFormat::new(Resolution::new(w, h), fmt, fps);
        let formats = [
            f(3840, 2160, FrameFormat::MJPEG, 30),
            f(1920, 1080, FrameFormat::YUYV, 5),
            f(1920, 1080, FrameFormat::MJPEG, 30),
            f(1280, 720, FrameFormat::MJPEG, 60),
            f(640, 480, FrameFormat::YUYV, 30),
        ];
        assert_eq!(
            pick_format(&formats),
            Some(f(1920, 1080, FrameFormat::MJPEG, 30))
        );
    }

    #[test]
    fn prefers_60_fps_over_mjpeg_at_the_same_size() {
        let f = |w, h, fmt, fps| CameraFormat::new(Resolution::new(w, h), fmt, fps);
        let formats = [
            f(1920, 1080, FrameFormat::MJPEG, 30),
            f(1920, 1080, FrameFormat::NV12, 60),
            f(1280, 720, FrameFormat::MJPEG, 60),
        ];
        assert_eq!(
            pick_format(&formats),
            Some(f(1920, 1080, FrameFormat::NV12, 60))
        );
    }

    #[test]
    fn modes_round_trip_and_sort() {
        let f = |w, h, fmt, fps| CameraFormat::new(Resolution::new(w, h), fmt, fps);
        let formats = [
            f(1280, 720, FrameFormat::MJPEG, 60),
            f(1920, 1080, FrameFormat::MJPEG, 30),
            f(1920, 1080, FrameFormat::MJPEG, 60),
            f(1920, 1080, FrameFormat::MJPEG, 60),
        ];
        let modes = list_modes(&formats);
        assert_eq!(modes.len(), 3);
        assert_eq!((modes[0].width, modes[0].fps), (1920, 60));
        assert_eq!(modes[0].to_format(), Some(formats[2]));
    }

    #[test]
    fn reference_points_map_onto_the_reference_quad() {
        let layout = Layout::new(1600.0, 900.0, true);
        let to_camera = scene_to_camera(&layout, &DEFAULT_CORNERS).unwrap();
        let to_scene = camera_to_scene(&layout, &DEFAULT_CORNERS).unwrap();
        for (scene, corner) in reference_quad(&layout).iter().zip(DEFAULT_CORNERS) {
            let camera = homography::apply(&to_camera, *scene).unwrap();
            assert!(camera.distance(Vec2::from(corner)) < 1e-4);
            let back = homography::apply(&to_scene, camera).unwrap();
            assert!(back.distance(*scene) < 1e-4);
        }
    }
}
