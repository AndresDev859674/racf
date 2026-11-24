//! Pull-based playback engine: timeline cursor, seeking, and PCM rendering.

use std::error::Error;
use std::fmt;

use racf_core::{
    compile_timeline, parse_document, ChannelLayout, Document, Metadata, RacfError,
    ScheduledAction, Timeline,
};

use crate::eq::EqPreset;
use crate::mixer::MasterMixer;

/// Extra silence rendered after the last event so notes and embedded clips can
/// decay naturally before the stream reports EOF.
pub const TAIL_MS: u64 = 700;

/// Configuration failures that can happen before rendering starts.
#[derive(Debug)]
pub enum EngineError {
    InvalidSampleRate(u32),
    InvalidChannels(u16),
    Parse(RacfError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::InvalidSampleRate(rate) => write!(f, "invalid sample rate: {rate}"),
            EngineError::InvalidChannels(channels) => {
                write!(f, "invalid channel count: {channels}")
            }
            EngineError::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl Error for EngineError {}

/// State changes worth reporting to a host with a UI. Audio events are handled
/// internally; separation changes are also emitted here so the CLI player can
/// keep printing its status messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineNotification {
    Separation {
        active: bool,
        target: Option<String>,
    },
}

/// A document bound to an output rate and channel layout, rendered on demand.
///
/// The engine advances its own timeline cursor in locks-step with the rendered
/// samples, so callers only need to keep pulling [`RacfEngine::render_f32`].
pub struct RacfEngine {
    metadata: Metadata,
    layout: ChannelLayout,
    audio_durations_ms: Vec<u64>,
    timeline: Timeline,
    mixer: MasterMixer,
    sample_rate: u32,
    channels: usize,
    cursor: usize,
    position_frames: u64,
    looping: bool,
    paused: bool,
    finished: bool,
    notifications: Vec<EngineNotification>,
}

impl RacfEngine {
    /// Creates an engine from a parsed document, compiling the timeline.
    ///
    /// `sample_rate` and `channels` describe the *output* stream and may be any
    /// rate/channel count supported by the host. Embedded clips are resampled
    /// and the EQ/filter state is allocated accordingly.
    pub fn from_document(
        document: &Document,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, EngineError> {
        Self::build(document, sample_rate, channels)
    }

    /// Like [`Self::from_document`], but lets the caller drop the document as
    /// soon as the engine is built.
    pub fn from_owned_document(
        document: Document,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, EngineError> {
        Self::build(&document, sample_rate, channels)
    }

    fn build(document: &Document, sample_rate: u32, channels: u16) -> Result<Self, EngineError> {
        if sample_rate == 0 {
            return Err(EngineError::InvalidSampleRate(sample_rate));
        }
        if channels == 0 {
            return Err(EngineError::InvalidChannels(channels));
        }

        let timeline = compile_timeline(document);
        let mixer = MasterMixer::new(document, sample_rate as f32, channels as usize);

        // Keep only the small pieces the host may query later. Retaining the
        // whole `Document` would duplicate embedded audio bytes (and every
        // string in the score) for the lifetime of playback.
        let metadata = document.metadata.clone();
        let layout = document.channel_layout.clone();
        let audio_durations_ms = document
            .audio_clips
            .iter()
            .map(|clip| clip.duration_ms())
            .collect();

        Ok(Self {
            metadata,
            layout,
            audio_durations_ms,
            timeline,
            mixer,
            sample_rate,
            channels: channels as usize,
            cursor: 0,
            position_frames: 0,
            looping: false,
            paused: false,
            finished: false,
            notifications: Vec::new(),
        })
    }

    /// Parses RACF XML and creates an engine in one step.
    pub fn from_xml(xml: &str, sample_rate: u32, channels: u16) -> Result<Self, EngineError> {
        let document = parse_document(xml).map_err(EngineError::Parse)?;
        Self::from_owned_document(document, sample_rate, channels)
    }

    /// Renders up to `out.len() / channels` interleaved frames.
    ///
    /// Returns the number of frames actually rendered. The remainder of the
    /// buffer is zeroed, and `0` is returned once the timeline (plus its tail)
    /// has been reached. While paused, silence is returned without advancing
    /// the timeline. When looping is enabled, reaching the end restarts at 0.
    pub fn render_f32(&mut self, out: &mut [f32]) -> usize {
        let requested = out.len() / self.channels;
        if requested == 0 {
            return 0;
        }

        if self.paused {
            out.fill(0.0);
            return requested;
        }

        let mut written = 0usize;
        while written < requested {
            if self.finished {
                break;
            }

            let position_ms = self.position_ms();
            let end_ms = self.timeline.end_ms.saturating_add(TAIL_MS);
            if position_ms >= end_ms {
                if self.looping && self.timeline.end_ms > 0 {
                    self.restart();
                    continue;
                }
                self.finished = true;
                break;
            }

            self.dispatch_due(position_ms);

            let mut frames = requested - written;
            if let Some(next_event_ms) = self
                .timeline
                .events
                .get(self.cursor)
                .map(|e| e.timestamp_ms)
            {
                frames = frames.min(frames_until(position_ms, next_event_ms, self.sample_rate));
            }
            frames = frames.min(frames_until(position_ms, end_ms, self.sample_rate));
            frames = frames.max(1).min(requested - written);

            let start = written * self.channels;
            let end = start + frames * self.channels;
            self.mixer.render_f32(&mut out[start..end]);
            self.position_frames += frames as u64;
            written += frames;
        }

        let remainder = &mut out[written * self.channels..requested * self.channels];
        remainder.fill(0.0);
        written
    }

    /// Moves the playback cursor to `timestamp_ms`, restarting sounding voices
    /// partway through notes and clips just as the player did previously.
    pub fn seek(&mut self, timestamp_ms: u64) {
        let target = timestamp_ms.min(self.timeline.end_ms);
        let mut separation = (false, None);

        self.mixer.reset();
        for event in &self.timeline.events {
            if event.timestamp_ms > target {
                break;
            }

            match &event.action {
                ScheduledAction::Play(note) => {
                    let duration_ms = (note.duration_secs.max(0.0) * 1_000.0).round() as u64;
                    if target < event.timestamp_ms.saturating_add(duration_ms) {
                        let offset = (target - event.timestamp_ms) as f32 / 1_000.0;
                        self.mixer.push_note_at(note, offset);
                    }
                }
                ScheduledAction::PlayAudio(audio) => {
                    let duration_ms = self
                        .audio_durations_ms
                        .get(audio.clip_index)
                        .copied()
                        .unwrap_or(0);
                    if target < event.timestamp_ms.saturating_add(duration_ms) {
                        let offset = (target - event.timestamp_ms) as f32 / 1_000.0;
                        self.mixer.push_audio_at(audio, offset);
                    }
                }
                ScheduledAction::SetSeparation {
                    active,
                    target: name,
                } => {
                    separation = (*active, name.clone());
                }
            }
        }

        self.mixer.set_separation(separation.0, separation.1);
        self.cursor = self
            .timeline
            .events
            .partition_point(|event| event.timestamp_ms <= target);
        self.position_frames = frames_for_ms(target, self.sample_rate);
        self.finished = false;
        self.notifications.clear();
    }

    /// Current playback position, derived from rendered frames.
    pub fn position_ms(&self) -> u64 {
        self.position_frames
            .saturating_mul(1_000)
            .checked_div(self.sample_rate as u64)
            .unwrap_or(0)
    }

    pub fn duration_ms(&self) -> u64 {
        self.timeline.end_ms
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn layout(&self) -> &ChannelLayout {
        &self.layout
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.mixer.set_paused(paused);
    }

    pub fn is_looping(&self) -> bool {
        self.looping
    }

    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn drain_notifications(&mut self) -> Vec<EngineNotification> {
        std::mem::take(&mut self.notifications)
    }

    // Mixer controls shared with the CLI player (the C API only needs the
    // defaults, but exposing them keeps the engine useful on its own).

    pub fn set_master_volume_percent(&mut self, percent: f32) {
        self.mixer.set_master_volume_percent(percent);
    }

    pub fn master_volume_percent(&self) -> f32 {
        self.mixer.master_volume_percent()
    }

    pub fn set_eq_preset(&mut self, preset: EqPreset) {
        self.mixer.set_eq_preset(preset);
    }

    pub fn set_eq_band_gain(&mut self, index: usize, gain_db: f32) -> bool {
        self.mixer.set_eq_band_gain(index, gain_db)
    }

    pub fn disable_eq(&mut self) {
        self.mixer.disable_eq();
    }

    pub fn eq_label(&self) -> String {
        self.mixer.eq_label()
    }

    pub fn eq_gains(&self) -> [f32; 10] {
        self.mixer.eq_gains()
    }

    fn dispatch_due(&mut self, position_ms: u64) {
        while let Some(event) = self.timeline.events.get(self.cursor).cloned() {
            if event.timestamp_ms > position_ms {
                break;
            }

            match event.action {
                ScheduledAction::Play(note) => self.mixer.push_note(&note),
                ScheduledAction::PlayAudio(audio) => self.mixer.push_audio(&audio),
                ScheduledAction::SetSeparation { active, target } => {
                    self.mixer.set_separation(active, target.clone());
                    self.notifications
                        .push(EngineNotification::Separation { active, target });
                }
            }
            self.cursor += 1;
        }
    }

    fn restart(&mut self) {
        self.seek(0);
        self.finished = false;
    }
}

/// Number of output frames needed to advance by `target_ms - position_ms`.
fn frames_until(position_ms: u64, target_ms: u64, sample_rate: u32) -> usize {
    if target_ms <= position_ms {
        return 0;
    }

    let frames = (target_ms - position_ms)
        .saturating_mul(sample_rate as u64)
        .saturating_add(999)
        / 1_000;
    frames.min(usize::MAX as u64) as usize
}

fn frames_for_ms(ms: u64, sample_rate: u32) -> u64 {
    ms.saturating_mul(sample_rate as u64).saturating_add(999) / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = r#"
        <racf channelLayout="stereo">
            <score>
                <track instrument="Lead">
                    <note pitch="C4" duration="1/1" />
                </track>
            </score>
        </racf>
    "#;

    #[test]
    fn rejects_invalid_output_configuration() {
        assert!(matches!(
            RacfEngine::from_xml(NOTE, 0, 2),
            Err(EngineError::InvalidSampleRate(0))
        ));
        assert!(matches!(
            RacfEngine::from_xml(NOTE, 48_000, 0),
            Err(EngineError::InvalidChannels(0))
        ));
    }

    #[test]
    fn renders_interleaved_frames_at_the_requested_rate_and_layout() {
        let mut engine = RacfEngine::from_xml(NOTE, 8_000, 3).unwrap();
        let mut output = vec![0.0f32; 100 * 3];

        let frames = engine.render_f32(&mut output);

        assert_eq!(frames, 100);
        assert_eq!(engine.sample_rate(), 8_000);
        assert_eq!(engine.channels(), 3);
        assert_eq!(engine.position_ms(), 12);
        assert!(output[..100 * 3].iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn dispatch_and_position_follow_rendered_samples() {
        let mut engine = RacfEngine::from_xml(
            r#"
            <racf tempo="60">
                <score>
                    <track instrument="Lead">
                        <note pitch="C4" duration="1/8" />
                        <rest duration="1/8" />
                        <note pitch="E4" duration="1/8" />
                    </track>
                </score>
            </racf>
            "#,
            1_000,
            1,
        )
        .unwrap();

        let mut block = vec![0.0f32; 2_000]; // 2 s at 1 kHz.
        assert_eq!(engine.render_f32(&mut block), 2_000);
        assert_eq!(engine.position_ms(), 2_000);

        // A 1/8 note and rest at 60 BPM are 500 ms each.
        assert!(block[..500].iter().any(|sample| *sample != 0.0));
        assert!(block[500..1_000].iter().all(|sample| *sample == 0.0));
        assert!(block[1_000..].iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn seek_restarts_at_the_requested_position() {
        let mut engine = RacfEngine::from_xml(NOTE, 1_000, 1).unwrap();
        engine.seek(500);

        assert_eq!(engine.position_ms(), 500);
        let mut block = [0.0f32; 10];
        assert_eq!(engine.render_f32(&mut block), 10);
        assert_eq!(engine.position_ms(), 510);
        assert!(block.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn paused_renders_silence_without_advancing() {
        let mut engine = RacfEngine::from_xml(NOTE, 1_000, 1).unwrap();
        engine.set_paused(true);

        let mut block = [1.0f32; 32];
        assert_eq!(engine.render_f32(&mut block), 32);
        assert!(block.iter().all(|sample| *sample == 0.0));
        assert_eq!(engine.position_ms(), 0);
    }

    #[test]
    fn rendering_stops_at_the_end_of_the_timeline() {
        let mut engine = RacfEngine::from_xml(
            r#"<racf tempo="60000"><score><track instrument="x"><note pitch="C4" duration="1/4" /></track></score></racf>"#,
            1_000,
            1,
        )
        .unwrap();
        assert_eq!(engine.duration_ms(), 1);

        let mut block = [0.0f32; 64];
        let mut total = 0;
        while engine.render_f32(&mut block) > 0 {
            total += 64;
            assert!(total < 1_000, "engine never reached EOF");
        }
        assert!(engine.is_finished());
        assert!(total >= (1 + TAIL_MS) as usize);
    }

    #[test]
    fn looping_restarts_instead_of_finishing() {
        let mut engine = RacfEngine::from_xml(
            r#"<racf tempo="60000"><score><track instrument="x"><note pitch="C4" duration="1/4" /></track></score></racf>"#,
            1_000,
            1,
        )
        .unwrap();
        engine.set_looping(true);

        for _ in 0..20 {
            let mut block = [0.0f32; 64];
            assert_eq!(engine.render_f32(&mut block), 64);
        }
        assert!(!engine.is_finished());
        assert!(engine.position_ms() < engine.duration_ms() + TAIL_MS);
    }
}
