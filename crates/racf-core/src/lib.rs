//! Core types for the Readable Audio Code Factor (RACF).
//!
//! This crate owns the *format*: XML parsing, pitch/duration math, channel
//! layouts, instrument definitions, embedded sampled audio, and compilation
//! into an absolute-time event timeline. It deliberately contains no
//! audio-device or DSP code, so it can be reused by players, editors,
//! validators, and converters.

pub mod audio;
pub mod channel;
pub mod document;
pub mod duration;
pub mod error;
pub mod instrument;
pub mod metadata;
pub mod pitch;
pub mod timeline;

pub use audio::{decode_mulaw, AudioClip, SampleEncoding};
pub use channel::{Channel, ChannelLayout};
pub use document::{
    parse_document, BeatSpec, BeatStep, ChordSpec, Document, Import, LyricSpec, NoteSpec, RestSpec,
    SequenceSpec, TimeSignature, Track, TrackItem, DEFAULT_BITRATE_KBPS, DEFAULT_BIT_DEPTH,
    DEFAULT_TEMPO,
};
pub use duration::{beats_to_ms, parse_duration, parse_target_duration, parse_time_ms};
pub use error::RacfError;
pub use instrument::{DrumType, InstrumentDef, InstrumentKind, VocalParams, Waveform};
pub use metadata::Metadata;
pub use pitch::parse_pitch;
pub use timeline::{
    compile_timeline, AudioEvent, NoteEvent, ScheduledAction, ScheduledEvent, SoundSource, Timeline,
};
