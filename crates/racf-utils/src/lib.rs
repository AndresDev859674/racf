//! Conversion and inspection helpers for RACF documents.
//!
//! The converter embeds decoded audio rather than linking to the source file,
//! so a converted document is portable. File size is controlled with the
//! output quality preset, sample rate, channel count, and encoding options.

pub mod decode;
pub mod encode;
pub mod writer;

use std::path::Path;

use anyhow::{bail, Context, Result};
use racf_core::{Lyrics, LyricsFormat, Metadata, SampleEncoding};

use crate::decode::Tags;

/// How much fidelity the converter should preserve by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputQuality {
    /// Small files: 22.05 kHz mono µ-law, good for speech and casual listening.
    Lossy,
    /// High fidelity: source sample rate and channel count, 16-bit PCM.
    Raw,
}

impl OutputQuality {
    pub const fn name(self) -> &'static str {
        match self {
            OutputQuality::Lossy => "lossy",
            OutputQuality::Raw => "raw",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "lossy" | "compact" | "small" | "mulaw" | "mu-law" => Some(OutputQuality::Lossy),
            "raw" | "lossless" | "high" | "hifi" | "pcm" | "pcm16" => Some(OutputQuality::Raw),
            _ => None,
        }
    }
}

/// Options controlling the audio payload written into the RACF file.
#[derive(Clone, Debug)]
pub struct ConvertOptions {
    /// Default format profile. Explicit fields below always win.
    pub quality: OutputQuality,
    /// Target sample rate in Hz. `None` uses the quality profile default.
    pub sample_rate: Option<u32>,
    /// Target channel count. `None` uses the quality profile default.
    pub channels: Option<u8>,
    /// Sample encoding. `None` uses the quality profile default.
    pub encoding: Option<SampleEncoding>,
    /// Metadata supplied on the command line. Missing fields fall back to tags
    /// in the source file and then to the input filename.
    pub metadata: Metadata,
    /// Lyrics supplied explicitly, for example by `--lyrics`.
    pub lyrics: Option<Lyrics>,
    /// Import embedded tags or sidecar `.lrc`/`.srt` files when no explicit
    /// lyrics were supplied.
    pub include_lyrics: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            quality: OutputQuality::Lossy,
            sample_rate: None,
            channels: None,
            encoding: None,
            metadata: Metadata::default(),
            lyrics: None,
            include_lyrics: true,
        }
    }
}

/// Result of a successful conversion.
pub struct Conversion {
    pub xml: String,
    pub metadata: Metadata,
    pub tags: Tags,
    pub lyrics: Option<Lyrics>,
    /// Where the lyrics came from (`--lyrics`, an embedded tag, or a sidecar).
    pub lyrics_source: Option<String>,
    pub quality: OutputQuality,
    pub output_sample_rate: u32,
    pub output_channels: u8,
    pub output_encoding: SampleEncoding,
    pub source_sample_rate: u32,
    pub source_channels: usize,
    pub source_duration_secs: f32,
    pub output_duration_secs: f32,
    pub data_bytes: usize,
}

/// Decodes `input` and builds the XML for a new RACF document.
pub fn convert_file(input: &Path, options: &ConvertOptions) -> Result<Conversion> {
    let decoded = decode::decode_file(input)?;
    let source_duration_secs = decoded.duration_secs();

    let settings = resolve_audio_settings(options, &decoded)?;
    let (lyrics, lyrics_source) = resolve_lyrics(options, &decoded.tags, input);

    let downmixed = decode::downmix(
        &decoded.samples,
        decoded.channels,
        settings.channels as usize,
    );
    let resampled = decode::resample(
        &downmixed,
        decoded.sample_rate,
        settings.sample_rate,
        settings.channels as usize,
    );

    if resampled.is_empty() {
        bail!("the converted audio is empty");
    }

    let data = encode::encode(&resampled, settings.encoding);
    let metadata = merge_metadata(&options.metadata, &decoded.tags, lyrics.as_ref(), input);

    let clip = writer::ClipSpec {
        id: "main",
        at_ms: 0,
        gain: 1.0,
        pan: 0.0,
        channel: None,
        sample_rate: settings.sample_rate,
        channels: settings.channels,
        encoding: settings.encoding,
        data: &data,
    };
    let xml = writer::render_document(&metadata, lyrics.as_ref(), &clip);

    let output_duration_secs =
        resampled.len() as f32 / (settings.sample_rate as f32 * settings.channels as f32);

    Ok(Conversion {
        xml,
        metadata,
        tags: decoded.tags,
        lyrics,
        lyrics_source,
        quality: options.quality,
        output_sample_rate: settings.sample_rate,
        output_channels: settings.channels,
        output_encoding: settings.encoding,
        source_sample_rate: decoded.sample_rate,
        source_channels: decoded.channels,
        source_duration_secs,
        output_duration_secs,
        data_bytes: data.len(),
    })
}

struct AudioSettings {
    sample_rate: u32,
    channels: u8,
    encoding: SampleEncoding,
}

fn resolve_audio_settings(
    options: &ConvertOptions,
    decoded: &decode::DecodedAudio,
) -> Result<AudioSettings> {
    let (default_rate, default_channels, default_encoding) = match options.quality {
        OutputQuality::Lossy => (22_050, 1, SampleEncoding::Mulaw),
        OutputQuality::Raw => (
            decoded.sample_rate,
            decoded.channels.clamp(1, 2) as u8,
            SampleEncoding::PcmI16Le,
        ),
    };

    let sample_rate = options.sample_rate.unwrap_or(default_rate);
    let channels = options.channels.unwrap_or(default_channels);
    let encoding = options.encoding.unwrap_or(default_encoding);

    if !(4_000..=384_000).contains(&sample_rate) {
        bail!("sample rate must be between 4000 and 384000 Hz, got {sample_rate}");
    }

    if !(1..=2).contains(&channels) {
        bail!("channels must be 1 or 2, got {channels}");
    }

    Ok(AudioSettings {
        sample_rate,
        channels,
        encoding,
    })
}

