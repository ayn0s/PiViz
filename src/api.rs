//! WebSocket remote control: read the state and change any setting live, from a Stream Deck
//! plugin, a script or any WebSocket client. The protocol is described in `docs/API.md`.
//!
//! The server only listens on 127.0.0.1. One thread accepts connections and one thread per client
//! reads its messages; requests are handled by a Bevy system, which also pushes the new state to
//! every client whenever something changes.

use crate::colors::{ColorScheme, builtin_presets};
use crate::persist::{Library, Session};
use crate::settings::{Background, Settings};
use crate::shared::Frame;
use crate::webcam::{Webcam, WebcamStatus};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Map, Value, json};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

/// Port used when none was chosen in the panel.
pub const DEFAULT_PORT: u16 = 7341;
/// Bumped when a change could break existing clients.
const API_VERSION: u32 = 1;
/// State events are sent at most this often while something keeps changing (a slider dragged).
const STATE_INTERVAL: f64 = 0.1;

#[derive(Clone, Debug, PartialEq)]
/// State of the server, shown in the panel.
pub enum ApiStatus {
    /// Turned off in the panel.
    Off,
    /// Accepting connections on this port.
    Listening(u16),
    /// The port could not be opened (already used by another program, …).
    Failed(u16, String),
}

#[derive(Resource)]
/// The remote control server and its connections.
pub struct Api {
    /// Shown in the panel.
    pub status: ApiStatus,
    /// The running server; `None` when off or failed.
    server: Option<Server>,
    /// Requests from the client threads, handled by [`handle_requests`].
    requests: Receiver<Request>,
    /// Cloned into each new server for its client threads.
    request_tx: Sender<Request>,
    /// Outgoing queues of the connected clients.
    clients: Clients,
    /// Where `camera` with `enabled: false` returns to.
    background_before_camera: Background,
}

/// Outgoing message queues of the connected clients, by client id.
type Clients = Arc<Mutex<Vec<(u64, Sender<String>)>>>;

/// A running server; dropping it stops the accept loop and every client thread.
struct Server {
    /// The port it listens on.
    port: u16,
    /// Set when the server is dropped.
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A message from a client.
struct Request {
    /// Id of the client, to send the response back.
    client: u64,
    /// The message text; empty when the client has just connected.
    text: String,
}

/// The WebSocket remote control (protocol in `docs/API.md`).
pub struct ApiPlugin;

impl Plugin for ApiPlugin {
    fn build(&self, app: &mut App) {
        let (request_tx, requests) = unbounded();
        app.insert_resource(Api {
            status: ApiStatus::Off,
            server: None,
            requests,
            request_tx,
            clients: Arc::default(),
            background_before_camera: Background::Theme,
        })
        .add_systems(
            Update,
            (manage_server, handle_requests)
                .chain()
                .in_set(Frame::Input),
        )
        .add_systems(Last, publish_state);
    }
}

// ---- Server threads ----

/// Starts or stops the server to match the session (enabled, port).
fn manage_server(session: Res<Session>, mut api: ResMut<Api>) {
    let wanted = session.api_enabled.then_some(session.api_port);
    let running = api.server.as_ref().map(|s| s.port);
    let failed = matches!(api.status, ApiStatus::Failed(port, _) if Some(port) == wanted);
    if wanted == running || failed {
        return;
    }
    api.server = None;
    let Some(port) = wanted else {
        api.status = ApiStatus::Off;
        return;
    };
    match start(port, api.request_tx.clone(), api.clients.clone()) {
        Ok(server) => {
            info!("API listening on ws://127.0.0.1:{port}");
            api.server = Some(server);
            api.status = ApiStatus::Listening(port);
        }
        Err(e) => {
            warn!("API: cannot listen on port {port}: {e}");
            api.status = ApiStatus::Failed(port, e);
        }
    }
}

/// Binds `127.0.0.1:port` (so a busy port is reported at once) and starts the accept thread.
fn start(port: u16, requests: Sender<Request>, clients: Clients) -> Result<Server, String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    std::thread::Builder::new()
        .name("api".into())
        .spawn(move || accept_loop(&listener, &requests, &clients, &thread_stop))
        .map_err(|e| e.to_string())?;
    Ok(Server { port, stop })
}

