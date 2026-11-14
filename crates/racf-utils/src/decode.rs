use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{Metadata, MetadataLog, MetadataOptions, StandardTagKey, Value};
use symphonia::core::probe::Hint;

/// Tags read from the source file's metadata.
#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub title: Option<String>,
    pub description: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Decoded source audio, still at its native sample rate and channel count.
pub struct DecodedAudio {
    /// Interleaved `f32` samples in the range `-1.0..=1.0`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: usize,
    pub tags: Tags,
}

impl DecodedAudio {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 || self.channels == 0 {
            return 0.0;
        }

        self.samples.len() as f32 / (self.sample_rate as f32 * self.channels as f32)
    }
}

/// Decodes an MP3 (or any other format enabled in Cargo features) with Symphonia.
pub fn decode_file(path: &Path) -> Result<DecodedAudio> {
    let file = File::open(path)
        .with_context(|| format!("could not open input file {}", path.display()))?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("unsupported or invalid audio file {}", path.display()))?;

    let symphonia::core::probe::ProbeResult {
        mut format,
        metadata,
    } = probed;

    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .context("the input contains no decodable audio track")?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let mut sample_rate = codec_params.sample_rate.unwrap_or(0);
    let mut channels = codec_params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(0);

    let mut tags = read_log_tags(metadata.into_inner());

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .context("the input uses an unsupported audio codec")?;

    let mut samples = Vec::new();
    let mut buffer: Option<SampleBuffer<f32>> = None;
    let mut buffer_channels = 0usize;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(error) => {
                return Err(error).context("failed while reading the input stream");
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(error) => return Err(error).context("failed while decoding audio"),
        };

        let spec = *decoded.spec();
        let decoded_channels = spec.channels.count();

        if decoded_channels == 0 || spec.rate == 0 {
            continue;
        }

        if sample_rate == 0 {
            sample_rate = spec.rate;
        }
        if channels == 0 {
            channels = decoded_channels;
        }

        if buffer.is_none()
            || buffer_channels != decoded_channels
            || buffer
                .as_ref()
                .is_some_and(|buffer| buffer.capacity() < decoded.capacity() * decoded_channels)
        {
            buffer = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            buffer_channels = decoded_channels;
        }

        let Some(buffer) = buffer.as_mut() else {
            continue;
        };

        buffer.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buffer.samples());
    }

    if samples.is_empty() {
        bail!("no audio samples were decoded from {}", path.display());
    }

    if sample_rate == 0 {
        bail!("could not determine the input sample rate");
    }

    if channels == 0 {
        channels = 1;
    }

    // Some containers expose their tag revision only after the first packets
    // have been read. Merge anything new without overriding earlier values.
    let later_tags = read_tags(&mut format.metadata());
    merge_tags(&mut tags, later_tags);

    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
        tags,
    })
}

fn read_log_tags(log: Option<MetadataLog>) -> Tags {
    let Some(mut log) = log else {
        return Tags::default();
    };
    let mut metadata = log.metadata();
    read_tags(&mut metadata)
}

fn read_tags(metadata: &mut Metadata<'_>) -> Tags {
    let Some(revision) = metadata.skip_to_latest() else {
        return Tags::default();
    };

    let mut tags = Tags::default();

    if std::env::var_os("RACF_DEBUG_TAGS").is_some() {
        for tag in revision.tags() {
            eprintln!(
                "[tags] std={:?} key={:?} value={:?}",
                tag.std_key, tag.key, tag.value
            );
        }
    }

    for tag in revision.tags() {
        let Value::String(value) = &tag.value else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        match tag.std_key {
            Some(StandardTagKey::TrackTitle) if tags.title.is_none() => {
                tags.title = Some(value.to_string());
            }
            Some(StandardTagKey::Description) | Some(StandardTagKey::Comment)
                if tags.description.is_none() =>
            {
                tags.description = Some(value.to_string());
            }
            Some(StandardTagKey::Artist) if tags.artist.is_none() => {
                tags.artist = Some(value.to_string());
            }
            Some(StandardTagKey::Album) if tags.album.is_none() => {
                tags.album = Some(value.to_string());
            }
            _ => {}
        }

        // Some taggers write comments as custom `TXXX:comment` frames, which
        // Symphonia cannot map to a standard key.
        if tags.description.is_none() {
            let key = tag.key.to_ascii_lowercase();
            if key.contains("comment") || key.contains("description") {
                tags.description = Some(value.to_string());
            }
        }
    }

    tags
}

