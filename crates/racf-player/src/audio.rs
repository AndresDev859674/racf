use std::io::IsTerminal;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use racf_core::{Document, ScheduledAction, ScheduledEvent, Timeline};

use crate::control::{
    self, format_duration, Command, EqRequest, LoopAction, SeekRequest, StatusInfo, VolumeChange,
};
use crate::eq::{self, EqPreset};
use crate::error::PlayerError;
use crate::mixer::MasterMixer;
use crate::mpris::{MprisHandle, PlaybackStatus};
use crate::ui::{self, TerminalGuard, Ui, UiEvent};

const EVENT_TICK: Duration = Duration::from_millis(5);
const UI_TICK: Duration = Duration::from_millis(100);
const TAIL_MS: u64 = 700;

/// Initial playback settings controlled from the command line.
#[derive(Clone, Debug)]
pub struct PlaybackOptions {
    /// Master volume in percent; `100.0` is unity.
    pub volume_percent: f32,
    /// Optional equalizer preset.
    pub eq: Option<EqPreset>,
    /// Repeat from the beginning when the timeline ends.
    pub looping: bool,
}

impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            volume_percent: 100.0,
            eq: None,
            looping: false,
        }
    }
}

/// Opens the default output device and plays a compiled timeline.
///
/// `source` is used as a fallback title for the MPRIS service. Set
/// `enable_mpris` to `false` to run without publishing to D-Bus.
pub fn play(
    document: &Document,
    timeline: &Timeline,
    source: &Path,
    enable_mpris: bool,
    options: &PlaybackOptions,
) -> Result<(), PlayerError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(PlayerError::NoOutputDevice)?;
    let supported = device
        .default_output_config()
        .map_err(|error| PlayerError::Audio(error.to_string()))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let sample_rate = config.sample_rate.0 as f32;
    let device_channels = config.channels as usize;

    let mixer = Arc::new(Mutex::new(MasterMixer::new(document, sample_rate)));
    {
        let mut guard = mixer.lock().unwrap();
        guard.set_master_volume_percent(options.volume_percent);
        if let Some(preset) = options.eq {
            guard.set_eq_preset(preset);
        }
    }

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::F64 => build_stream::<f64>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I8 => build_stream::<i8>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I64 => build_stream::<i64>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U32 => build_stream::<u32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U64 => build_stream::<u64>(&device, &config, Arc::clone(&mixer)),
        other => {
            return Err(PlayerError::UnsupportedSampleFormat(format!("{other:?}")));
        }
    }?;

    stream
        .play()
        .map_err(|error| PlayerError::Audio(error.to_string()))?;

    println!(
        "[RACF] Output: {} | {} Hz | {} channel(s) | layout: {}",
        device
            .name()
            .unwrap_or_else(|_| "unknown device".to_string()),
        config.sample_rate.0,
        device_channels,
        mixer.lock().unwrap().layout()
    );

    let mpris = crate::mpris::start(document, timeline, source, enable_mpris);
    let title = document
        .metadata
        .title
        .clone()
        .or_else(|| {
            source
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "RACF document".to_string());

    let mut interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let guard = if interactive {
        match TerminalGuard::enable() {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("[RACF] Interactive UI unavailable: {error}");
                interactive = false;
                None
            }
        }
    } else {
        None
    };

    let (sender, receiver) = mpsc::channel();
    ui::spawn_input(interactive, sender);

    let transport = Transport {
        document,
        timeline,
        mixer,
        mpris,
        ui: Ui::new(interactive),
        receiver,
        cursor: 0,
        base_ms: 0,
        anchor: Instant::now(),
        paused: false,
        pause_position: 0,
        looping: options.looping,
        next_ui: Instant::now(),
        title,
        interactive,
        _terminal: guard,
    };

    let result = transport.run();
    drop(stream);
    result
}

struct Transport<'a> {
    document: &'a Document,
    timeline: &'a Timeline,
    mixer: Arc<Mutex<MasterMixer>>,
    mpris: MprisHandle,
    ui: Ui,
    receiver: Receiver<UiEvent>,
    cursor: usize,
    base_ms: u64,
    anchor: Instant,
    paused: bool,
    pause_position: u64,
    looping: bool,
    next_ui: Instant,
    title: String,
    interactive: bool,
    _terminal: Option<TerminalGuard>,
}

