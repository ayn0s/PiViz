//! Audio output: a cpal stream rendering a rustysynth synthesizer.
//!
//! Bevy and the MIDI thread talk to the audio callback only through a bounded
//! command channel (`try_send`), so nothing ever blocks on audio.

use crate::persist::Session;
use crate::settings::{CliArgs, Settings, soundfont_candidates};
use crate::shared::{Frame, LoadSoundFont};
use bevy::prelude::*;
use bevy::window::FileDragAndDrop;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Largest block rendered at once; bigger device buffers are split.
const BLOCK: usize = 4096;

/// A command for the audio callback.
pub enum AudioCmd {
    /// Raw channel message from the instrument.
    Midi([u8; 3]),
    /// Releases every note (when the MIDI input disconnects).
    AllNotesOff,
    /// Output gain.
    SetVolume(f32),
    /// Swaps in a newly loaded synthesizer; the old one is sent back to be freed outside the
    /// callback.
    SetSynth(Box<Synthesizer>),
}

#[derive(Resource, Clone)]
/// Bevy's side of the audio command channel.
pub struct AudioHandle {
    /// The command channel (bounded: a full queue drops commands instead of blocking).
    tx: Sender<AudioCmd>,
    /// Output sample rate of the device, needed to build a synthesizer.
    pub sample_rate: u32,
    /// False when no output device could be opened.
    pub available: bool,
}

impl AudioHandle {
    /// Never blocks: if the queue is full the command is dropped.
    pub fn send(&self, cmd: AudioCmd) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(cmd) {
            warn!("audio command queue full, command dropped");
        }
    }

    /// A clone of the raw sender, for threads that may block safely (the loader, the MIDI
    /// callback).
    pub fn sender(&self) -> Sender<AudioCmd> {
        self.tx.clone()
    }
}

/// SoundFont loading state shown in the UI.
#[derive(Resource, Default)]
pub struct SoundFontStatus {
    /// The SoundFont path, as shown and edited in the panel.
    pub path: String,
    /// Loading state.
    pub state: SoundFontState,
}

/// Translated for display by `i18n`.
#[derive(Default, Clone, Debug)]
pub enum SoundFontState {
    #[default]
    /// Nothing loaded or requested yet.
    Idle,
    /// None of the default locations has one; lists where it looked.
    NotFound(String),
    /// Being parsed on the loader thread.
    Loading,
    /// Loaded; holds the file name.
    Loaded(String),
    /// The file could not be read.
    ReadError(String),
    /// The file is not a valid SoundFont.
    Invalid(String),
}

/// Results from the loader thread and synthesizers retired by the audio thread.
#[derive(Resource)]
struct AudioChannels {
    /// For the loader threads to report their result.
    status_tx: Sender<Result<String, SoundFontState>>,
    /// Results of the loader threads.
    status_rx: Receiver<Result<String, SoundFontState>>,
    /// Synthesizers retired by the audio callback, freed by Bevy.
    trash_rx: Receiver<Box<Synthesizer>>,
}

/// Sound output and SoundFont loading.
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        let (tx, rx) = bounded(1024);
        let (trash_tx, trash_rx) = unbounded();
        let (sample_rate, available) = match start_output(rx, trash_tx) {
            Ok(sr) => (sr, true),
            Err(e) => {
                error!("audio disabled: {e}");
                (48_000, false)
            }
        };
        let (status_tx, status_rx) = unbounded();
        app.insert_resource(AudioHandle {
            tx,
            sample_rate,
            available,
        })
        .insert_resource(AudioChannels {
            status_tx,
            status_rx,
            trash_rx,
        })
        .init_resource::<SoundFontStatus>()
        .add_systems(Startup, load_initial_soundfont)
        .add_systems(
            Update,
            (
                handle_drops,
                load_soundfont,
                poll_audio_channels,
                sync_volume,
            )
                .chain()
                .in_set(Frame::Input),
        );
    }
}

