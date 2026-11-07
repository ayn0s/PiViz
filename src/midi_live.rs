//! Live MIDI input through midir.
//!
//! The midir callback runs on its own thread: it forwards messages straight to the
//! audio thread (lowest latency) and copies note events to Bevy through a channel.

use crate::audio::{AudioCmd, AudioHandle};
use crate::colors::Palette;
use crate::persist::Session;
use crate::settings::CliArgs;
use crate::shared::{Frame, KeyHit, KeyLights};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use midir::{Ignore, MidiInput};

/// Name PiViz gives itself to the MIDI system.
const CLIENT_NAME: &str = "PiViz";

/// A note played on the connected instrument.
#[derive(Message, Clone, Copy)]
pub struct LiveNote {
    /// MIDI note number.
    pub key: u8,
    /// 1–127 for a note on, 0 for a note off.
    pub velocity: u8,
    /// Whether the key went down.
    pub on: bool,
}

#[derive(Message, Clone)]
/// Requests from the panel and the startup.
pub enum LiveCmd {
    /// List the MIDI inputs again.
    Refresh,
    /// Connect to this port id.
    Connect(String),
    /// Close the current connection.
    Disconnect,
}

/// A MIDI input port.
pub struct PortInfo {
    /// midir's stable identifier, used to connect.
    pub id: String,
    /// Name shown to the user, remembered in the session.
    pub name: String,
}

/// State shown in the UI.
#[derive(Resource, Default)]
pub struct MidiLive {
    /// MIDI inputs found by the last scan.
    pub ports: Vec<PortInfo>,
    /// Name of the connected port.
    pub connected: Option<String>,
    /// Connection state, for the panel.
    pub status: MidiStatus,
}

/// Connection state, translated for display by `i18n`.
#[derive(Default, Clone, Debug)]
pub enum MidiStatus {
    #[default]
    /// Nothing attempted yet.
    Idle,
    /// The MIDI system itself could not be opened.
    Unavailable(String),
    /// No port matches the name given with `--port`.
    NoMatch(String),
    /// The connection thread is opening the port.
    Connecting,
    /// Connected to this port.
    Connected(String),
    /// Disconnected by the user.
    Disconnected,
    /// The port disappeared (unplugged) before connecting.
    PortNotFound,
    /// The port could not be opened.
    ConnectFailed(String),
}

/// Keys currently held on the instrument (velocity, 0 = released).
#[derive(Resource)]
pub struct LiveKeys {
    /// Velocity of each key currently held, 0 when released.
    pub velocity: [u8; 128],
}

impl Default for LiveKeys {
    fn default() -> Self {
        Self { velocity: [0; 128] }
    }
}

#[derive(Resource)]
/// Channels between the MIDI threads and Bevy.
struct LiveChannels {
    /// For the midir callback (and `--bench`) to send notes.
    note_tx: Sender<LiveNote>,
    /// Notes waiting for Bevy.
    note_rx: Receiver<LiveNote>,
    /// For the connection thread to report its result.
    status_tx: Sender<Result<String, MidiStatus>>,
    /// Connection results waiting for Bevy.
    status_rx: Receiver<Result<String, MidiStatus>>,
    /// Dropping this sender closes the current connection.
    stop: Option<Sender<()>>,
}

/// MIDI input: connection, held keys and note events.
pub struct MidiLivePlugin;

impl Plugin for MidiLivePlugin {
    fn build(&self, app: &mut App) {
        let (note_tx, note_rx) = bounded(4096);
        let (status_tx, status_rx) = unbounded();
        app.insert_resource(LiveChannels {
            note_tx,
            note_rx,
            status_tx,
            status_rx,
            stop: None,
        })
        .init_resource::<MidiLive>()
        .init_resource::<LiveKeys>()
        .add_message::<LiveNote>()
        .add_message::<LiveCmd>()
        .add_systems(Startup, startup)
        .add_systems(
            Update,
            (handle_commands, drain_notes).chain().in_set(Frame::Input),
        )
        .add_systems(Update, light_live_keys.in_set(Frame::Gather))
        .add_systems(
            Update,
            simulate_playing
                .run_if(|cli: Res<CliArgs>| cli.bench)
                .before(drain_notes)
                .in_set(Frame::Input),
        );
    }
}

