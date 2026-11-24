use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use racf_core::{AudioClip, Channel, ChannelLayout, SampleEncoding};

const MAX_CHANNELS: usize = 8;

/// An embedded clip whose encoded bytes stay shared and are decoded on demand.
///
/// Keeping the compressed data instead of an eagerly resampled `Vec<f32>` is
/// what makes long embedded clips cheap: memory use follows the file size, not
/// the rendered duration.
#[derive(Clone, Debug)]
pub struct PreparedClip {
    pub id: String,
    /// Number of interleaved source channels in each frame.
    pub channels: usize,
    /// Output sample rate, exposed for callers that need frame arithmetic.
    pub sample_rate: f32,
    data: Arc<[u8]>,
    encoding: SampleEncoding,
    source_frames: usize,
    source_rate: f64,
    frames: usize,
    passthrough: bool,
    kernel: Option<Arc<PolyphaseKernel>>,
}

impl PreparedClip {
    pub fn new(clip: &AudioClip, device_rate: f32) -> Self {
        let mut cache = ResamplerCache::default();
        Self::with_cache(clip, device_rate, &mut cache)
    }

    /// Shares resampling kernels between clips with the same source/destination
    /// rate pair instead of allocating one weight table per clip.
    pub(crate) fn with_cache(
        clip: &AudioClip,
        device_rate: f32,
        cache: &mut ResamplerCache,
    ) -> Self {
        let source_frames = clip.frames();
        let source_rate = clip.sample_rate as f64;
        let target_rate = device_rate as f64;
        let valid_rates = source_rate > 0.0 && target_rate > 0.0;
        let passthrough = !valid_rates || (source_rate - target_rate).abs() < 0.5;

        let frames = if source_frames == 0 || !valid_rates {
            0
        } else if passthrough {
            source_frames
        } else {
            ((source_frames as f64 * target_rate / source_rate).round() as usize).max(1)
        };

        let kernel = if passthrough {
            None
        } else {
            Some(cache.get(source_rate, target_rate))
        };

        Self {
            id: clip.id.clone(),
            data: Arc::clone(&clip.data),
            encoding: clip.encoding,
            source_frames,
            channels: clip.channels as usize,
            sample_rate: device_rate,
            source_rate,
            frames,
            passthrough,
            kernel,
        }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Frame index for a playback offset. The result is clamped to the clip.
    pub fn frame_at(&self, offset_secs: f32) -> usize {
        let frame = (offset_secs.max(0.0) * self.sample_rate).round() as usize;
        frame.min(self.frames)
    }

    /// Decodes and resamples one output frame into `out`.
    fn render_frame(&self, frame: usize, out: &mut [f32; MAX_CHANNELS]) {
        for sample in out.iter_mut().take(self.channels) {
            *sample = 0.0;
        }
        if frame >= self.frames || self.source_frames == 0 {
            return;
        }

        if self.passthrough {
            self.decode_into(frame.min(self.source_frames - 1), out);
            return;
        }

        let kernel = self
            .kernel
            .as_deref()
            .expect("non-passthrough clips have a resampling kernel");
        let step = self.source_rate / self.sample_rate as f64;
        let center = frame as f64 * step;
        let base = center.floor() as isize;
        let fraction = (center - base as f64) as f32;
        let phase = (fraction * PHASES as f32) as usize;
        let mut decoded = [0.0f32; MAX_CHANNELS];

        for (tap, weight) in kernel.phase_weights(phase).iter().enumerate() {
            let source = (base + kernel.first_offset + tap as isize)
                .clamp(0, self.source_frames as isize - 1) as usize;
            self.decode_into(source, &mut decoded);
            for (sample, value) in out
                .iter_mut()
                .take(self.channels)
                .zip(decoded.iter().take(self.channels))
            {
                *sample += *value * weight;
            }
        }
    }

    fn decode_into(&self, frame: usize, out: &mut [f32; MAX_CHANNELS]) {
        let frame_bytes = self.channels * self.encoding.bytes_per_sample();
        let start = frame * frame_bytes;
        let bytes = self.data.get(start..).unwrap_or_default();

        match self.encoding {
            SampleEncoding::Mulaw => {
                for (sample, byte) in out
                    .iter_mut()
                    .take(self.channels)
                    .zip(bytes.iter().take(self.channels))
                {
                    *sample = racf_core::decode_mulaw(*byte);
                }
            }
            SampleEncoding::PcmU8 => {
                for (sample, byte) in out
                    .iter_mut()
                    .take(self.channels)
                    .zip(bytes.iter().take(self.channels))
                {
                    *sample = (*byte as f32 - 128.0) / 128.0;
                }
            }
            SampleEncoding::PcmI16Le => {
                for (sample, pair) in out
                    .iter_mut()
                    .take(self.channels)
                    .zip(bytes.as_chunks::<2>().0.iter().take(self.channels))
                {
                    *sample = i16::from_le_bytes(*pair) as f32 / 32768.0;
                }
            }
        }
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

        let mut samples = [0.0f32; MAX_CHANNELS];
        self.clip.render_frame(self.frame, &mut samples);
        let channels = self.clip.channels;

        if channels == 1 {
            let sample = samples[0] * self.gain;
            add_mono(sample, self.pan, self.channel, bus, layout);
        } else if let Some(start) = self.channel.and_then(|channel| layout.index_of(channel)) {
            // A requested role acts as the first destination; subsequent
            // source channels follow the layout order and wrap around.
            for (source, sample) in samples.iter().take(channels).copied().enumerate() {
                let index = (start + source) % bus.len();
                bus[index] += sample * self.gain;
            }
        } else if let Some((left, right)) = layout.front_pair() {
            for (source, sample) in samples.iter().take(channels).copied().enumerate() {
                let index = if source == 0 {
                    left
                } else if source == 1 {
                    right
                } else {
                    source % bus.len()
                };
                bus[index] += sample * self.gain;
            }
        } else {
            for (source, sample) in samples.iter().take(channels).copied().enumerate() {
                let index = source % bus.len();
                bus[index] += sample * self.gain;
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
///
/// Kernels are precomputed per fractional output phase (polyphase resampling)
/// and shared by every clip with the same rate pair.
const LOBES: f32 = 4.0;

/// Caches one kernel per distinct source/destination rate ratio.
#[derive(Default)]
pub(crate) struct ResamplerCache(HashMap<u64, Arc<PolyphaseKernel>>);

impl ResamplerCache {
    pub(crate) fn get(&mut self, source_rate: f64, target_rate: f64) -> Arc<PolyphaseKernel> {
        let step = source_rate / target_rate;
        self.0
            .entry(step.to_bits())
            .or_insert_with(|| {
                let cutoff = if step > 1.0 { (1.0 / step) as f32 } else { 1.0 };
                Arc::new(PolyphaseKernel::new(LOBES / cutoff, cutoff, LOBES))
            })
            .clone()
    }
}

/// Number of fractional phases in the polyphase kernel table.
const PHASES: usize = 1024;

/// Precomputed, normalized Lanczos kernels indexed by fractional output phase.
///
/// Every output frame uses the same tap offsets; only the phase (where the
/// output sample falls between two source samples) changes. Building these
/// kernels once avoids evaluating `sin` and a table lookup for every tap of
/// every output frame.
pub(crate) struct PolyphaseKernel {
    first_offset: isize,
    taps: usize,
    weights: Vec<f32>,
}

impl fmt::Debug for PolyphaseKernel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PolyphaseKernel")
            .field("first_offset", &self.first_offset)
            .field("taps", &self.taps)
            .field("weights", &self.weights.len())
            .finish()
    }
}

impl PolyphaseKernel {
    fn new(radius: f32, cutoff: f32, lobes: f32) -> Self {
        let first_offset = -(radius.floor() as isize);
        let last_offset = radius.ceil() as isize;
        let taps = (last_offset - first_offset + 1) as usize;
        let mut weights = vec![0.0f32; PHASES * taps];

        for phase in 0..PHASES {
            let fraction = phase as f32 / PHASES as f32;
            let slice = &mut weights[phase * taps..(phase + 1) * taps];
            let mut sum = 0.0f32;

            for (tap, offset) in (first_offset..=last_offset).enumerate() {
                let weight = lanczos((fraction - offset as f32) * cutoff, lobes);
                slice[tap] = weight;
                sum += weight;
            }

            if sum.abs() > f32::EPSILON {
                for weight in slice.iter_mut() {
                    *weight /= sum;
                }
            }
        }

        Self {
            first_offset,
            taps,
            weights,
        }
    }

    fn phase_weights(&self, phase: usize) -> &[f32] {
        let start = phase.min(PHASES - 1) * self.taps;
        &self.weights[start..start + self.taps]
    }
}

#[cfg(test)]
struct ResampleContext<'a> {
    input: &'a [f32],
    source_frames: isize,
    step: f64,
    channels: usize,
    kernel: &'a PolyphaseKernel,
}

/// Renders `output` (interleaved frames starting at `start`) from the context.
#[cfg(test)]
fn render_frames(context: &ResampleContext<'_>, start: usize, output: &mut [f32]) {
    let channels = context.channels;
    let mut sums = vec![0.0f32; channels];

    for (local, frame_output) in output.chunks_mut(channels).enumerate() {
        let frame = start + local;
        let center = frame as f64 * context.step;
        let base = center.floor() as isize;
        let fraction = (center - base as f64) as f32;
        let phase = (fraction * PHASES as f32) as usize;

        sums.fill(0.0);
        for (tap, weight) in context.kernel.phase_weights(phase).iter().enumerate() {
            let source = (base + context.kernel.first_offset + tap as isize)
                .clamp(0, context.source_frames - 1) as usize;
            let source_start = source * channels;
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += context.input[source_start + channel] * weight;
            }
        }

        frame_output.copy_from_slice(&sums);
    }
}

#[cfg(test)]
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
    let kernel = ResamplerCache::default().get(source_rate, target_rate);
    let context = ResampleContext {
        input,
        source_frames: source_frames as isize,
        step,
        channels,
        kernel: &kernel,
    };

