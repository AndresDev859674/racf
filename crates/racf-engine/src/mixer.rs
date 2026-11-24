use std::sync::Arc;

use racf_core::{AudioEvent, Channel, ChannelLayout, Document, NoteEvent};

use crate::eq::{EqPreset, GraphicEq};
use crate::sample::{PreparedClip, SampleVoice};
use crate::voice::Voice;

/// Real-time renderer. The audio callback owns the only mutable access to the
/// mixer, so no allocation happens while rendering.
pub struct MasterMixer {
    voices: Vec<Voice>,
    sample_voices: Vec<SampleVoice>,
    clips: Vec<Arc<PreparedClip>>,
    layout: ChannelLayout,
    sample_rate: f32,
    channels: usize,
    bit_depth: u8,
    bitrate_kbps: u32,
    target_khz: Option<f32>,
    isolation_active: bool,
    isolated_target: Option<String>,
    master_gain: f32,
    paused: bool,
    eq: GraphicEq,

    // Per-channel scratch state, sized to the document's channel layout.
    bus: Vec<f32>,
    lowpass: Vec<f32>,
    held: Vec<f32>,
    hold_counter: usize,
}

impl MasterMixer {
    /// Creates a mixer for `document`, rendering at `sample_rate` with
    /// `channels` interleaved output channels.
    pub fn new(document: &Document, sample_rate: f32, channels: usize) -> Self {
        let channel_count = document.channel_layout.len();

        let clips = document
            .audio_clips
            .iter()
            .map(|clip| Arc::new(PreparedClip::new(clip, sample_rate)))
            .collect();

        Self {
            voices: Vec::new(),
            sample_voices: Vec::new(),
            clips,
            layout: document.channel_layout.clone(),
            sample_rate,
            channels: channels.max(1),
            bit_depth: document.bit_depth,
            bitrate_kbps: document.bitrate_kbps,
            target_khz: document.target_sample_rate.map(|rate| rate / 1000.0),
            isolation_active: false,
            isolated_target: None,
            master_gain: 1.0,
            paused: false,
            eq: GraphicEq::new(sample_rate, channel_count),
            bus: vec![0.0; channel_count],
            lowpass: vec![0.0; channel_count],
            held: vec![0.0; channel_count],
            hold_counter: 0,
        }
    }

    pub fn push_note(&mut self, note: &NoteEvent) {
        self.voices.push(Voice::from_event(note, self.sample_rate));
    }

    /// Starts a note partway through, used when seeking.
    pub fn push_note_at(&mut self, note: &NoteEvent, offset_secs: f32) {
        self.voices
            .push(Voice::from_event_at(note, self.sample_rate, offset_secs));
    }

    pub fn push_audio(&mut self, event: &AudioEvent) {
        let Some(clip) = self.clips.get(event.clip_index) else {
            return;
        };

        self.sample_voices.push(SampleVoice::new(
            Arc::clone(clip),
            event.gain,
            event.pan,
            event.channel,
        ));
    }

    /// Starts an embedded clip at `offset_secs`, used when seeking.
    pub fn push_audio_at(&mut self, event: &AudioEvent, offset_secs: f32) {
        let Some(clip) = self.clips.get(event.clip_index) else {
            return;
        };

        self.sample_voices.push(SampleVoice::new_at(
            Arc::clone(clip),
            event.gain,
            event.pan,
            event.channel,
            offset_secs,
        ));
    }

    pub fn set_separation(&mut self, active: bool, target: Option<String>) {
        self.isolation_active = active;
        self.isolated_target = target;
    }

    /// Sets the master volume. `100.0` is unity gain; the usable range is
    /// 0–200%.
    pub fn set_master_volume_percent(&mut self, percent: f32) {
        self.master_gain = percent.clamp(0.0, 200.0) / 100.0;
    }