#[derive(Default)]
/// State of the `--bench` fake pianist.
struct Simulation {
    /// When the next note is due (seconds since startup).
    next: f32,
    /// State of a small linear congruential random generator.
    rng: u32,
    /// Notes being held, with their release time.
    held: Vec<(u8, f32)>,
}

/// `--bench`: feeds dense random playing through the same channel as a real instrument.
fn simulate_playing(time: Res<Time>, channels: Res<LiveChannels>, mut sim: Local<Simulation>) {
    let now = time.elapsed_secs();
    sim.held.retain(|&(key, until)| {
        let done = now >= until;
        if done {
            let _ = channels.note_tx.try_send(LiveNote {
                key,
                velocity: 0,
                on: false,
            });
        }
        !done
    });
    while sim.next <= now {
        // About 16 notes per second, each held 0.2 to 1.2 s: a busy pianist.
        sim.next += 0.06;
        sim.rng = sim.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let r = sim.rng >> 8;
        let key = 33 + (r % 64) as u8;
        if sim.held.iter().any(|&(k, _)| k == key) {
            continue;
        }
        let _ = channels.note_tx.try_send(LiveNote {
            key,
            velocity: 50 + ((r >> 8) % 77) as u8,
            on: true,
        });
        sim.held
            .push((key, now + 0.2 + ((r >> 4) % 100) as f32 / 100.0));
    }
}

/// Connects at startup: to the port named on the command line, otherwise to the port used last
/// time if it is plugged in, otherwise to the first real instrument (virtual "Through" ports only
/// echo other software).
fn startup(
    cli: Res<CliArgs>,
    session: Res<Session>,
    mut live: ResMut<MidiLive>,
    mut cmds: MessageWriter<LiveCmd>,
) {
    refresh_ports(&mut live);
    let wanted = cli.port.as_ref().map(|w| w.to_lowercase());
    let port = match &wanted {
        Some(w) => live
            .ports
            .iter()
            .find(|p| p.name.to_lowercase().contains(w)),
        None => session
            .midi_port
            .as_ref()
            .and_then(|name| live.ports.iter().find(|p| &p.name == name))
            .or_else(|| {
                live.ports
                    .iter()
                    .find(|p| !p.name.to_lowercase().contains("through"))
            }),
    };
    match (port.map(|p| p.id.clone()), wanted) {
        (Some(id), _) => {
            cmds.write(LiveCmd::Connect(id));
        }
        (None, Some(w)) => live.status = MidiStatus::NoMatch(w),
        (None, None) => {}
    }
}

/// Lists the MIDI inputs into [`MidiLive::ports`].
fn refresh_ports(live: &mut MidiLive) {
    live.ports.clear();
    match MidiInput::new(CLIENT_NAME) {
        Ok(input) => {
            for port in input.ports() {
                let name = input.port_name(&port).unwrap_or_else(|_| "?".into());
                live.ports.push(PortInfo {
                    id: port.id(),
                    name,
                });
            }
        }
        Err(e) => live.status = MidiStatus::Unavailable(e.to_string()),
    }
}

/// Runs [`LiveCmd`]s (connect, disconnect, refresh) and applies the connection threads' results.
fn handle_commands(
    mut cmds: MessageReader<LiveCmd>,
    mut live: ResMut<MidiLive>,
    mut channels: ResMut<LiveChannels>,
    mut keys: ResMut<LiveKeys>,
    mut session: ResMut<Session>,
    audio: Res<AudioHandle>,
) {
    for cmd in cmds.read() {
        match cmd {
            LiveCmd::Refresh => refresh_ports(&mut live),
            LiveCmd::Disconnect => {
                channels.stop = None;
                live.connected = None;
                live.status = MidiStatus::Disconnected;
                *keys = LiveKeys::default();
                audio.send(AudioCmd::AllNotesOff);
            }
            LiveCmd::Connect(id) => {
                channels.stop = None;
                *keys = LiveKeys::default();
                audio.send(AudioCmd::AllNotesOff);
                let (stop_tx, stop_rx) = bounded::<()>(0);
                channels.stop = Some(stop_tx);
                live.status = MidiStatus::Connecting;
                spawn_connection(
                    id.clone(),
                    channels.note_tx.clone(),
                    audio.sender(),
                    channels.status_tx.clone(),
                    stop_rx,
                );
            }
        }
    }
    for result in channels.status_rx.try_iter() {
        match result {
            Ok(name) => {
                live.status = MidiStatus::Connected(name.clone());
                if session.midi_port.as_ref() != Some(&name) {
                    session.midi_port = Some(name.clone());
                }
                live.connected = Some(name);
            }
            Err(e) => {
                live.status = e;
                live.connected = None;
            }
        }
    }
}