/// Accepts connections until the server stops, one thread per client (polls every 50 ms so it can
/// stop).
fn accept_loop(
    listener: &TcpListener,
    requests: &Sender<Request>,
    clients: &Clients,
    stop: &Arc<AtomicBool>,
) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let (requests, clients, stop) = (requests.clone(), clients.clone(), stop.clone());
                let _ = std::thread::Builder::new()
                    .name(format!("api-client-{id}"))
                    .spawn(move || serve_client(id, stream, &requests, &clients, &stop));
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!("API: accept failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// One client: forwards its text messages to Bevy and sends it what Bevy queues for it.
fn serve_client(
    id: u64,
    stream: TcpStream,
    requests: &Sender<Request>,
    clients: &Clients,
    stop: &AtomicBool,
) {
    // The handshake is blocking; a short timeout keeps a silent client from holding the thread.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    // Then reads wait briefly, so outgoing messages are sent promptly between them.
    let _ = socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(20)));
    let (tx, outgoing) = unbounded();
    if let Ok(mut list) = clients.lock() {
        list.push((id, tx));
    }
    // A first state event is queued by Bevy for every new client (see `publish_state`).
    let _ = requests.send(Request {
        client: id,
        text: String::new(),
    });
    while !stop.load(Ordering::Relaxed) && exchange(id, &mut socket, requests, &outgoing) {}
    let _ = socket.close(None);
    let _ = socket.flush();
    if let Ok(mut list) = clients.lock() {
        list.retain(|(client, _)| *client != id);
    }
}

/// One round of reading and writing; false once the connection is over.
fn exchange(
    id: u64,
    socket: &mut WebSocket<TcpStream>,
    requests: &Sender<Request>,
    outgoing: &Receiver<String>,
) -> bool {
    match socket.read() {
        Ok(Message::Text(text)) => {
            let _ = requests.send(Request {
                client: id,
                text: text.to_string(),
            });
        }
        Ok(Message::Close(_)) => return false,
        // Pings are answered by tungstenite itself; binary messages are not part of the API.
        Ok(_) => {}
        Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
        Err(_) => return false,
    }
    for message in outgoing.try_iter() {
        if socket.send(Message::text(message)).is_err() {
            return false;
        }
    }
    // Pending pongs.
    match socket.flush() {
        Ok(()) => true,
        Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
        {
            true
        }
        Err(_) => false,
    }
}

impl Api {
    /// Queues `message` for one client.
    fn send(&self, client: u64, message: &Value) {
        if let Ok(list) = self.clients.lock()
            && let Some((_, tx)) = list.iter().find(|(id, _)| *id == client)
        {
            let _ = tx.send(message.to_string());
        }
    }

    /// Queues `message` for every client.
    fn broadcast(&self, message: &Value) {
        let text = message.to_string();
        if let Ok(list) = self.clients.lock() {
            for (_, tx) in list.iter() {
                let _ = tx.send(text.clone());
            }
        }
    }
}

// ---- Requests ----

/// What a request handler can read and change.
struct Context<'a> {
    /// Copy of the settings, written back if changed.
    settings: &'a mut Settings,
    /// Copy of the session, written back if changed.
    session: &'a mut Session,
    /// Profiles and color presets.
    library: &'a Library,
    /// Camera devices and status.
    webcam: &'a Webcam,
    /// Where `camera` off returns to.
    background_before_camera: Background,
}

/// Runs the pending requests against copies of the settings and session, answers each one, and
/// writes the copies back if they changed.
fn handle_requests(
    mut api: ResMut<Api>,
    mut settings: ResMut<Settings>,
    mut session: ResMut<Session>,
    library: Res<Library>,
    webcam: Res<Webcam>,
) {
    if settings.background != Background::Camera {
        api.background_before_camera = settings.background;
    }
    let requests: Vec<Request> = api.requests.try_iter().collect();
    if requests.is_empty() {
        return;
    }
    // Edit copies, written back only if something changed (see the panel for the same pattern).
    let mut new_settings = settings.clone();
    let mut new_session = session.clone();
    let mut context = Context {
        settings: &mut new_settings,
        session: &mut new_session,
        library: &library,
        webcam: &webcam,
        background_before_camera: api.background_before_camera,
    };
    for request in requests {
        // An empty request is a new connection: greet it with the state.
        if request.text.is_empty() {
            let event = json!({ "event": "state", "state": context.state() });
            api.send(request.client, &event);
            continue;
        }
        let response = respond(&mut context, &request.text);
        api.send(request.client, &response);
    }
    settings.set_if_neq(new_settings);
    session.set_if_neq(new_session);
}