    pub fn master_volume_percent(&self) -> f32 {
        self.master_gain * 100.0
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn set_eq_preset(&mut self, preset: EqPreset) {
        self.eq.set_preset(preset);
    }

    pub fn set_eq_band_gain(&mut self, index: usize, gain_db: f32) -> bool {
        self.eq.set_band_gain(index, gain_db)
    }

    pub fn disable_eq(&mut self) {
        self.eq.disable();
    }

    pub fn eq_label(&self) -> String {
        self.eq.label()
    }

    pub fn eq_gains(&self) -> [f32; 10] {
        self.eq.gains()
    }

    pub fn eq_enabled(&self) -> bool {
        self.eq.is_enabled()
    }

    /// Clears all sounding voices and filter memory. Volume and EQ settings are
    /// preserved. Used when seeking or looping.
    pub fn reset(&mut self) {
        self.voices.clear();
        self.sample_voices.clear();
        self.lowpass.fill(0.0);
        self.held.fill(0.0);
        self.hold_counter = 0;
        self.eq.reset();
    }

    pub fn layout(&self) -> &ChannelLayout {
        &self.layout
    }

    /// Renders interleaved `f32` frames into `output` and returns the number of
    /// complete frames written. The channel count is fixed at construction, so
    /// a host only has to hand over a buffer and count the returned frames.
    pub fn render_f32(&mut self, output: &mut [f32]) -> usize {
        let device_channels = self.channels;
        let frames = output.len() / device_channels;
        if frames == 0 {
            return 0;
        }

        if self.paused {
            output.fill(0.0);
            return frames;
        }

        let channel_count = self.layout.len();
        self.bus.resize(channel_count, 0.0);
        self.lowpass.resize(channel_count, 0.0);
        self.held.resize(channel_count, 0.0);

        let khz_factor = self
            .target_khz
            .map(|khz| ((self.sample_rate / 1000.0) / khz).max(1.0) as usize)
            .unwrap_or(1);

        let lowpass_cutoff = if self.bitrate_kbps < 320 {
            let cutoff = (self.bitrate_kbps as f32 * 65.0 + 6000.0).clamp(2000.0, 18500.0);
            let w = 2.0 * std::f32::consts::PI * cutoff / self.sample_rate;
            Some(w / (1.0 + w))
        } else {
            None
        };

        // Synthesized voices are summed from many oscillators, so they need
        // headroom to avoid clipping. Embedded audio is already at normal
        // playback level and must not be attenuated by this factor.
        const SYNTH_HEADROOM: f32 = 0.18;

        for frame in output.chunks_mut(device_channels) {
            self.bus.fill(0.0);

            for voice in self.voices.iter_mut() {
                if self.isolation_active {
                    if let Some(target) = self.isolated_target.as_deref() {
                        if voice.track_id != target {
                            continue;
                        }
                    }
                }

                let sample = voice.next_sample() * SYNTH_HEADROOM;
                voice.add_to_bus(sample, &mut self.bus, &self.layout);
            }

            // Bitrate low-pass, sample-rate hold, and bit-depth quantization
            // are characteristics of the synthesized format. Applying them to
            // an embedded MP3-derived clip would only degrade it further.
            if let Some(alpha) = lowpass_cutoff {
                for (state, sample) in self.lowpass.iter_mut().zip(self.bus.iter_mut()) {
                    *state += alpha * (*sample - *state);
                    *sample = *state;
                }
            }

            if khz_factor > 1 {
                self.hold_counter += 1;
                if self.hold_counter >= khz_factor {
                    self.held.copy_from_slice(&self.bus);
                    self.hold_counter = 0;
                } else {
                    self.bus.copy_from_slice(&self.held);
                }
            }

            if self.bit_depth == 8 {
                let steps = 32.0;
                for sample in &mut self.bus {
                    *sample = (*sample * steps).round() / steps;
                }
            }

            if !self.isolation_active || self.isolated_target.is_none() {
                for voice in &mut self.sample_voices {
                    voice.advance(&mut self.bus, &self.layout);
                }
            }

            self.eq.process(&mut self.bus);

            downmix(&self.bus, &self.layout, frame);
            apply_master(frame, self.master_gain);
        }

        self.voices.retain(Voice::is_active);
        self.sample_voices.retain(SampleVoice::is_active);
        frames
    }
}

/// Converts the document's channel bus to the device's channel count.
fn downmix(bus: &[f32], layout: &ChannelLayout, output: &mut [f32]) {
    if output.is_empty() {
        return;
    }

    if output.len() == bus.len() {
        output.copy_from_slice(bus);
        return;
    }

    match output.len() {
        1 => {
            let mut weighted = 0.0;
            let mut total_weight = 0.0;

            for (channel, sample) in layout.channels().iter().zip(bus.iter()) {
                let weight = match channel {
                    Channel::FrontLeft | Channel::FrontRight | Channel::FrontCenter => 1.0,
                    Channel::LowFrequency => 0.5,
                    Channel::RearLeft
                    | Channel::RearRight
                    | Channel::SideLeft
                    | Channel::SideRight => 0.7,
                };
                weighted += *sample * weight;
                total_weight += weight;
            }

            output[0] = if total_weight > 0.0 {
                weighted / total_weight
            } else {
                0.0
            };
        }
        2 => {
            let mut left = 0.0;
            let mut right = 0.0;

            for (channel, sample) in layout.channels().iter().zip(bus.iter()) {
                match channel {
                    Channel::FrontLeft => left += *sample,
                    Channel::FrontRight => right += *sample,
                    Channel::FrontCenter => {
                        left += *sample * 0.707;
                        right += *sample * 0.707;
                    }
                    Channel::LowFrequency => {
                        left += *sample * 0.5;
                        right += *sample * 0.5;
                    }
                    Channel::RearLeft | Channel::SideLeft => {
                        left += *sample * 0.707;
                    }
                    Channel::RearRight | Channel::SideRight => {
                        right += *sample * 0.707;
                    }
                }
            }

            output[0] = left;
            output[1] = right;
        }
        _ => {
            if bus.len() == 1 {
                output.fill(0.0);
                output[0] = bus[0];
                output[1] = bus[0];
            } else {
                for (index, sample) in output.iter_mut().enumerate() {
                    *sample = bus.get(index).copied().unwrap_or(0.0);
                }
            }
        }
    }
}

/// Applies master volume and a soft-knee limiter.
///
/// Everything below the knee is untouched, so 100% volume remains transparent.
/// Above it the curve bends continuously and approaches full scale instead of
/// hard-clipping like the old `0.95` clamp.
fn apply_master(samples: &mut [f32], gain: f32) {
    for sample in samples {
        *sample = soft_limit(*sample * gain);
    }
}

fn soft_limit(sample: f32) -> f32 {
    const KNEE: f32 = 0.75;
    const HEADROOM: f32 = 1.0 - KNEE;

    let magnitude = sample.abs();
    if magnitude <= KNEE {
        return sample;
    }

    let over = magnitude - KNEE;
    let limited = KNEE + HEADROOM * (over / HEADROOM).tanh();
    limited.copysign(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn stereo_downmix_folds_center_lfe_and_surrounds() {
        let layout = ChannelLayout::surround_5_1();
        let bus = [0.05, 0.10, 0.15, 0.20, 0.25, 0.30];
        let mut output = [0.0; 2];

        downmix(&bus, &layout, &mut output);

        assert_close(output[0], 0.05 + 0.15 * 0.707 + 0.20 * 0.5 + 0.25 * 0.707);
        assert_close(output[1], 0.10 + 0.15 * 0.707 + 0.20 * 0.5 + 0.30 * 0.707);
    }

    #[test]
    fn mono_downmix_averages_the_bus() {
        let layout = ChannelLayout::stereo();
        let bus = [0.5, 0.25];
        let mut output = [0.0; 1];

        downmix(&bus, &layout, &mut output);

        assert_close(output[0], 0.375);
    }

    #[test]
    fn mono_bus_is_duplicated_to_a_stereo_device() {
        let layout = ChannelLayout::mono();
        let bus = [0.5];
        let mut output = [0.0; 2];

        downmix(&bus, &layout, &mut output);

        assert_close(output[0], 0.5 * 0.707);
        assert_close(output[1], 0.5 * 0.707);
    }

    #[test]
    fn embedded_audio_is_not_attenuated_by_synth_headroom() {
        // One PCM16 sample at 0.5: bytes 0x00 0x40, base64 "AEA=".
        let xml = r#"
            <racf channelLayout="stereo">
                <audio>
                    <clip at="0ms" sampleRate="44100" channels="1" encoding="pcm16">
                        <data>AEA=</data>
                    </clip>
                </audio>
            </racf>
        "#;
        let document = racf_core::parse_document(xml).unwrap();
        let mut mixer = MasterMixer::new(&document, 44_100.0, 2);

        mixer.push_audio(&AudioEvent {
            clip_index: 0,
            clip_id: "clip-0".to_string(),
            gain: 1.0,
            pan: 0.0,
            channel: None,
        });

        let mut output = [0.0f32; 2];
        mixer.render_f32(&mut output);

        // A centered mono clip splits across the front pair with equal power.
        assert_close(output[0], 0.5 * 0.707);
        assert_close(output[1], 0.5 * 0.707);
    }

    #[test]
    fn master_volume_scales_and_limits_without_hard_clipping() {
        let mut transparent = [0.25, -0.5];
        apply_master(&mut transparent, 1.0);
        assert_close(transparent[0], 0.25);
        assert_close(transparent[1], -0.5);

        let mut boosted = [0.25, -0.5];
        apply_master(&mut boosted, 4.0);
        assert!(boosted[0] > 0.9 && boosted[0] < 1.0);
        assert!(boosted[1] < -0.9 && boosted[1] > -1.0);
    }

    #[test]
    fn volume_is_clamped_to_the_supported_range() {
        let xml = r#"<racf channelLayout="stereo"></racf>"#;
        let document = racf_core::parse_document(xml).unwrap();
        let mut mixer = MasterMixer::new(&document, 44_100.0, 2);

        mixer.set_master_volume_percent(250.0);
        assert!((mixer.master_volume_percent() - 200.0).abs() < f32::EPSILON);

        mixer.set_master_volume_percent(-10.0);
        assert!((mixer.master_volume_percent() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn pause_outputs_silence_without_dropping_the_voice() {
        let xml = r#"
            <racf channelLayout="stereo">
                <audio>
                    <clip at="0ms" sampleRate="44100" channels="1" encoding="pcm16">
                        <data>AEA=</data>
                    </clip>
                </audio>
            </racf>
        "#;
        let document = racf_core::parse_document(xml).unwrap();
        let mut mixer = MasterMixer::new(&document, 44_100.0, 2);
        mixer.push_audio(&AudioEvent {
            clip_index: 0,
            clip_id: "clip-0".to_string(),
            gain: 1.0,
            pan: 0.0,
            channel: None,
        });

        mixer.set_paused(true);
        let mut output = [1.0f32; 2];
        mixer.render_f32(&mut output);
        assert_eq!(output, [0.0, 0.0]);

        mixer.set_paused(false);
        mixer.render_f32(&mut output);
        assert!(output.iter().any(|sample| *sample != 0.0));
    }
}
