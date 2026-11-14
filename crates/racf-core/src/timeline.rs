use crate::channel::Channel;
use crate::document::{Document, Track, TrackItem};
use crate::duration::beats_to_ms;
use crate::instrument::{DrumType, InstrumentDef, InstrumentKind, VocalParams, Waveform};
use crate::pitch::parse_pitch;

/// The sound engine a compiled note should be rendered with.
#[derive(Clone, Debug, PartialEq)]
pub enum SoundSource {
    Oscillator { waveform: Waveform, distortion: f32 },
    Piano,
    Synth,
    Trumpet,
    Otamatone,
    Scratch,
    Drum { kind: DrumType, electronic: bool },
    Vocal(VocalParams),
}

/// A single compiled note/event, independent of any audio device.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteEvent {
    /// Track identity as written in `instrument="..."`.
    pub track_id: String,
    pub frequency: f32,
    pub duration_secs: f32,
    pub gain: f32,
    pub pan: f32,
    /// When set, the note is routed to this exact channel instead of using
    /// the pan position.
    pub channel: Option<Channel>,
    pub source: SoundSource,
}

/// A scheduled playback of an embedded sampled-audio clip.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioEvent {
    /// Index into [`Document::audio_clips`].
    pub clip_index: usize,
    /// Clip identity, kept for diagnostics.
    pub clip_id: String,
    pub gain: f32,
    pub pan: f32,
    pub channel: Option<Channel>,
}