/// Parses and runs one request, and builds its response.
fn respond(context: &mut Context, text: &str) -> Value {
    let request: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => return json!({ "ok": false, "error": format!("invalid JSON: {e}") }),
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match request.get("type").and_then(Value::as_str) {
        Some(kind) => run(context, kind, &request),
        None => Err("missing \"type\"".to_owned()),
    };
    match result {
        Ok(Some(state)) => json!({ "id": id, "ok": true, "state": state }),
        Ok(None) => json!({ "id": id, "ok": true }),
        Err(error) => json!({ "id": id, "ok": false, "error": error }),
    }
}

/// Runs a command. `Ok(Some(state))` for `get_state`, `Ok(None)` for changes (the new state
/// follows as an event).
fn run(context: &mut Context, kind: &str, request: &Value) -> Result<Option<Value>, String> {
    let s = &mut *context.settings;
    match kind {
        "get_state" => return Ok(Some(context.state())),
        "set" => {
            let patch = request
                .get("settings")
                .ok_or("missing \"settings\"")?
                .clone();
            *s = merge_settings(s, patch)?;
        }
        "panel" => s.show_panel = switch(request.get("visible"), s.show_panel)?,
        "keyboard" => s.show_keyboard = switch(request.get("visible"), s.show_keyboard)?,
        "camera" => {
            if let Some(device) = request.get("device") {
                let device = device.as_str().ok_or("\"device\" must be a string")?;
                if !context.webcam.devices.iter().any(|d| d == device) {
                    return Err(format!("unknown camera {device:?}"));
                }
                if context.session.camera.as_deref() != Some(device) {
                    context.session.camera = Some(device.to_owned());
                    context.session.camera_mode = None;
                }
            }
            let on = s.background == Background::Camera;
            let wanted = match request.get("enabled") {
                // Choosing a device alone does not turn the camera on or off.
                None if request.get("device").is_some() => on,
                value => switch(value, on)?,
            };
            if wanted != on {
                s.background = if wanted {
                    Background::Camera
                } else {
                    context.background_before_camera
                };
            }
        }
        "color_preset" => {
            let presets = color_presets(context.library);
            let index = pick(
                &presets
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>(),
                presets.iter().position(|(_, scheme)| *scheme == s.colors),
                request,
            )?;
            s.colors = presets[index].1.clone();
        }
        "profile" => {
            let names: Vec<String> = context.library.profiles.keys().cloned().collect();
            let current = names
                .iter()
                .position(|name| profile_matches(&context.library.profiles[name], s));
            let index = pick(&names, current, request)?;
            let show_panel = s.show_panel;
            *s = context.library.profiles[&names[index]].clone();
            s.show_panel = show_panel;
            s.sanitize();
        }
        other => return Err(format!("unknown type {other:?}")),
    }
    Ok(None)
}

/// `true`, `false`, or `"toggle"` (also when absent).
fn switch(value: Option<&Value>, current: bool) -> Result<bool, String> {
    match value {
        None => Ok(!current),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s == "toggle" => Ok(!current),
        Some(_) => Err("expected true, false or \"toggle\"".to_owned()),
    }
}

