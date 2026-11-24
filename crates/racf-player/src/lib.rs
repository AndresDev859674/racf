//! Audio player front-end for RACF.
//!
//! Synthesis, timeline tracking, and PCM rendering live in the hardware-agnostic
//! `racf-engine` crate. This crate adds the CLI transport (cpal output, terminal
//! UI, MPRIS, and command parsing) and re-exports the engine types for
//! compatibility with existing users.

pub mod audio;
pub mod control;
pub mod dsp;
pub mod eq;
pub mod error;
pub mod mixer;
pub mod mpris;
pub mod report;
pub mod sample;
pub mod ui;
pub mod voice;

pub use audio::{play, PlaybackOptions};
pub use control::Command;
pub use eq::{EqPreset, GraphicEq};
pub use error::PlayerError;
pub use mixer::MasterMixer;
pub use mpris::PlaybackStatus;
pub use racf_engine::{EngineNotification, RacfEngine};
pub use sample::{PreparedClip, SampleVoice};
pub use voice::Voice;
