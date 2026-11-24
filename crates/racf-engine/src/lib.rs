//! Hardware-agnostic synthesis and playback engine for RACF documents.
//!
//! This crate owns everything needed to turn a compiled RACF timeline into
//! interleaved `f32` PCM: voices, embedded-audio playback, the master mixer,
//! the timeline cursor, and seeking. It has no dependency on audio devices,
//! command-line parsing, or desktop services, so the same renderer can be
//! driven by the CLI player, an audio plugin, or a C FFI host.

pub mod dsp;
pub mod engine;
pub mod eq;
pub mod mixer;
pub mod sample;
pub mod voice;

pub use dsp::{noise_bipolar, rand_noise, ResonantFilter, VocalFormants};
pub use engine::{EngineError, EngineNotification, RacfEngine, TAIL_MS};
pub use eq::{band_index, EqPreset, GraphicEq, BAND_FREQUENCIES, BAND_LABELS, MAX_BAND_GAIN_DB};
pub use mixer::MasterMixer;
pub use sample::{PreparedClip, SampleVoice};
pub use voice::Voice;
