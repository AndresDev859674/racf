//! Audio player and DSP engines for RACF.
//!
//! The player is intentionally separate from `racf-core`: core decides *what*
//! should sound and *when*, while this crate decides *how* to synthesize and
//! route it to the output device.

pub mod audio;
pub mod dsp;
pub mod error;
pub mod mixer;
pub mod mpris;
pub mod sample;
pub mod voice;

pub use audio::play;
pub use error::PlayerError;
pub use mixer::MasterMixer;
pub use sample::{PreparedClip, SampleVoice};
pub use voice::Voice;