impl<'a> Transport<'a> {
    fn run(mut self) -> Result<(), PlayerError> {
        self.mpris.set_status(PlaybackStatus::Playing);
        if self.interactive {
            self.ui.log(
                "[RACF] Press : for commands, space to pause, left/right to seek, q to quit",
            )?;
        }

        loop {
            loop {
                match self.receiver.try_recv() {
                    Ok(UiEvent::Prompt(prompt)) => self.ui.set_prompt(prompt),
                    Ok(UiEvent::Command(line)) => {
                        if self.handle_command(&line)? {
                            self.finish("[RACF] Stopped")?;
                            return Ok(());
                        }
                    }
                    Ok(UiEvent::Quit) => {
                        self.finish("[RACF] Stopped")?;
                        return Ok(());
                    }
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }

            let now = Instant::now();
            let position = self.position();

            if !self.paused {
                while let Some(event) = self.timeline.events.get(self.cursor).cloned() {
                    if event.timestamp_ms > position {
                        break;
                    }
                    self.dispatch(&event)?;
                    self.cursor += 1;
                }

                if position >= self.timeline.end_ms.saturating_add(TAIL_MS) {
                    if self.looping && self.timeline.end_ms > 0 {
                        self.seek_to(0);
                    } else {
                        self.finish("[RACF] Playback complete")?;
                        return Ok(());
                    }
                }
            }

            if now >= self.next_ui {
                let position = self.position();
                self.mpris.set_position(position.min(self.timeline.end_ms));
                let status = self.status_line(position);
                self.ui.render(&status)?;
                self.next_ui = now + UI_TICK;
            }

            std::thread::sleep(EVENT_TICK);
        }
    }

    fn position(&self) -> u64 {
        if self.paused {
            self.pause_position
        } else {
            self.base_ms
                .saturating_add(self.anchor.elapsed().as_millis() as u64)
        }
    }

    fn dispatch(&mut self, event: &ScheduledEvent) -> Result<(), PlayerError> {
        match &event.action {
            ScheduledAction::Play(note) => {
                self.mixer.lock().unwrap().push_note(note);
            }
            ScheduledAction::PlayAudio(audio) => {
                self.mixer.lock().unwrap().push_audio(audio);
            }
            ScheduledAction::SetSeparation { active, target } => {
                self.mixer
                    .lock()
                    .unwrap()
                    .set_separation(*active, target.clone());
                if *active {
                    self.ui.log(&format!(
                        "[RACF] Stem separation on -> isolated: {target:?}"
                    ))?;
                } else {
                    self.ui
                        .log("[RACF] Stem separation off -> normal mix restored")?;
                }
            }
        }

        Ok(())
    }

    fn seek_to(&mut self, target_ms: u64) {
        let target = target_ms.min(self.timeline.end_ms);
        let mut separation = (false, None);

        {
            let mut mixer = self.mixer.lock().unwrap();
            mixer.reset();

            for event in &self.timeline.events {
                if event.timestamp_ms > target {
                    break;
                }

                match &event.action {
                    ScheduledAction::Play(note) => {
                        let duration_ms = (note.duration_secs.max(0.0) * 1_000.0).round() as u64;
                        if target < event.timestamp_ms.saturating_add(duration_ms) {
                            let offset = target.saturating_sub(event.timestamp_ms) as f32 / 1_000.0;
                            mixer.push_note_at(note, offset);
                        }
                    }
                    ScheduledAction::PlayAudio(audio) => {
                        let duration_ms = self
                            .document
                            .audio_clips
                            .get(audio.clip_index)
                            .map(|clip| clip.duration_ms())
                            .unwrap_or(0);
                        if target < event.timestamp_ms.saturating_add(duration_ms) {
                            let offset = target.saturating_sub(event.timestamp_ms) as f32 / 1_000.0;
                            mixer.push_audio_at(audio, offset);
                        }
                    }
                    ScheduledAction::SetSeparation { active, target } => {
                        separation = (*active, target.clone());
                    }
                }
            }

            mixer.set_separation(separation.0, separation.1);
        }

        self.cursor = self
            .timeline
            .events
            .partition_point(|event| event.timestamp_ms <= target);
        self.base_ms = target;
        self.anchor = Instant::now();
        if self.paused {
            self.pause_position = target;
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if paused == self.paused {
            return;
        }

        if paused {
            self.pause_position = self.position();
            self.paused = true;
            self.mixer.lock().unwrap().set_paused(true);
            self.mpris.set_status(PlaybackStatus::Paused);
        } else {
            self.base_ms = self.pause_position;
            self.anchor = Instant::now();
            self.paused = false;
            self.mixer.lock().unwrap().set_paused(false);
            self.mpris.set_status(PlaybackStatus::Playing);
        }
    }

    fn handle_command(&mut self, line: &str) -> Result<bool, PlayerError> {
        match control::parse(line) {
            Err(message) => self.ui.log(&format!("[RACF] {message}"))?,
            Ok(Command::Quit) => return Ok(true),
            Ok(Command::Help) => self.ui.log(&control::help_text())?,
            Ok(Command::Status) => {
                let report = self.status_report();
                self.ui.log(&report)?;
            }
            Ok(Command::Volume(None)) => {
                let volume = self.mixer.lock().unwrap().master_volume_percent();
                self.ui.log(&format!("[RACF] volume: {volume:.0}%"))?;
            }
            Ok(Command::Volume(Some(change))) => {
                let volume = {
                    let mut mixer = self.mixer.lock().unwrap();
                    let current = mixer.master_volume_percent();
                    let target = match change {
                        VolumeChange::Set(value) => value,
                        VolumeChange::Delta(delta) => current + delta,
                    }
                    .clamp(0.0, 200.0);
                    mixer.set_master_volume_percent(target);
                    target
                };
                self.ui.log(&format!("[RACF] volume: {volume:.0}%"))?;
            }
            Ok(Command::Seek(request)) => {
                let current = self.position();
                let target = match request {
                    SeekRequest::Absolute(target) => target,
                    SeekRequest::Relative(delta) if delta >= 0 => {
                        current.saturating_add(delta as u64)
                    }
                    SeekRequest::Relative(delta) => current.saturating_sub(delta.unsigned_abs()),
                };
                self.seek_to(target);
                self.ui.log(&format!(
                    "[RACF] playback: {} / {}",
                    format_duration(self.position()),
                    format_duration(self.timeline.end_ms)
                ))?;
            }
            Ok(Command::Pause) => {
                self.set_paused(true);
                self.ui.log("[RACF] paused")?;
            }
            Ok(Command::Resume) => {
                self.set_paused(false);
                self.ui.log("[RACF] playing")?;
            }
            Ok(Command::TogglePause) => {
                let paused = !self.paused;
                self.set_paused(paused);
                self.ui.log(if paused {
                    "[RACF] paused"
                } else {
                    "[RACF] playing"
                })?;
            }
            Ok(Command::Eq(EqRequest::Show)) => {
                let (label, gains) = {
                    let mixer = self.mixer.lock().unwrap();
                    (mixer.eq_label(), mixer.eq_gains())
                };
                let bands = eq::BAND_LABELS
                    .iter()
                    .zip(gains.iter())
                    .map(|(band, gain)| format!("{band}:{gain:+.0}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                self.ui.log(&format!("[RACF] eq {label} | {bands}"))?;
            }
            Ok(Command::Eq(EqRequest::Off)) => {
                self.mixer.lock().unwrap().disable_eq();
                self.ui.log("[RACF] eq off")?;
            }
            Ok(Command::Eq(EqRequest::Preset(preset))) => {
                self.mixer.lock().unwrap().set_eq_preset(preset);
                self.ui
                    .log(&format!("[RACF] eq preset: {}", preset.name()))?;
            }
            Ok(Command::Eq(EqRequest::Band { index, gain_db })) => {
                if self
                    .mixer
                    .lock()
                    .unwrap()
                    .set_eq_band_gain(index, gain_db as f32)
                {
                    let band = eq::BAND_LABELS.get(index).copied().unwrap_or("?");
                    self.ui
                        .log(&format!("[RACF] eq {band} Hz: {gain_db:+} dB"))?;
                } else {
                    self.ui.log("[RACF] invalid EQ band")?;
                }
            }
            Ok(Command::Loop(None)) => {
                self.ui.log(&format!(
                    "[RACF] loop: {}",
                    if self.looping { "on" } else { "off" }
                ))?;
            }
            Ok(Command::Loop(Some(action))) => {
                self.looping = match action {
                    LoopAction::On => true,
                    LoopAction::Off => false,
                    LoopAction::Toggle => !self.looping,
                };
                self.ui.log(&format!(
                    "[RACF] loop: {}",
                    if self.looping { "on" } else { "off" }
                ))?;
            }
        }

        Ok(false)
    }

    fn status_line(&self, position: u64) -> String {
        let (volume, eq_label) = {
            let mixer = self.mixer.lock().unwrap();
            (mixer.master_volume_percent(), mixer.eq_label())
        };

        control::status_line(&StatusInfo {
            position_ms: position,
            duration_ms: self.timeline.end_ms,
            paused: self.paused,
            volume_percent: volume,
            eq_label: &eq_label,
            looping: self.looping,
        })
    }

    fn status_report(&self) -> String {
        let position = self.position();
        let (label, gains) = {
            let mixer = self.mixer.lock().unwrap();
            (mixer.eq_label(), mixer.eq_gains())
        };
        let bands = eq::BAND_LABELS
            .iter()
            .zip(gains.iter())
            .map(|(band, gain)| format!("{band}:{gain:+.0}"))
            .collect::<Vec<_>>()
            .join("  ");

        format!(
            "[RACF] {}\n{}\n[RACF] EQ {label}: {bands}",
            self.title,
            self.status_line(position),
        )
    }

    fn finish(&mut self, message: &str) -> Result<(), PlayerError> {
        self.mpris.stop();
        self.mixer.lock().unwrap().set_paused(true);
        self.ui.log(message)?;
        self.ui.finish()?;
        Ok(())
    }
}

impl Drop for Transport<'_> {
    fn drop(&mut self) {
        self.mpris.stop();
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mixer: Arc<Mutex<MasterMixer>>,
) -> Result<cpal::Stream, PlayerError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch = Vec::<f32>::new();

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if scratch.len() != data.len() {
                    scratch.resize(data.len(), 0.0);
                }

                mixer.lock().unwrap().render(&mut scratch, channels);

                for (output, sample) in data.iter_mut().zip(scratch.iter()) {
                    *output = T::from_sample(*sample);
                }
            },
            |error| eprintln!("[RACF] Audio stream error: {error}"),
            None,
        )
        .map_err(|error| PlayerError::Audio(error.to_string()))
}