/// The entry chosen by `"name"`, or `"step"` (+1 next, -1 previous, wrapping) from `current`.
fn pick(names: &[String], current: Option<usize>, request: &Value) -> Result<usize, String> {
    if names.is_empty() {
        return Err("the list is empty".to_owned());
    }
    if let Some(name) = request.get("name") {
        let name = name.as_str().ok_or("\"name\" must be a string")?;
        return names
            .iter()
            .position(|n| n == name)
            .ok_or_else(|| format!("unknown name {name:?}"));
    }
    let step = request
        .get("step")
        .ok_or("expected \"name\" or \"step\"")?
        .as_i64()
        .ok_or("\"step\" must be an integer")?;
    let len = names.len() as i64;
    let from = match current {
        Some(i) => i as i64,
        // Not on any entry: +1 lands on the first one, -1 on the last one.
        None if step > 0 => -1,
        None => len,
    };
    Ok((from + step).rem_euclid(len) as usize)
}

/// Built-in presets first, then the user's, as in the panel.
fn color_presets(library: &Library) -> Vec<(String, ColorScheme)> {
    builtin_presets()
        .into_iter()
        .map(|(name, scheme)| (name.to_owned(), scheme))
        .chain(
            library
                .color_presets
                .iter()
                .map(|(name, scheme)| (name.clone(), scheme.clone())),
        )
        .collect()
}

/// A profile is "current" if loading it would change nothing.
fn profile_matches(profile: &Settings, settings: &Settings) -> bool {
    let mut profile = profile.clone();
    profile.show_panel = settings.show_panel;
    profile.sanitize();
    profile == *settings
}

/// Applies a partial settings object: given fields are replaced, nested objects are merged
/// field by field, arrays are replaced whole. Colors may be written `"#rrggbb"`.
fn merge_settings(settings: &Settings, mut patch: Value) -> Result<Settings, String> {
    hex_colors_to_arrays(&mut patch);
    let mut value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    merge(&mut value, patch, "")?;
    let mut merged: Settings =
        serde_json::from_value(value).map_err(|e| format!("invalid settings: {e}"))?;
    // Not serialized: keep it.
    merged.show_panel = settings.show_panel;
    merged.sanitize();
    Ok(merged)
}

/// Merges `patch` into `base`: objects field by field (an unknown key is an error naming its
/// `path`), anything else replaced.
fn merge(base: &mut Value, patch: Value, path: &str) -> Result<(), String> {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                let path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let field = base
                    .get_mut(&key)
                    .ok_or_else(|| format!("unknown setting {path:?}"))?;
                merge(field, value, &path)?;
            }
            Ok(())
        }
        (base, patch) => {
            *base = patch;
            Ok(())
        }
    }
}

/// Replaces every `"#rrggbb"` string with `[r, g, b]`.
fn hex_colors_to_arrays(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Some(rgb) = parse_hex(s) {
                *value = json!(rgb);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(hex_colors_to_arrays),
        Value::Object(fields) => fields.values_mut().for_each(hex_colors_to_arrays),
        _ => {}
    }
}

/// `"#rrggbb"` → `[r, g, b]`.
fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

// ---- State ----

impl Context<'_> {
    /// The state of this context, as sent to clients.
    fn state(&self) -> Value {
        state(self.settings, self.session, self.library, self.webcam)
    }
}

/// Everything a client shows: settings, camera, presets and profiles.
fn state(settings: &Settings, session: &Session, library: &Library, webcam: &Webcam) -> Value {
    let mut settings_value = serde_json::to_value(settings).unwrap_or(Value::Null);
    round_floats(&mut settings_value);
    if let Value::Object(fields) = &mut settings_value {
        fields.remove("show_panel");
    }
    let presets = color_presets(library);
    let current_preset = presets
        .iter()
        .find(|(_, scheme)| *scheme == settings.colors)
        .map(|(name, _)| name.clone());
    let current_profile = library
        .profiles
        .iter()
        .find(|(_, profile)| profile_matches(profile, settings))
        .map(|(name, _)| name.clone());
    let mut camera = Map::new();
    camera.insert(
        "enabled".into(),
        json!(settings.background == Background::Camera),
    );
    camera.insert("device".into(), json!(session.camera));
    camera.insert("devices".into(), json!(webcam.devices));
    camera.insert("status".into(), json!(status_name(&webcam.status)));
    camera.insert("fps".into(), json!(webcam.rate.map(|r| r.fps.round())));
    json!({
        "api_version": API_VERSION,
        "settings": settings_value,
        "panel_visible": settings.show_panel,
        "camera": camera,
        "color_presets": presets.iter().map(|(name, _)| name).collect::<Vec<_>>(),
        "color_preset": current_preset,
        "profiles": library.profiles.keys().collect::<Vec<_>>(),
        "profile": current_profile,
    })
}

