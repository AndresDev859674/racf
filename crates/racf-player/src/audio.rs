//! cpal output and interactive transport for a RACF document.
//!
//! All synthesis and timeline tracking is performed by `racf-engine`; this
//! module only drives it from the audio callback and the terminal UI.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use racf_core::{Document, Timeline};
use racf_engine::{EngineNotification, RacfEngine};

use crate::control::{
    self, format_duration, Command, EqRequest, LoopAction, SeekRequest, StatusInfo, VolumeChange,
};
use crate::eq::{self, EqPreset};
use crate::error::PlayerError;
use crate::mpris::{MprisHandle, PlaybackStatus};
use crate::ui::{self, TerminalGuard, Ui, UiEvent};

const EVENT_TICK: Duration = Duration::from_millis(5);
const UI_TICK: Duration = Duration::from_millis(100);

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
    let sample_rate = config.sample_rate.0;
    let device_channels = config.channels as usize;

    let engine = Arc::new(Mutex::new(
        RacfEngine::from_document(document, sample_rate, device_channels as u16)
            .map_err(|error| PlayerError::Engine(error.to_string()))?,
    ));
    {
        let mut guard = engine.lock().unwrap();
        guard.set_master_volume_percent(options.volume_percent);
        if let Some(preset) = options.eq {
            guard.set_eq_preset(preset);
        }
        guard.set_looping(options.looping);
    }

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&engine)),
        SampleFormat::F64 => build_stream::<f64>(&device, &config, Arc::clone(&engine)),
        SampleFormat::I8 => build_stream::<i8>(&device, &config, Arc::clone(&engine)),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&engine)),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, Arc::clone(&engine)),
        SampleFormat::I64 => build_stream::<i64>(&device, &config, Arc::clone(&engine)),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, Arc::clone(&engine)),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&engine)),
        SampleFormat::U32 => build_stream::<u32>(&device, &config, Arc::clone(&engine)),
        SampleFormat::U64 => build_stream::<u64>(&device, &config, Arc::clone(&engine)),
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
        engine.lock().unwrap().document().channel_layout
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
        engine,
        mpris,
        ui: Ui::new(interactive),
        receiver,
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
    engine: Arc<Mutex<RacfEngine>>,
    mpris: MprisHandle,
    ui: Ui,
    receiver: Receiver<UiEvent>,
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

            // Separation events are dispatched by the engine when their
            // timestamp is reached; forward them to the status log here.
            let notifications = self.engine.lock().unwrap().drain_notifications();
            for notification in notifications {
                match notification {
                    EngineNotification::Separation { active, target } => {
                        if active {
                            self.ui.log(&format!(
                                "[RACF] Stem separation on -> isolated: {target:?}"
                            ))?;
                        } else {
                            self.ui
                                .log("[RACF] Stem separation off -> normal mix restored")?;
                        }
                    }
                }
            }

            let (position, finished) = {
                let engine = self.engine.lock().unwrap();
                (engine.position_ms(), engine.is_finished())
            };

            if finished {
                self.finish("[RACF] Playback complete")?;
                return Ok(());
            }

            let now = Instant::now();
            if now >= self.next_ui {
                self.mpris.set_position(position.min(self.timeline.end_ms));
                let status = self.status_line(position);
                let lyrics = self.lyrics_lines(position);
                self.ui.render(&status, &lyrics)?;
                self.next_ui = now + UI_TICK;
            }

            std::thread::sleep(EVENT_TICK);
        }
    }

    fn position(&self) -> u64 {
        self.engine.lock().unwrap().position_ms()
    }

    fn seek_to(&mut self, target_ms: u64) {
        self.engine.lock().unwrap().seek(target_ms);
    }

    fn set_paused(&mut self, paused: bool) {
        if paused == self.engine.lock().unwrap().is_paused() {
            return;
        }

        self.engine.lock().unwrap().set_paused(paused);
        self.mpris.set_status(if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        });
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
                let volume = self.engine.lock().unwrap().master_volume_percent();
                self.ui.log(&format!("[RACF] volume: {volume:.0}%"))?;
            }
            Ok(Command::Volume(Some(change))) => {
                let volume = {
                    let mut engine = self.engine.lock().unwrap();
                    let current = engine.master_volume_percent();
                    let target = match change {
                        VolumeChange::Set(value) => value,
                        VolumeChange::Delta(delta) => current + delta,
                    }
                    .clamp(0.0, 200.0);
                    engine.set_master_volume_percent(target);
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
                let paused = !self.engine.lock().unwrap().is_paused();
                self.set_paused(paused);
                self.ui.log(if paused {
                    "[RACF] paused"
                } else {
                    "[RACF] playing"
                })?;
            }
            Ok(Command::Eq(EqRequest::Show)) => {
                let (label, gains) = {
                    let engine = self.engine.lock().unwrap();
                    (engine.eq_label(), engine.eq_gains())
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
                self.engine.lock().unwrap().disable_eq();
                self.ui.log("[RACF] eq off")?;
            }
            Ok(Command::Eq(EqRequest::Preset(preset))) => {
                self.engine.lock().unwrap().set_eq_preset(preset);
                self.ui
                    .log(&format!("[RACF] eq preset: {}", preset.name()))?;
            }
            Ok(Command::Eq(EqRequest::Band { index, gain_db })) => {
                if self
                    .engine
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
                let looping = self.engine.lock().unwrap().is_looping();
                self.ui.log(&format!(
                    "[RACF] loop: {}",
                    if looping { "on" } else { "off" }
                ))?;
            }
            Ok(Command::Loop(Some(action))) => {
                let looping = match action {
                    LoopAction::On => true,
                    LoopAction::Off => false,
                    LoopAction::Toggle => !self.engine.lock().unwrap().is_looping(),
                };
                self.engine.lock().unwrap().set_looping(looping);
                self.ui.log(&format!(
                    "[RACF] loop: {}",
                    if looping { "on" } else { "off" }
                ))?;
            }
        }

        Ok(false)
    }

    fn status_line(&self, position: u64) -> String {
        let (volume, eq_label) = {
            let engine = self.engine.lock().unwrap();
            (engine.master_volume_percent(), engine.eq_label())
        };

        control::status_line(&StatusInfo {
            position_ms: position,
            duration_ms: self.timeline.end_ms,
            paused: self.engine.lock().unwrap().is_paused(),
            volume_percent: volume,
            eq_label: &eq_label,
            looping: self.engine.lock().unwrap().is_looping(),
        })
    }

    /// Lines shown directly below the transport status.
    ///
    /// Timed lyrics follow playback and show the active line. Untimed lyrics
    /// are shown as a static block so plain-text lyric files remain readable.
    fn lyrics_lines(&self, position: u64) -> Vec<String> {
        let Some(lyrics) = self.document.lyrics.as_ref() else {
            return Vec::new();
        };

        if lyrics.is_timed() {
            let Some(line) = lyrics.active_line(position) else {
                return Vec::new();
            };
            return line
                .text
                .lines()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(|text| format!("[RACF] ♪ {text}"))
                .collect();
        }

        lyrics
            .lines
            .iter()
            .flat_map(|line| line.text.lines())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| format!("[RACF]   {text}"))
            .collect()
    }

    fn status_report(&self) -> String {
        let position = self.position();
        let (paused, label, gains) = {
            let engine = self.engine.lock().unwrap();
            (engine.is_paused(), engine.eq_label(), engine.eq_gains())
        };
        let bands = eq::BAND_LABELS
            .iter()
            .zip(gains.iter())
            .map(|(band, gain)| format!("{band}:{gain:+.0}"))
            .collect::<Vec<_>>()
            .join("  ");

        let status = control::status_line(&StatusInfo {
            position_ms: position,
            duration_ms: self.timeline.end_ms,
            paused,
            volume_percent: self.engine.lock().unwrap().master_volume_percent(),
            eq_label: &label,
            looping: self.engine.lock().unwrap().is_looping(),
        });

        format!(
            "[RACF] {}\n{}\n[RACF] EQ {label}: {bands}",
            self.title, status
        )
    }

    fn finish(&mut self, message: &str) -> Result<(), PlayerError> {
        self.mpris.stop();
        self.engine.lock().unwrap().set_paused(true);
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
    engine: Arc<Mutex<RacfEngine>>,
) -> Result<cpal::Stream, PlayerError>
where
    T: SizedSample + FromSample<f32>,
{
    let mut scratch = Vec::<f32>::new();

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if scratch.len() != data.len() {
                    scratch.resize(data.len(), 0.0);
                }

                engine.lock().unwrap().render_f32(&mut scratch);

                for (output, sample) in data.iter_mut().zip(scratch.iter()) {
                    *output = T::from_sample(*sample);
                }
            },
            |error| eprintln!("[RACF] Audio stream error: {error}"),
            None,
        )
        .map_err(|error| PlayerError::Audio(error.to_string()))
}