fn merge_tags(existing: &mut Tags, incoming: Tags) {
    if existing.title.is_none() {
        existing.title = incoming.title;
    }
    if existing.description.is_none() {
        existing.description = incoming.description;
    }
    if existing.artist.is_none() {
        existing.artist = incoming.artist;
    }
    if existing.album.is_none() {
        existing.album = incoming.album;
    }
}

/// Reduces the channel count of interleaved audio.
pub fn downmix(input: &[f32], source_channels: usize, target_channels: usize) -> Vec<f32> {
    if source_channels == 0 || target_channels == 0 || input.is_empty() {
        return Vec::new();
    }

    if source_channels == target_channels {
        return input.to_vec();
    }

    let frames = input.len() / source_channels;
    let mut output = vec![0.0; frames * target_channels];

    for frame in 0..frames {
        let source = &input[frame * source_channels..(frame + 1) * source_channels];

        match target_channels {
            1 => {
                output[frame] = source.iter().sum::<f32>() / source.len() as f32;
            }
            2 if source_channels == 1 => {
                output[frame * 2] = source[0];
                output[frame * 2 + 1] = source[0];
            }
            2 => {
                output[frame * 2] = source[0];
                output[frame * 2 + 1] = source[1];

                // Fold any extra channels into both sides at reduced level.
                for extra in &source[2..] {
                    output[frame * 2] += extra * 0.5;
                    output[frame * 2 + 1] += extra * 0.5;
                }
            }
            _ => {
                for channel in 0..target_channels {
                    output[frame * target_channels + channel] =
                        source.get(channel).copied().unwrap_or(0.0);
                }
            }
        }
    }

    output
}

/// Resamples interleaved audio. Upsampling is linear; downsampling uses a box
/// average to keep aliasing under control while remaining dependency-free.
pub fn resample(input: &[f32], source_rate: u32, target_rate: u32, channels: usize) -> Vec<f32> {
    if input.is_empty() || source_rate == 0 || target_rate == 0 || channels == 0 {
        return Vec::new();
    }

    if source_rate == target_rate {
        return input.to_vec();
    }

    let source_frames = input.len() / channels;
    if source_frames == 0 {
        return Vec::new();
    }

    let step = source_rate as f64 / target_rate as f64;
    let target_frames = ((source_frames as f64 / step).round() as usize).max(1);
    let mut output = vec![0.0; target_frames * channels];

    for frame in 0..target_frames {
        let position = frame as f64 * step;

        if step > 1.0 {
            let start = position.floor() as usize;
            let end = ((position + step).ceil() as usize).min(source_frames);
            let start = start.min(source_frames - 1);
            let end = end.max(start + 1).min(source_frames);
            let count = (end - start) as f32;

            for channel in 0..channels {
                let mut sum = 0.0;
                for source_frame in start..end {
                    sum += input[source_frame * channels + channel];
                }
                output[frame * channels + channel] = sum / count;
            }
        } else {
            let first = (position.floor() as usize).min(source_frames - 1);
            let second = (first + 1).min(source_frames - 1);
            let fraction = (position - first as f64) as f32;

            for channel in 0..channels {
                let a = input[first * channels + channel];
                let b = input[second * channels + channel];
                output[frame * channels + channel] = a + (b - a) * fraction;
            }
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_stereo_to_mono() {
        let output = downmix(&[1.0, 0.0, -1.0, 0.0], 2, 1);
        assert_eq!(output, vec![0.5, -0.5]);
    }

    #[test]
    fn downmix_duplicates_mono_to_stereo() {
        assert_eq!(downmix(&[0.25, -0.5], 1, 2), vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn resample_changes_frame_count() {
        let input = vec![0.0; 800];
        let output = resample(&input, 8000, 16_000, 1);
        assert_eq!(output.len(), 1600);

        let output = resample(&input, 16_000, 8000, 1);
        assert_eq!(output.len(), 400);
    }
}