#[derive(Clone, Debug)]
pub enum ScheduledAction {
    Play(NoteEvent),
    PlayAudio(AudioEvent),
    SetSeparation {
        active: bool,
        target: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct ScheduledEvent {
    pub timestamp_ms: u64,
    pub action: ScheduledAction,
}

#[derive(Clone, Debug)]
pub struct Timeline {
    pub events: Vec<ScheduledEvent>,
    /// The base tempo after applying `targetDuration`.
    pub effective_tempo: f32,
    /// Approximate end of the last sounding note, in milliseconds.
    pub end_ms: u64,
}

/// Compiles a parsed document into absolute-time playback events.
pub fn compile_timeline(document: &Document) -> Timeline {
    let effective_tempo = document.effective_tempo();
    let mut events = Vec::new();

    for track in &document.tracks {
        compile_track(document, track, effective_tempo, &mut events);
    }

    for (clip_index, clip) in document.audio_clips.iter().enumerate() {
        events.push(ScheduledEvent {
            timestamp_ms: clip.at_ms,
            action: ScheduledAction::PlayAudio(AudioEvent {
                clip_index,
                clip_id: clip.id.clone(),
                gain: clip.gain,
                pan: clip.pan,
                channel: clip.channel,
            }),
        });
    }

    events.sort_by_key(|event| event.timestamp_ms);

    let end_ms = events
        .iter()
        .map(|event| match &event.action {
            ScheduledAction::Play(note) => {
                event.timestamp_ms + (note.duration_secs.max(0.0) * 1000.0).round() as u64
            }
            ScheduledAction::PlayAudio(audio) => {
                event.timestamp_ms
                    + document
                        .audio_clips
                        .get(audio.clip_index)
                        .map(|clip| clip.duration_ms())
                        .unwrap_or(0)
            }
            ScheduledAction::SetSeparation { .. } => event.timestamp_ms,
        })
        .max()
        .unwrap_or(0);

    Timeline {
        events,
        effective_tempo,
        end_ms,
    }
}

fn compile_track(
    document: &Document,
    track: &Track,
    initial_tempo: f32,
    events: &mut Vec<ScheduledEvent>,
) {
    let instrument = document.resolve_instrument(&track.instrument);
    let mut time_ms = 0.0f32;
    let mut bpm = initial_tempo;
    let mut force_gain = track.gain;
    let mut fades = FadeState::default();

    for item in &track.items {
        match item {
            TrackItem::Tempo(value) => bpm = *value,
            TrackItem::Gain(value) => force_gain = *value,
            TrackItem::FadeIn { beats } => {
                fades.fade_in = Some((time_ms, time_ms + beats_to_ms(*beats, bpm)));
            }
            TrackItem::FadeOut { beats } => {
                fades.fade_out = Some((time_ms, time_ms + beats_to_ms(*beats, bpm)));
            }
            TrackItem::Separation { active, target } => {
                push_event(
                    events,
                    time_ms,
                    ScheduledAction::SetSeparation {
                        active: *active,
                        target: target.clone(),
                    },
                );
            }
            TrackItem::Note(note) => {
                let duration_ms = beats_to_ms(note.beats, note.bpm.unwrap_or(bpm));
                let event = NoteEvent {
                    track_id: track.instrument.clone(),
                    frequency: parse_pitch(&note.pitch),
                    duration_secs: duration_ms / 1000.0,
                    gain: force_gain * fades.multiplier(time_ms) * note.velocity,
                    pan: note.pan.unwrap_or(track.pan),
                    channel: note.channel.or(track.channel),
                    source: source_for_instrument(&instrument),
                };
                push_event(events, time_ms, ScheduledAction::Play(event));
                time_ms += duration_ms;
            }
            TrackItem::Lyric(lyric) => {
                let duration_ms = beats_to_ms(lyric.beats, lyric.bpm.unwrap_or(bpm));
                let event = NoteEvent {
                    track_id: track.instrument.clone(),
                    frequency: parse_pitch(&lyric.pitch),
                    duration_secs: duration_ms / 1000.0,
                    gain: force_gain * fades.multiplier(time_ms) * lyric.velocity,
                    pan: lyric.pan.unwrap_or(track.pan),
                    channel: lyric.channel.or(track.channel),
                    source: SoundSource::Vocal(lyric.vocal.clone()),
                };
                push_event(events, time_ms, ScheduledAction::Play(event));
                time_ms += duration_ms;
            }
            TrackItem::Chord(chord) => {
                let duration_ms = beats_to_ms(chord.beats, chord.bpm.unwrap_or(bpm));
                let gain = force_gain * fades.multiplier(time_ms) * chord.velocity;

                for pitch in &chord.pitches {
                    let event = NoteEvent {
                        track_id: track.instrument.clone(),
                        frequency: parse_pitch(pitch),
                        duration_secs: duration_ms / 1000.0,
                        gain,
                        pan: chord.pan.unwrap_or(track.pan),
                        channel: chord.channel.or(track.channel),
                        source: source_for_instrument(&instrument),
                    };
                    push_event(events, time_ms, ScheduledAction::Play(event));
                }

                time_ms += duration_ms;
            }
            TrackItem::Rest(rest) => {
                time_ms += beats_to_ms(rest.beats, rest.bpm.unwrap_or(bpm));
            }
            TrackItem::Sequence(sequence) => {
                let sequence_bpm = sequence.bpm.unwrap_or(bpm);

                for note in &sequence.notes {
                    let duration_ms = beats_to_ms(note.beats, sequence_bpm);
                    let event = NoteEvent {
                        track_id: track.instrument.clone(),
                        frequency: parse_pitch(&note.pitch),
                        duration_secs: duration_ms / 1000.0,
                        gain: force_gain * fades.multiplier(time_ms) * note.velocity,
                        pan: note.pan.unwrap_or(track.pan),
                        channel: note.channel.or(track.channel),
                        source: source_for_instrument(&instrument),
                    };
                    push_event(events, time_ms, ScheduledAction::Play(event));
                    time_ms += duration_ms;
                }
            }
            TrackItem::Beat(beat) => {
                let beat_bpm = beat.bpm.unwrap_or(bpm);
                let electronic = instrument.kind == InstrumentKind::ElectroDrum;

                for step in &beat.steps {
                    let duration_ms = beats_to_ms(step.beats, beat_bpm);
                    let gain = force_gain * fades.multiplier(time_ms);

                    for drum in &step.drums {
                        let event = NoteEvent {
                            track_id: track.instrument.clone(),
                            frequency: 0.0,
                            duration_secs: 0.6,
                            gain,
                            pan: step.pan.unwrap_or(track.pan),
                            channel: beat.channel.or(track.channel),
                            source: SoundSource::Drum {
                                kind: *drum,
                                electronic,
                            },
                        };
                        push_event(events, time_ms, ScheduledAction::Play(event));
                    }

                    time_ms += duration_ms;
                }
            }
        }
    }
}

fn source_for_instrument(instrument: &InstrumentDef) -> SoundSource {
    match instrument.kind {
        InstrumentKind::Oscillator | InstrumentKind::ElectroDrum => SoundSource::Oscillator {
            waveform: instrument.waveform,
            distortion: instrument.distortion,
        },
        InstrumentKind::Piano => SoundSource::Piano,
        InstrumentKind::Synth => SoundSource::Synth,
        InstrumentKind::Trumpet => SoundSource::Trumpet,
        InstrumentKind::Otamatone => SoundSource::Otamatone,
        InstrumentKind::Scratch => SoundSource::Scratch,
        InstrumentKind::Vocal => SoundSource::Vocal(instrument.vocal.clone().unwrap_or_default()),
    }
}

fn push_event(events: &mut Vec<ScheduledEvent>, time_ms: f32, action: ScheduledAction) {
    events.push(ScheduledEvent {
        timestamp_ms: time_ms.max(0.0).round() as u64,
        action,
    });
}

#[derive(Clone, Debug, Default)]
struct FadeState {
    fade_in: Option<(f32, f32)>,
    fade_out: Option<(f32, f32)>,
}

impl FadeState {
    fn multiplier(&self, time_ms: f32) -> f32 {
        if let Some((start, end)) = self.fade_in {
            if end > start && time_ms >= start && time_ms < end {
                return (0.15 + 0.85 * ((time_ms - start) / (end - start))).clamp(0.0, 1.0);
            }
        }

        if let Some((start, end)) = self.fade_out {
            if end > start && time_ms >= start && time_ms < end {
                return (1.0 - ((time_ms - start) / (end - start))).clamp(0.0, 1.0);
            }
        }

        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse_document;

    const DOCUMENT: &str = r#"
        <racf tempo="120" channelLayout="stereo">
            <definitions>
                <instrument id="Lead">
                    <oscillator type="square" />
                    <effect type="distortion" drive="4.5" />
                </instrument>
            </definitions>
            <score>
                <track instrument="Lead" gain="0.5" pan="-0.6" channel="left">
                    <note pitch="C4" duration="1/4" velocity="0.5" />
                    <rest duration="1/4" />
                    <chord pitch="C4,E4,G4" duration="1/2" />
                    <sequence pattern="D4:1/8, E4:1/8" />
                    <beat pattern="KICK/SNARE:1/4" />
                </track>
            </score>
        </racf>
    "#;

    fn notes(timeline: &Timeline) -> Vec<(u64, &NoteEvent)> {
        timeline
            .events
            .iter()
            .filter_map(|event| match &event.action {
                ScheduledAction::Play(note) => Some((event.timestamp_ms, note)),
                ScheduledAction::PlayAudio(_) | ScheduledAction::SetSeparation { .. } => None,
            })
            .collect()
    }

    #[test]
    fn compiles_notes_rests_chords_sequences_and_drums() {
        let document = parse_document(DOCUMENT).unwrap();
        let timeline = compile_timeline(&document);
        let notes = notes(&timeline);

        assert_eq!(notes.len(), 8);
        assert_eq!(notes[0].0, 0);
        assert_eq!(notes[1].0, 1000);
        assert_eq!(notes[3].0, 1000);
        assert_eq!(notes[4].0, 2000);
        assert_eq!(notes[5].0, 2250);
        assert_eq!(notes[6].0, 2500);
        assert_eq!(notes[7].0, 2500);
        assert_eq!(timeline.end_ms, 3100);
    }

    #[test]
    fn applies_track_gain_velocity_pan_and_channel() {
        let document = parse_document(DOCUMENT).unwrap();
        let timeline = compile_timeline(&document);
        let first = notes(&timeline)[0].1;

        assert!((first.gain - 0.25).abs() < f32::EPSILON);
        assert!((first.pan - -0.6).abs() < f32::EPSILON);
        assert_eq!(first.channel, Some(Channel::FrontLeft));
        assert_eq!(
            first.source,
            SoundSource::Oscillator {
                waveform: Waveform::Square,
                distortion: 4.5
            }
        );
    }

    #[test]
    fn electronic_drum_tracks_use_the_electro_engine() {
        let xml = r#"
            <racf tempo="120">
                <imports>
                    <import src="import::electrodrums" alias="Drums" />
                </imports>
                <score>
                    <track instrument="Drums">
                        <beat pattern="KICK:1/4" />
                    </track>
                </score>
            </racf>
        "#;

        let document = parse_document(xml).unwrap();
        let timeline = compile_timeline(&document);
        let note = notes(&timeline)[0].1;

        assert_eq!(
            note.source,
            SoundSource::Drum {
                kind: DrumType::Kick,
                electronic: true
            }
        );
        assert_eq!(note.duration_secs, 0.6);
    }

    #[test]
    fn compiles_embedded_audio_into_absolute_events() {
        use base64::Engine as _;

        let data = base64::engine::general_purpose::STANDARD.encode([0xffu8; 8000]);
        let xml = format!(
            r#"
            <racf>
                <audio>
                    <clip id="main" at="1s" sampleRate="8000" channels="1" encoding="mulaw" gain="0.8">
                        <data>{data}</data>
                    </clip>
                </audio>
            </racf>
            "#
        );

        let document = parse_document(&xml).unwrap();
        let timeline = compile_timeline(&document);

        assert_eq!(timeline.events.len(), 1);
        assert_eq!(timeline.events[0].timestamp_ms, 1000);
        assert_eq!(timeline.end_ms, 2000);

        let ScheduledAction::PlayAudio(audio) = &timeline.events[0].action else {
            panic!("expected an audio event");
        };
        assert_eq!(audio.clip_index, 0);
        assert_eq!(audio.clip_id, "main");
        assert!((audio.gain - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn fades_scale_note_gain_over_time() {
        let xml = r#"
            <racf tempo="60">
                <score>
                    <track instrument="x" gain="1.0">
                        <fade-in duration="1/1" />
                        <note pitch="C4" duration="1/4" />
                        <note pitch="C4" duration="1/4" />
                    </track>
                </score>
            </racf>
        "#;

        let document = parse_document(xml).unwrap();
        let timeline = compile_timeline(&document);
        let notes = notes(&timeline);

        assert!((notes[0].1.gain - 0.15).abs() < 0.001);
        assert!((notes[1].1.gain - 0.3625).abs() < 0.001);
    }
}