/// Settings are `f32`: shown as JSON doubles they read 0.2800000011920929. Rounded to 4 decimals.
fn round_floats(value: &mut Value) {
    match value {
        Value::Number(n) if n.is_f64() => {
            if let Some(rounded) = n
                .as_f64()
                .and_then(|x| serde_json::Number::from_f64((x * 1e4).round() / 1e4))
            {
                *n = rounded;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(round_floats),
        Value::Object(fields) => fields.values_mut().for_each(round_floats),
        _ => {}
    }
}

/// Name of a camera status in the state, such as `"running"`.
fn status_name(status: &WebcamStatus) -> &'static str {
    match status {
        WebcamStatus::Off => "off",
        WebcamStatus::Opening(_) => "opening",
        WebcamStatus::Running { .. } => "running",
        WebcamStatus::NoDevice => "no_device",
        WebcamStatus::NotFound(_) => "not_found",
        WebcamStatus::Failed(_) => "failed",
        WebcamStatus::Lost => "lost",
    }
}

/// Pushes the state to every client when it changed, at most every `STATE_INTERVAL` while
/// it keeps changing, and always once after the last change.
#[allow(clippy::too_many_arguments)]
fn publish_state(
    api: Res<Api>,
    settings: Res<Settings>,
    session: Res<Session>,
    library: Res<Library>,
    webcam: Res<Webcam>,
    time: Res<Time<Real>>,
    mut pending: Local<bool>,
    mut last_sent: Local<f64>,
) {
    if settings.is_changed() || session.is_changed() || library.is_changed() || webcam.is_changed()
    {
        *pending = true;
    }
    let now = time.elapsed_secs_f64();
    if !*pending || now - *last_sent < STATE_INTERVAL || api.server.is_none() {
        return;
    }
    let state = state(&settings, &session, &library, &webcam);
    api.broadcast(&json!({ "event": "state", "state": state }));
    *pending = false;
    *last_sent = now;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_merge_and_validate() {
        let settings = Settings::default();
        let merged = merge_settings(
            &settings,
            json!({ "bloom": 0.5, "colors": { "accent": "#ff8000" }, "background": "black" }),
        )
        .unwrap();
        assert_eq!(merged.bloom, 0.5);
        assert_eq!(merged.colors.accent, [255, 128, 0]);
        assert_eq!(merged.background, Background::Black);
        assert_eq!(merged.colors.solid, settings.colors.solid);

        assert!(merge_settings(&settings, json!({ "no_such": 1 })).is_err());
        assert!(merge_settings(&settings, json!({ "bloom": "high" })).is_err());
        // Out-of-range values are brought back into range.
        let clamped = merge_settings(&settings, json!({ "bloom": 9.0 })).unwrap();
        assert_eq!(clamped.bloom, 1.0);
    }

    #[test]
    fn steps_wrap_and_names_resolve() {
        let names: Vec<String> = ["A", "B", "C"].map(String::from).to_vec();
        assert_eq!(pick(&names, Some(2), &json!({ "step": 1 })), Ok(0));
        assert_eq!(pick(&names, Some(0), &json!({ "step": -1 })), Ok(2));
        assert_eq!(pick(&names, None, &json!({ "step": 1 })), Ok(0));
        assert_eq!(pick(&names, None, &json!({ "step": -1 })), Ok(2));
        assert_eq!(pick(&names, None, &json!({ "name": "B" })), Ok(1));
        assert!(pick(&names, None, &json!({ "name": "Z" })).is_err());
    }

    #[test]
    fn switches_accept_bools_and_toggle() {
        assert_eq!(switch(Some(&json!(true)), false), Ok(true));
        assert_eq!(switch(Some(&json!("toggle")), true), Ok(false));
        assert_eq!(switch(None, false), Ok(true));
        assert!(switch(Some(&json!(1)), false).is_err());
    }
}