fn resolve_lyrics(
    options: &ConvertOptions,
    tags: &Tags,
    input: &Path,
) -> (Option<Lyrics>, Option<String>) {
    if let Some(lyrics) = &options.lyrics {
        return (Some(lyrics.clone()), Some("command line".to_string()));
    }

    if !options.include_lyrics {
        return (None, None);
    }

    if let Some(text) = &tags.lyrics {
        if !text.trim().is_empty() {
            let format = detect_lyrics_format(text);
            return (
                Some(Lyrics::parse(format, text.clone())),
                Some("embedded tag".to_string()),
            );
        }
    }

    for extension in ["lrc", "srt"] {
        let sidecar = input.with_extension(extension);
        if !sidecar.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&sidecar) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let format =
            LyricsFormat::from_path(&sidecar).unwrap_or_else(|| detect_lyrics_format(&text));
        return (
            Some(Lyrics::parse(format, text)),
            Some(format!("sidecar {}", sidecar.display())),
        );
    }

    (None, None)
}

/// Loads a lyrics file, using the suffix or the content to pick a format.
pub fn load_lyrics(path: &Path, format: Option<LyricsFormat>) -> Result<Lyrics> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read lyrics file {}", path.display()))?;
    if text.trim().is_empty() {
        bail!("lyrics file {} is empty", path.display());
    }

    let format = format
        .or_else(|| LyricsFormat::from_path(path))
        .unwrap_or_else(|| detect_lyrics_format(&text));
    Ok(Lyrics::parse(format, text))
}

/// Guesses whether lyric text is SRT, LRC, or plain text.
pub fn detect_lyrics_format(text: &str) -> LyricsFormat {
    if text.contains("-->") {
        return LyricsFormat::Srt;
    }

    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some((inside, _)) = rest.split_once(']') else {
            continue;
        };
        if parse_lrc_clock(inside) {
            return LyricsFormat::Lrc;
        }
    }

    LyricsFormat::Native
}

fn parse_lrc_clock(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn merge_metadata(cli: &Metadata, tags: &Tags, lyrics: Option<&Lyrics>, input: &Path) -> Metadata {
    let lyric_tag = |key: &str| {
        lyrics
            .and_then(|lyrics| lyrics.tag(key))
            .map(str::to_string)
    };

    Metadata {
        title: cli
            .title
            .clone()
            .or_else(|| tags.title.clone())
            .or_else(|| lyric_tag("ti"))
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
            .or_else(|| lyric_tag("ar"))
            .filter(|artist| !artist.trim().is_empty()),
        album: cli
            .album
            .clone()
            .or_else(|| tags.album.clone())
            .or_else(|| lyric_tag("al"))
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
            sample_rate: Some(8_000),
            channels: Some(1),
            encoding: Some(SampleEncoding::Mulaw),
            metadata: Metadata {
                title: Some("Test Tone".to_string()),
                description: Some("A 440 Hz sine".to_string()),
                ..Metadata::default()
            },
            ..ConvertOptions::default()
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
    fn raw_quality_preserves_the_source_rate_and_uses_pcm16() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("tune.wav");
        write_test_wav(&input, 44_100, 1, &[0; 441]);

        let options = ConvertOptions {
            quality: OutputQuality::Raw,
            ..ConvertOptions::default()
        };
        let conversion = convert_file(&input, &options).unwrap();

        assert_eq!(conversion.output_sample_rate, 44_100);
        assert_eq!(conversion.output_encoding, SampleEncoding::PcmI16Le);
    }

    #[test]
    fn filename_is_the_title_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("my-song.wav");
        write_test_wav(&input, 8_000, 1, &[0; 800]);

        let conversion = convert_file(&input, &ConvertOptions::default()).unwrap();
        assert_eq!(conversion.metadata.title.as_deref(), Some("my-song"));
    }

    #[test]
    fn sidecar_lrc_is_embedded_and_fills_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("song.wav");
        write_test_wav(&input, 8_000, 1, &[0; 800]);
        std::fs::write(
            directory.path().join("song.lrc"),
            "[ti:Night Drive]\n[ar:Andres]\n[00:01.00]Hello\n",
        )
        .unwrap();

        let conversion = convert_file(&input, &ConvertOptions::default()).unwrap();
        let document = parse_document(&conversion.xml).unwrap();

        assert_eq!(document.metadata.title.as_deref(), Some("Night Drive"));
        assert_eq!(document.metadata.artist.as_deref(), Some("Andres"));
        let lyrics = document.lyrics.as_ref().unwrap();
        assert_eq!(lyrics.format, LyricsFormat::Lrc);
        assert_eq!(lyrics.lines[0].text, "Hello");
    }

    #[test]
    fn detects_lrc_after_header_tags() {
        assert_eq!(
            detect_lyrics_format("[ar:Artist]\n[ti:Song]\n[00:01.00]Hello\n"),
            LyricsFormat::Lrc
        );
        assert_eq!(
            detect_lyrics_format("plain line\nanother line\n"),
            LyricsFormat::Native
        );
    }

    #[test]
    fn include_lyrics_false_skips_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("song.wav");
        write_test_wav(&input, 8_000, 1, &[0; 800]);
        std::fs::write(directory.path().join("song.lrc"), "[00:01.00]Hello\n").unwrap();

        let options = ConvertOptions {
            include_lyrics: false,
            ..ConvertOptions::default()
        };
        let conversion = convert_file(&input, &options).unwrap();
        assert!(conversion.lyrics.is_none());
    }
}
