use std::fmt;

/// Errors produced by the player/audio backend.
#[derive(Debug)]
pub enum PlayerError {
    NoOutputDevice,
    Audio(String),
    UnsupportedSampleFormat(String),
    Engine(String),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlayerError::NoOutputDevice => f.write_str("no audio output device found"),
            PlayerError::Audio(message) => write!(f, "audio error: {message}"),
            PlayerError::UnsupportedSampleFormat(format) => {
                write!(f, "unsupported output sample format: {format}")
            }
            PlayerError::Engine(message) => write!(f, "playback engine error: {message}"),
        }
    }
}

impl std::error::Error for PlayerError {}

impl From<std::io::Error> for PlayerError {
    fn from(error: std::io::Error) -> Self {
        PlayerError::Audio(error.to_string())
    }
}