/// Opens the port on a dedicated thread that holds the connection until `stop` is dropped.
fn spawn_connection(
    port_id: String,
    notes: Sender<LiveNote>,
    audio: Sender<AudioCmd>,
    status: Sender<Result<String, MidiStatus>>,
    stop: Receiver<()>,
) {
    let spawned = std::thread::Builder::new()
        .name("midi-input".into())
        .spawn(move || {
            let mut input = match MidiInput::new(CLIENT_NAME) {
                Ok(i) => i,
                Err(e) => {
                    let _ = status.send(Err(MidiStatus::Unavailable(e.to_string())));
                    return;
                }
            };
            input.ignore(Ignore::All);
            let Some(port) = input.find_port_by_id(&port_id) else {
                let _ = status.send(Err(MidiStatus::PortNotFound));
                return;
            };
            let name = input.port_name(&port).unwrap_or_default();
            let callback =
                move |_stamp: u64, msg: &[u8], _: &mut ()| on_message(msg, &notes, &audio);
            match input.connect(&port, "PiViz in", callback, ()) {
                Ok(connection) => {
                    let _ = status.send(Ok(name));
                    // Blocks this thread only; returns when the sender is dropped.
                    let _ = stop.recv();
                    connection.close();
                }
                Err(e) => {
                    let _ = status.send(Err(MidiStatus::ConnectFailed(e.to_string())));
                }
            }
        });
    if let Err(e) = spawned {
        error!("cannot spawn MIDI thread: {e}");
    }
}

/// Runs on the midir thread: must never block.
fn on_message(msg: &[u8], notes: &Sender<LiveNote>, audio: &Sender<AudioCmd>) {
    let Some(&status) = msg.first() else { return };
    if !(0x80..0xF0).contains(&status) {
        return;
    }
    let data1 = msg.get(1).copied().unwrap_or(0) & 0x7F;
    let data2 = msg.get(2).copied().unwrap_or(0) & 0x7F;
    let _ = audio.try_send(AudioCmd::Midi([status, data1, data2]));
    let note = match status & 0xF0 {
        0x90 if data2 > 0 => LiveNote {
            key: data1,
            velocity: data2,
            on: true,
        },
        0x80 | 0x90 => LiveNote {
            key: data1,
            velocity: 0,
            on: false,
        },
        _ => return,
    };
    let _ = notes.try_send(note);
}

/// Takes the notes received since the last frame: updates [`LiveKeys`] and emits [`LiveNote`] and
/// [`KeyHit`] messages.
fn drain_notes(
    channels: Res<LiveChannels>,
    palette: Res<Palette>,
    mut keys: ResMut<LiveKeys>,
    mut notes: MessageWriter<LiveNote>,
    mut hits: MessageWriter<KeyHit>,
) {
    for note in channels.note_rx.try_iter() {
        let k = note.key as usize;
        keys.velocity[k] = note.velocity;
        if note.on {
            hits.write(KeyHit {
                key: note.key,
                velocity: note.velocity,
                color: palette.note_color(note.key),
            });
        }
        notes.write(note);
    }
}

/// Lights every held key in [`KeyLights`] with its note color.
fn light_live_keys(keys: Res<LiveKeys>, palette: Res<Palette>, mut lights: ResMut<KeyLights>) {
    for (k, &velocity) in keys.velocity.iter().enumerate() {
        if velocity > 0 {
            let color = palette.note_color(k as u8);
            lights.light(k as u8, color);
        }
    }
}
