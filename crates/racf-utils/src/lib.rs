//! Conversion and inspection helpers for RACF documents.
//!
//! The converter embeds decoded audio rather than linking to the source file,
//! so a converted document is portable. File size is controlled with the
//! sample rate, channel count, and encoding options.

pub mod decode;
pub mod encode;
pub mod writer;

use std::path::Path;

use anyhow::{bail, Context, Result};
use racf_core::{Metadata, SampleEncoding};

use crate::decode::Tags;

/// Options controlling the audio payload written into the RACF file.
#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub sample_rate: u32,
    pub channels: u8,
    pub encoding: SampleEncoding,
    /// Metadata supplied on the command line. Missing fields fall back to tags
    /// in the source file and then to the input filename.
    pub metadata: Metadata,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            sample_rate: 22_050,
            channels: 1,
            encoding: SampleEncoding::Mulaw,
            metadata: Metadata::default(),
        }
    }
}

/// Result of a successful conversion.
pub struct Conversion {
    pub xml: String,
    pub metadata: Metadata,
    pub tags: Tags,
    pub source_sample_rate: u32,
    pub source_channels: usize,
    pub source_duration_secs: f32,
    pub output_duration_secs: f32,
    pub data_bytes: usize,
}

/// Decodes `input` and builds the XML for a new RACF document.
pub fn convert_file(input: &Path, options: &ConvertOptions) -> Result<Conversion> {
    if !(4_000..=96_000).contains(&options.sample_rate) {
        bail!(
            "sample rate must be between 4000 and 96000 Hz, got {}",
            options.sample_rate
        );
    }

    if !(1..=2).contains(&options.channels) {
        bail!("channels must be 1 or 2, got {}", options.channels);
    }

    let decoded = decode::decode_file(input)?;
    let source_duration_secs = decoded.duration_secs();

    let downmixed = decode::downmix(
        &decoded.samples,
        decoded.channels,
        options.channels as usize,
    );
    let resampled = decode::resample(
        &downmixed,
        decoded.sample_rate,
        options.sample_rate,
        options.channels as usize,
    );

    if resampled.is_empty() {
        bail!("the converted audio is empty");
    }

    let data = encode::encode(&resampled, options.encoding);
    let metadata = merge_metadata(&options.metadata, &decoded.tags, input);

    let clip = writer::ClipSpec {
        id: "main",
        at_ms: 0,
        gain: 1.0,
        pan: 0.0,
        channel: None,
        sample_rate: options.sample_rate,
        channels: options.channels,
        encoding: options.encoding,
        data: &data,
    };
    let xml = writer::render_document(&metadata, &clip);

    let output_duration_secs =
        resampled.len() as f32 / (options.sample_rate as f32 * options.channels as f32);

    Ok(Conversion {
        xml,
        metadata,
        tags: decoded.tags,
        source_sample_rate: decoded.sample_rate,
        source_channels: decoded.channels,
        source_duration_secs,
        output_duration_secs,
        data_bytes: data.len(),
    })
}

fn merge_metadata(cli: &Metadata, tags: &Tags, input: &Path) -> Metadata {
    Metadata {
        title: cli
            .title
            .clone()
            .or_else(|| tags.title.clone())
            .or_else(|| {
                input
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .filter(|title| !title.trim().is_empty()),
        description: cli
            .description
            .clone()
            .or_else(|| tags.description.clone())
            .filter(|description| !description.trim().is_empty()),
        artist: cli
            .artist
            .clone()
            .or_else(|| tags.artist.clone())
            .filter(|artist| !artist.trim().is_empty()),
        album: cli
            .album
            .clone()
            .or_else(|| tags.album.clone())
            .filter(|album| !album.trim().is_empty()),
    }
}

/// Writes a converted document, reporting clear filesystem context on error.
pub fn write_document(path: &Path, xml: &str) -> Result<()> {
    std::fs::write(path, xml)
        .with_context(|| format!("could not write output file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use racf_core::{parse_document, SampleEncoding};

    fn write_test_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[i16]) {
        let data_len = (samples.len() * 2) as u32;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn converts_wav_into_a_parsable_racf_document() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("tune.wav");
        let samples: Vec<i16> = (0..8_000)
            .map(|frame| {
                let phase = frame as f32 / 8_000.0 * 440.0 * std::f32::consts::TAU;
                (phase.sin() * 12_000.0) as i16
            })
            .collect();
        write_test_wav(&input, 8_000, 1, &samples);

        let options = ConvertOptions {
            sample_rate: 8_000,
            channels: 1,
            encoding: SampleEncoding::Mulaw,
            metadata: Metadata {
                title: Some("Test Tone".to_string()),
                description: Some("A 440 Hz sine".to_string()),
                ..Metadata::default()
            },
        };

        let conversion = convert_file(&input, &options).unwrap();
        let document = parse_document(&conversion.xml).unwrap();

        assert_eq!(document.metadata.title.as_deref(), Some("Test Tone"));
        assert_eq!(
            document.metadata.description.as_deref(),
            Some("A 440 Hz sine")
        );
        assert_eq!(document.audio_clips.len(), 1);

        let clip = &document.audio_clips[0];
        assert_eq!(clip.sample_rate, 8_000);
        assert_eq!(clip.channels, 1);
        assert_eq!(clip.encoding, SampleEncoding::Mulaw);
        assert!((clip.duration_secs() - 1.0).abs() < 0.01);
    }

    #[test]
    fn filename_is_the_title_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("my-song.wav");
        write_test_wav(&input, 8_000, 1, &[0; 800]);

        let conversion = convert_file(&input, &ConvertOptions::default()).unwrap();
        assert_eq!(conversion.metadata.title.as_deref(), Some("my-song"));
    }
}
