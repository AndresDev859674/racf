use std::sync::Arc;

use racf_core::{AudioClip, Channel, ChannelLayout};

/// An embedded clip decoded and resampled to the output device rate.
#[derive(Clone, Debug)]
pub struct PreparedClip {
    pub id: String,
    /// Interleaved frames at the device sample rate.
    pub samples: Arc<[f32]>,
    pub channels: usize,
    pub sample_rate: f32,
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
            sample_rate: device_rate,
        }
    }

    pub fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }

    /// Frame index for a playback offset. The result is clamped to the clip.
    pub fn frame_at(&self, offset_secs: f32) -> usize {
        let frame = (offset_secs.max(0.0) * self.sample_rate).round() as usize;
        frame.min(self.frames())
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

    /// Starts a clip at `offset_secs`, used when seeking into a long clip.
    pub fn new_at(
        clip: Arc<PreparedClip>,
        gain: f32,
        pan: f32,
        channel: Option<Channel>,
        offset_secs: f32,
    ) -> Self {
        let mut voice = Self::new(clip, gain, pan, channel);
        voice.frame = voice.clip.frame_at(offset_secs);
        voice
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

/// Windowed-sinc (Lanczos) resampling.
///
/// Upsampling uses a band-limited interpolator instead of linear interpolation,
/// and downsampling widens the kernel so content above the new Nyquist limit is
/// filtered out rather than folded back as aliasing. Weights are normalized,
/// which also keeps constant signals exactly constant at the edges.
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

    const LOBES: f32 = 4.0;
    const TABLE_RESOLUTION: usize = 4096;

    let step = source_rate / target_rate;
    let target_frames = ((source_frames as f64 / step).round() as usize).max(1);

    // Below 1.0 this stretches the kernel and lowers its cutoff, acting as an
    // anti-aliasing low-pass for downsampling.
    let cutoff = if step > 1.0 { (1.0 / step) as f32 } else { 1.0 };
    let radius = LOBES / cutoff;

    let table = kernel_table(LOBES, TABLE_RESOLUTION);
    let mut output = vec![0.0; target_frames * channels];
    let mut sums = vec![0.0f32; channels];

    for frame in 0..target_frames {
        let center = frame as f64 * step;
        let first = (center - radius as f64).ceil() as isize;
        let last = (center + radius as f64).floor() as isize;

        sums.fill(0.0);
        let mut weight_sum = 0.0f32;

        for source in first..=last {
            let distance = (center - source as f64) as f32;
            let weight = lookup_kernel(&table, distance * cutoff, LOBES, TABLE_RESOLUTION);
            if weight == 0.0 {
                continue;
            }

            let index = source.clamp(0, source_frames as isize - 1) as usize;
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += input[index * channels + channel] * weight;
            }
            weight_sum += weight;
        }

        if weight_sum.abs() > f32::EPSILON {
            for (channel, sum) in sums.iter().enumerate() {
                output[frame * channels + channel] = sum / weight_sum;
            }
        }
    }

    output
}

fn kernel_table(lobes: f32, resolution: usize) -> Vec<f32> {
    (0..=resolution)
        .map(|index| {
            let position = index as f32 / resolution as f32;
            lanczos(-lobes + 2.0 * lobes * position, lobes)
        })
        .collect()
}

fn lookup_kernel(table: &[f32], x: f32, lobes: f32, resolution: usize) -> f32 {
    if x <= -lobes || x >= lobes {
        return 0.0;
    }

    let position = (x + lobes) / (2.0 * lobes) * resolution as f32;
    let index = position.floor() as usize;
    let fraction = position - index as f32;
    let current = table[index.min(resolution)];
    let next = table[(index + 1).min(resolution)];

    current + (next - current) * fraction
}

fn lanczos(x: f32, lobes: f32) -> f32 {
    if x.abs() >= lobes {
        return 0.0;
    }

    sinc(x) * sinc(x / lobes)
}

fn sinc(x: f32) -> f32 {
    if x.abs() < 1.0e-6 {
        1.0
    } else {
        let value = std::f32::consts::PI * x;
        value.sin() / value
    }
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

    #[test]
    fn resampling_preserves_a_constant_level() {
        let samples = vec![0.5f32; 100];
        let output = resample(&samples, 8_000.0, 48_000.0, 1);

        assert!(!output.is_empty());
        assert!(output.iter().all(|sample| (sample - 0.5).abs() < 0.001));
    }

    #[test]
    fn resampling_preserves_sine_amplitude() {
        let source_rate = 22_050.0f64;
        let target_rate = 48_000.0f64;
        let input: Vec<f32> = (0..4_000)
            .map(|frame| {
                (2.0 * std::f32::consts::PI * 1_000.0 * frame as f32 / source_rate as f32).sin()
                    * 0.5
            })
            .collect();

        let output = resample(&input, source_rate, target_rate, 1);
        let middle = &output[500..output.len() - 500];
        let rms =
            (middle.iter().map(|sample| sample * sample).sum::<f32>() / middle.len() as f32).sqrt();

        assert!((rms - 0.5 / 2.0f32.sqrt()).abs() < 0.02, "rms was {rms}");
    }

    #[test]
    fn sample_voice_can_start_at_an_offset() {
        let prepared = Arc::new(PreparedClip::new(
            &clip(pcm16(&[16384, -16384, 8192, -8192]), 1, 2),
            2.0,
        ));
        let layout = ChannelLayout::stereo();
        let mut voice = SampleVoice::new_at(prepared, 1.0, 0.0, None, 1.0);
        let mut bus = vec![0.0; layout.len()];

        voice.advance(&mut bus, &layout);

        let expected = 0.25 * 0.707;
        assert!((bus[0] - expected).abs() < 0.001);
        assert!((bus[1] - expected).abs() < 0.001);
    }
}