/// Chooses the first SoundFont: `--soundfont`, the last one used if it still exists, then the
/// default locations.
fn load_initial_soundfont(
    cli: Res<CliArgs>,
    session: Res<Session>,
    mut writer: MessageWriter<LoadSoundFont>,
    mut status: ResMut<SoundFontStatus>,
) {
    let candidates = soundfont_candidates();
    // Command line, then the SoundFont used last time, then the default locations.
    let path = cli
        .soundfont
        .clone()
        .or_else(|| session.soundfont.clone().filter(|p| p.is_file()))
        .or_else(|| candidates.iter().find(|p| p.is_file()).cloned());
    match path {
        Some(p) => {
            writer.write(LoadSoundFont(p));
        }
        None => {
            let hint = candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            status.state = SoundFontState::NotFound(hint);
        }
    }
}

/// Dropping a .sf2 file on the window loads it.
fn handle_drops(
    mut drops: MessageReader<FileDragAndDrop>,
    mut writer: MessageWriter<LoadSoundFont>,
) {
    for drop in drops.read() {
        if let FileDragAndDrop::DroppedFile { path_buf, .. } = drop {
            let is_sf2 = path_buf
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("sf2"));
            if is_sf2 {
                writer.write(LoadSoundFont(path_buf.clone()));
            } else {
                warn!("unsupported file: {}", path_buf.display());
            }
        }
    }
}

