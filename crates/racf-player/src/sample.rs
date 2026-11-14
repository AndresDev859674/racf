use std::sync::Arc;

use racf_core::{AudioClip, Channel, ChannelLayout};

/// An embedded clip decoded and resampled to the output device rate.
#[derive(Clone, Debug)]
pub struct PreparedClip {
    pub id: String,
    /// Interleaved frames at the device sample rate.
    pub samples: Arc<[f32]>,
    pub channels: usize,
}

impl PreparedClip {
    pub fn new(clip: &AudioClip, device_rate: f32) -> Self {
        let decoded = clip.decode_frames();
        let channels = clip.channels as usize;
        let samples = resample(
            &decoded,
            clip.sample_rate as f64,
            device_rate as f64,
            channels,
        );

        Self {
            id: clip.id.clone(),
            samples: samples.into(),
            channels,
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }
}

/// A playing instance of a [`PreparedClip`].
#[derive(Clone, Debug)]
pub struct SampleVoice {
    clip: Arc<PreparedClip>,
    frame: usize,
    gain: f32,
    pan: f32,
    channel: Option<Channel>,
}

impl SampleVoice {
    pub fn new(clip: Arc<PreparedClip>, gain: f32, pan: f32, channel: Option<Channel>) -> Self {
        Self {
            clip,
            frame: 0,
            gain,
            pan: pan.clamp(-1.0, 1.0),
            channel,
        }
    }

    pub fn is_active(&self) -> bool {
        self.frame < self.clip.frames()
    }

    /// Mixes the current frame into the document bus and advances one frame.
    pub fn advance(&mut self, bus: &mut [f32], layout: &ChannelLayout) {
        if bus.is_empty() || !self.is_active() {
            return;
        }

        let frame_start = self.frame * self.clip.channels;
        let channels = self.clip.channels;

        if channels == 1 {
            let sample = self.clip.samples[frame_start] * self.gain;
            add_mono(sample, self.pan, self.channel, bus, layout);
        } else if let Some(start) = self.channel.and_then(|channel| layout.index_of(channel)) {
            // A requested role acts as the first destination; subsequent
            // source channels follow the layout order and wrap around.
            for source in 0..channels {
                let index = (start + source) % bus.len();
                bus[index] += self.clip.samples[frame_start + source] * self.gain;
            }
        } else if let Some((left, right)) = layout.front_pair() {
            for source in 0..channels {
                let index = if source == 0 {
                    left
                } else if source == 1 {
                    right
                } else {
                    source % bus.len()
                };
                bus[index] += self.clip.samples[frame_start + source] * self.gain;
            }
        } else {
            for source in 0..channels {
                let index = source % bus.len();
                bus[index] += self.clip.samples[frame_start + source] * self.gain;
            }
        }

        self.frame += 1;
    }
}

fn add_mono(
    sample: f32,
    pan: f32,
    channel: Option<Channel>,
    bus: &mut [f32],
    layout: &ChannelLayout,
) {
    if let Some(channel) = channel {
        if let Some(index) = layout.index_of(channel) {
            bus[index] += sample;
            return;
        }
    }

    if let Some((left, right)) = layout.front_pair() {
        let position = (pan + 1.0) * 0.5;
        bus[left] += sample * (1.0 - position).sqrt();
        bus[right] += sample * position.sqrt();
    } else if let Some(index) = layout.center_index() {
        bus[index] += sample;
    } else {
        bus[0] += sample;
    }
}

/// Resamples interleaved audio. Upsampling uses linear interpolation;
/// downsampling uses a short box average to reduce aliasing.
fn resample(input: &[f32], source_rate: f64, target_rate: f64, channels: usize) -> Vec<f32> {
    if input.is_empty() || channels == 0 || source_rate <= 0.0 || target_rate <= 0.0 {
        return Vec::new();
    }

    if (source_rate - target_rate).abs() < 0.5 {
        return input.to_vec();
    }

    let source_frames = input.len() / channels;
    if source_frames == 0 {
        return Vec::new();
    }

    let step = source_rate / target_rate;
    let target_frames = ((source_frames as f64 / step).round() as usize).max(1);
    let mut output = vec![0.0; target_frames * channels];

    for frame in 0..target_frames {
        let position = frame as f64 * step;

        if step > 1.0 {
            let start = position.floor() as usize;
            let end = ((position + step).ceil() as usize).min(source_frames);
            let count = (end.saturating_sub(start)).max(1) as f32;

            for channel in 0..channels {
                let mut sum = 0.0;
                for source in start..end.max(start + 1).min(source_frames) {
                    sum += input[source * channels + channel];
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
    use racf_core::SampleEncoding;

    fn clip(data: Vec<u8>, channels: u8, sample_rate: u32) -> AudioClip {
        AudioClip {
            id: "test".to_string(),
            at_ms: 0,
            gain: 1.0,
            pan: 0.0,
            channel: None,
            sample_rate,
            channels,
            encoding: SampleEncoding::PcmI16Le,
            data,
        }
    }

    fn pcm16(samples: &[i16]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    #[test]
    fn prepared_clip_decodes_and_resamples_to_device_rate() {
        let data = pcm16(&[0, 8192, -8192, 0, 8192, -8192, 0, 8192]);
        let prepared = PreparedClip::new(&clip(data, 1, 8000), 16_000.0);

        assert_eq!(prepared.channels, 1);
        assert_eq!(prepared.frames(), 16);
    }

    #[test]
    fn mono_clip_routes_to_requested_channel() {
        let prepared = Arc::new(PreparedClip::new(
            &clip(pcm16(&[16384, -16384]), 1, 44_100),
            44_100.0,
        ));
        let layout = ChannelLayout::surround_5_1();
        let mut voice = SampleVoice::new(Arc::clone(&prepared), 1.0, -1.0, Some(Channel::RearLeft));
        let mut bus = vec![0.0; layout.len()];

        voice.advance(&mut bus, &layout);

        assert!((bus[4] - 0.5).abs() < 0.001);
        assert!(bus
            .iter()
            .enumerate()
            .all(|(index, sample)| index == 4 || *sample == 0.0));
    }

    #[test]
    fn stereo_clip_uses_the_front_pair_by_default() {
        let prepared = Arc::new(PreparedClip::new(
            &clip(pcm16(&[16384, -16384]), 2, 44_100),
            44_100.0,
        ));
        let layout = ChannelLayout::stereo();
        let mut voice = SampleVoice::new(prepared, 1.0, 0.0, None);
        let mut bus = vec![0.0; layout.len()];

        voice.advance(&mut bus, &layout);

        assert!((bus[0] - 0.5).abs() < 0.001);
        assert!((bus[1] + 0.5).abs() < 0.001);
    }
}