    let mut output = vec![0.0; target_frames * channels];
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);

    if workers <= 1 || target_frames < 65_536 {
        render_frames(&context, 0, &mut output);
        return output;
    }

    let chunk_frames = target_frames.div_ceil(workers);
    std::thread::scope(|scope| {
        for (index, chunk) in output.chunks_mut(chunk_frames * channels).enumerate() {
            let context = &context;
            let start = index * chunk_frames;
            scope.spawn(move || render_frames(context, start, chunk));
        }
    });

    output
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
            data: Arc::from(data),
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
    fn streaming_prepared_clip_matches_full_resampling() {
        let source_rate = 8_000;
        let target_rate = 18_000;
        let channels = 2;
        let samples: Vec<i16> = (0..400)
            .map(|index| ((index as f32 * 0.37).sin() * 20_000.0) as i16)
            .collect();
        let clip = clip(pcm16(&samples), channels as u8, source_rate);

        let expected = resample(
            &clip.decode_frames(),
            source_rate as f64,
            target_rate as f64,
            channels,
        );
        let prepared = PreparedClip::new(&clip, target_rate as f32);
        let mut rendered = Vec::with_capacity(expected.len());
        let mut frame = [0.0f32; MAX_CHANNELS];
        for index in 0..prepared.frames() {
            prepared.render_frame(index, &mut frame);
            rendered.extend_from_slice(&frame[..channels]);
        }

        assert_eq!(rendered.len(), expected.len());
        for (index, (actual, expected)) in rendered.iter().zip(expected.iter()).enumerate() {
            assert!(
                (actual - expected).abs() < 1.0e-5,
                "frame sample {index}: {actual} != {expected}"
            );
        }
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