/// Parses the SoundFont on a worker thread, then hands the synthesizer to the audio thread.
fn load_soundfont(
    mut requests: MessageReader<LoadSoundFont>,
    audio: Res<AudioHandle>,
    channels: Res<AudioChannels>,
    mut status: ResMut<SoundFontStatus>,
) {
    let Some(LoadSoundFont(path)) = requests.read().last().cloned() else {
        return;
    };
    status.path = path.display().to_string();
    status.state = SoundFontState::Loading;
    let tx = audio.sender();
    let status_tx = channels.status_tx.clone();
    let sample_rate = audio.sample_rate;
    std::thread::Builder::new()
        .name("soundfont-loader".into())
        .spawn(move || {
            let result = build_synth(&path, sample_rate).map(|synth| {
                // The audio queue may be momentarily full; the loader thread can afford to wait.
                let _ = tx.send(AudioCmd::SetSynth(Box::new(synth)));
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let _ = status_tx.send(result);
        })
        .expect("failed to spawn soundfont loader");
}

/// Parses a SoundFont and builds a synthesizer for the device's sample rate.
fn build_synth(path: &PathBuf, sample_rate: u32) -> Result<Synthesizer, SoundFontState> {
    let file = File::open(path)
        .map_err(|e| SoundFontState::ReadError(format!("{}: {e}", path.display())))?;
    let font = SoundFont::new(&mut BufReader::new(file))
        .map_err(|e| SoundFontState::Invalid(format!("{e:?}")))?;
    let settings = SynthesizerSettings::new(sample_rate as i32);
    Synthesizer::new(&Arc::new(font), &settings)
        .map_err(|e| SoundFontState::Invalid(format!("{e:?}")))
}

/// Applies the loader results (remembering a loaded SoundFont in the session) and frees retired
/// synthesizers.
fn poll_audio_channels(
    channels: Res<AudioChannels>,
    mut status: ResMut<SoundFontStatus>,
    mut session: ResMut<Session>,
) {
    for result in channels.status_rx.try_iter() {
        status.state = match result {
            Ok(name) => {
                // Remember it, as an absolute path so it still resolves from another directory.
                let path = PathBuf::from(&status.path);
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                if session.soundfont.as_ref() != Some(&path) {
                    session.soundfont = Some(path);
                }
                SoundFontState::Loaded(name)
            }
            Err(state) => state,
        };
    }
    // Dropping retired synthesizers here keeps deallocation off the audio thread.
    for synth in channels.trash_rx.try_iter() {
        drop(synth);
    }
}

/// Sends the volume to the audio thread when it changes.
fn sync_volume(settings: Res<Settings>, audio: Res<AudioHandle>, mut last: Local<Option<f32>>) {
    if *last != Some(settings.volume) {
        *last = Some(settings.volume);
        audio.send(AudioCmd::SetVolume(settings.volume));
    }
}

/// Opens the default output device on a dedicated thread that keeps the stream alive.
/// Returns the device sample rate.
fn start_output(rx: Receiver<AudioCmd>, trash: Sender<Box<Synthesizer>>) -> Result<u32, String> {
    let (ready_tx, ready_rx) = bounded(1);
    std::thread::Builder::new()
        .name("audio-output".into())
        .spawn(move || {
            let stream = match open_stream(rx, trash) {
                Ok((stream, sr)) => {
                    let _ = ready_tx.send(Ok(sr));
                    stream
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // cpal streams are not Send on every platform: this thread owns it for the app lifetime.
            loop {
                std::thread::park();
                let _ = &stream;
            }
        })
        .map_err(|e| e.to_string())?;
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "timeout opening audio device".to_string())?
}

/// Opens the default output device: tries a small buffer (low latency) first, then the device
/// default.
fn open_stream(
    rx: Receiver<AudioCmd>,
    trash: Sender<Box<Synthesizer>>,
) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no output device")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let sample_rate = supported.sample_rate();
    let default_config = supported.config();
    let mut configs = vec![default_config];
    // Prefer a small buffer so live playing stays responsive; fall back to the device default.
    if let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() {
        let mut small = default_config;
        small.buffer_size = cpal::BufferSize::Fixed(512.clamp(*min, *max));
        configs.insert(0, small);
    }
    let mut last_err = String::new();
    for config in configs {
        let engine = Engine::new(rx.clone(), trash.clone());
        let built = match supported.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, engine),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, engine),
            cpal::SampleFormat::I32 => build::<i32>(&device, config, engine),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, engine),
            other => return Err(format!("unsupported sample format {other}")),
        };
        match built {
            Ok(stream) => {
                stream.play().map_err(|e| e.to_string())?;
                info!("audio output: {sample_rate} Hz");
                return Ok((stream, sample_rate));
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Builds the output stream for the device's sample type `T`; its callback runs [`Engine::render`].
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut engine: Engine,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| engine.render(out, channels),
            |e: cpal::Error| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// State living inside the audio callback.
struct Engine {
    /// Commands from Bevy, the MIDI callback and the loader.
    rx: Receiver<AudioCmd>,
    /// Where replaced synthesizers go, to be freed outside the callback.
    trash: Sender<Box<Synthesizer>>,
    /// The synthesizer; `None` (silence) until a SoundFont is loaded.
    synth: Option<Box<Synthesizer>>,
    /// Output gain.
    volume: f32,
    /// Left channel buffer, allocated once.
    left: Vec<f32>,
    /// Right channel buffer, allocated once.
    right: Vec<f32>,
}

impl Engine {
    /// An engine with its buffers allocated up front, so rendering never allocates.
    fn new(rx: Receiver<AudioCmd>, trash: Sender<Box<Synthesizer>>) -> Self {
        Self {
            rx,
            trash,
            synth: None,
            volume: 1.0,
            left: vec![0.0; BLOCK],
            right: vec![0.0; BLOCK],
        }
    }

    /// Audio callback: applies pending commands, renders the synthesizer and interleaves the output
    /// with the volume applied.
    fn render<T: SizedSample + FromSample<f32>>(&mut self, out: &mut [T], channels: usize) {
        self.handle_commands();
        for chunk in out.chunks_mut(BLOCK * channels) {
            let frames = chunk.len() / channels;
            let (l, r) = (&mut self.left[..frames], &mut self.right[..frames]);
            match self.synth.as_mut() {
                Some(s) => s.render(l, r),
                None => {
                    l.fill(0.0);
                    r.fill(0.0);
                }
            }
            for (i, frame) in chunk.chunks_mut(channels).enumerate() {
                let l = self.left[i] * self.volume;
                let r = self.right[i] * self.volume;
                for (c, sample) in frame.iter_mut().enumerate() {
                    *sample = T::from_sample(if c % 2 == 0 { l } else { r });
                }
            }
        }
    }

    /// Applies every queued command without blocking.
    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                AudioCmd::Midi([status, d1, d2]) => {
                    if let Some(s) = self.synth.as_mut() {
                        s.process_midi_message(
                            (status & 0x0F) as i32,
                            (status & 0xF0) as i32,
                            d1 as i32,
                            d2 as i32,
                        );
                    }
                }
                AudioCmd::AllNotesOff => {
                    if let Some(s) = self.synth.as_mut() {
                        s.note_off_all(false);
                    }
                }
                AudioCmd::SetVolume(v) => self.volume = v,
                AudioCmd::SetSynth(synth) => {
                    if let Some(old) = self.synth.replace(synth) {
                        let _ = self.trash.send(old);
                    }
                }
            }
        }
    }
}
