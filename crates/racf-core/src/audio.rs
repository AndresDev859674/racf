use std::fmt;

use base64::Engine as _;
use roxmltree::Node;

use crate::channel::Channel;
use crate::duration::parse_time_ms;
use crate::error::RacfError;

/// How the bytes inside an embedded audio clip are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding {
    /// G.711 µ-law, one byte per sample. Good quality at half the size of PCM16.
    Mulaw,
    /// Unsigned 8-bit PCM.
    PcmU8,
    /// Signed 16-bit little-endian PCM.
    PcmI16Le,
}

impl SampleEncoding {
    pub const fn name(self) -> &'static str {
        match self {
            SampleEncoding::Mulaw => "mulaw",
            SampleEncoding::PcmU8 => "pcm8",
            SampleEncoding::PcmI16Le => "pcm16",
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            SampleEncoding::Mulaw | SampleEncoding::PcmU8 => 1,
            SampleEncoding::PcmI16Le => 2,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value
            .trim()
            .to_ascii_lowercase()
            .replace(['_', ' '], "-")
            .as_str()
        {
            "mulaw" | "mu-law" | "ulaw" | "u-law" | "g711" | "g711-mulaw" => {
                Some(SampleEncoding::Mulaw)
            }
            "pcm8" | "pcm-u8" | "u8" | "8-bit" => Some(SampleEncoding::PcmU8),
            "pcm16" | "pcm-i16le" | "i16le" | "16-bit" => Some(SampleEncoding::PcmI16Le),
            _ => None,
        }
    }
}

impl fmt::Display for SampleEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A decoded-at-playback block of sampled audio embedded in a RACF document.
///
/// Clips are stored as encoded bytes so the document stays a single file and
/// avoids the size explosion of writing one XML element per sample.
#[derive(Clone, Debug)]
pub struct AudioClip {
    pub id: String,
    pub at_ms: u64,
    pub gain: f32,
    pub pan: f32,
    pub channel: Option<Channel>,
    pub sample_rate: u32,
    pub channels: u8,
    pub encoding: SampleEncoding,
    pub data: Vec<u8>,
}

impl AudioClip {
    pub fn frames(&self) -> usize {
        let frame_bytes = self.channels as usize * self.encoding.bytes_per_sample();
        self.data.len() / frame_bytes
    }

    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.frames() as f32 / self.sample_rate as f32
    }

    pub fn duration_ms(&self) -> u64 {
        (self.duration_secs() * 1000.0).round() as u64
    }

    /// Decodes the clip into interleaved `f32` samples in the range `-1.0..=1.0`.
    pub fn decode_frames(&self) -> Vec<f32> {
        match self.encoding {
            SampleEncoding::Mulaw => self.data.iter().copied().map(decode_mulaw).collect(),
            SampleEncoding::PcmU8 => self
                .data
                .iter()
                .map(|byte| (*byte as f32 - 128.0) / 128.0)
                .collect(),
            SampleEncoding::PcmI16Le => self
                .data
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| i16::from_le_bytes(*bytes) as f32 / 32768.0)
                .collect(),
        }
    }
}

/// Decodes one G.711 µ-law byte to a normalized sample.
pub fn decode_mulaw(byte: u8) -> f32 {
    let value = !byte;
    let sign = value & 0x80;
    let exponent = (value >> 4) & 0x07;
    let mantissa = value & 0x0f;
    let magnitude = (((mantissa as i32) << 3) + 0x84) << exponent as i32;
    let sample = if sign != 0 {
        0x84 - magnitude
    } else {
        magnitude - 0x84
    };

    (sample as f32 / 32768.0).clamp(-1.0, 1.0)
}

pub(crate) fn parse_audio(root: Node<'_, '_>) -> Result<Vec<AudioClip>, RacfError> {
    let Some(audio_node) = root.children().find(|node| node.has_tag_name("audio")) else {
        return Ok(Vec::new());
    };

    let mut clips = Vec::new();

    for (index, node) in audio_node
        .children()
        .filter(|node| node.has_tag_name("clip"))
        .enumerate()
    {
        let id = node
            .attribute("id")
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("clip-{index}"));

        let at_ms = match node.attribute("at").or_else(|| node.attribute("offset")) {
            Some(value) => parse_time_ms(value).ok_or_else(|| RacfError::InvalidAudio {
                clip: id.clone(),
                message: format!("invalid start time {value:?}"),
            })?,
            None => 0,
        };

        let sample_rate = node
            .attribute("sampleRate")
            .or_else(|| node.attribute("rate"))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|rate| *rate > 0)
            .unwrap_or(44_100);

        let channels = node
            .attribute("channels")
            .and_then(|value| value.trim().parse::<u8>().ok())
            .unwrap_or(1);

        if !(1..=8).contains(&channels) {
            return Err(RacfError::InvalidAudio {
                clip: id,
                message: format!("channels must be between 1 and 8, got {channels}"),
            });
        }

        let encoding_name = node.attribute("encoding").unwrap_or("mulaw");
        let encoding =
            SampleEncoding::parse(encoding_name).ok_or_else(|| RacfError::InvalidAudio {
                clip: id.clone(),
                message: format!("unknown encoding {encoding_name:?}"),
            })?;

        let data_node = node
            .children()
            .find(|child| child.has_tag_name("data"))
            .ok_or_else(|| RacfError::InvalidAudio {
                clip: id.clone(),
                message: "missing <data> element".to_string(),
            })?;

        let encoded: String = data_node
            .descendants()
            .filter(|child| child.is_text())
            .filter_map(|child| child.text())
            .flat_map(str::chars)
            .filter(|character| !character.is_whitespace())
            .collect();

        if encoded.is_empty() {
            return Err(RacfError::InvalidAudio {
                clip: id.clone(),
                message: "<data> is empty".to_string(),
            });
        }

        let data = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| RacfError::InvalidAudio {
                clip: id.clone(),
                message: format!("invalid base64 data: {error}"),
            })?;

        let frame_bytes = channels as usize * encoding.bytes_per_sample();
        if data.len() % frame_bytes != 0 {
            return Err(RacfError::InvalidAudio {
                clip: id,
                message: format!(
                    "data length {} is not a multiple of {frame_bytes} bytes per frame",
                    data.len()
                ),
            });
        }

        clips.push(AudioClip {
            id,
            at_ms,
            gain: node
                .attribute("gain")
                .and_then(|value| value.trim().parse::<f32>().ok())
                .unwrap_or(1.0),
            pan: node
                .attribute("pan")
                .and_then(|value| value.trim().parse::<f32>().ok())
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0),
            channel: node.attribute("channel").and_then(Channel::parse),
            sample_rate,
            channels,
            encoding,
            data,
        });
    }

    Ok(clips)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse_document;

    #[test]
    fn mulaw_silence_decodes_to_zero() {
        assert!((decode_mulaw(0xff)).abs() < 0.001);
    }

    #[test]
    fn decodes_embedded_audio_clip() {
        let data = base64::engine::general_purpose::STANDARD.encode([0xffu8, 0xff, 0xff, 0xff]);
        let xml = format!(
            r#"
            <racf>
                <audio>
                    <clip id="main" at="1.5s" sampleRate="22050" channels="1" encoding="mulaw">
                        <data>{data}</data>
                    </clip>
                </audio>
            </racf>
            "#
        );

        let document = parse_document(&xml).unwrap();
        assert_eq!(document.audio_clips.len(), 1);

        let clip = &document.audio_clips[0];
        assert_eq!(clip.id, "main");
        assert_eq!(clip.at_ms, 1500);
        assert_eq!(clip.sample_rate, 22_050);
        assert_eq!(clip.channels, 1);
        assert_eq!(clip.encoding, SampleEncoding::Mulaw);
        assert_eq!(clip.frames(), 4);
        assert!((clip.duration_secs() - 4.0 / 22_050.0).abs() < 0.0001);
        assert!(clip
            .decode_frames()
            .iter()
            .all(|sample| sample.abs() < 0.001));
    }

    #[test]
    fn decodes_pcm16_little_endian() {
        let data = base64::engine::general_purpose::STANDARD.encode([0x00u8, 0x40, 0x00, 0xc0]); // 16384, -16384
        let xml = format!(
            r#"<racf><audio><clip sampleRate="8000" channels="1" encoding="pcm16"><data>{data}</data></clip></audio></racf>"#
        );

        let document = parse_document(&xml).unwrap();
        let samples = document.audio_clips[0].decode_frames();
        assert_eq!(samples.len(), 2);
        assert!((samples[0] - 0.5).abs() < 0.001);
        assert!((samples[1] + 0.5).abs() < 0.001);
    }

    #[test]
    fn rejects_unknown_encoding() {
        let xml = r#"
            <racf>
                <audio>
                    <clip encoding="mp3"><data>AAAA</data></clip>
                </audio>
            </racf>
        "#;

        assert!(matches!(
            parse_document(xml),
            Err(RacfError::InvalidAudio { .. })
        ));
    }
}
