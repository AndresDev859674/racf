use racf_core::{Channel, ChannelLayout, DrumType, NoteEvent, SoundSource, Waveform};

use crate::dsp::{noise_bipolar, rand_noise, ResonantFilter, VocalFormants};

/// The synthesis engine selected for a voice.
#[derive(Clone, Debug)]
enum VoiceEngine {
    Oscillator(Waveform),
    Drum { kind: DrumType, electronic: bool },
    Piano,
    Synth,
    Trumpet,
    Otamatone,
    Scratch,
    Vocal(VocalFormants),
}

/// A single sounding voice. Voices are created from compiled [`NoteEvent`]s
/// and advanced one sample at a time by the realtime mixer.
#[derive(Clone, Debug)]
pub struct Voice {
    pub track_id: String,
    pub pan: f32,
    pub channel: Option<Channel>,
    active: bool,
    freq: f32,
    phase: f32,
    phase2: f32,
    sample_rate: f32,
    duration_samples: usize,
    current_sample: usize,
    distortion: f32,
    gain: f32,
    engine: VoiceEngine,
    filter: ResonantFilter,
}

impl Voice {
    pub fn from_event(event: &NoteEvent, sample_rate: f32) -> Self {
        Self::from_event_at(event, sample_rate, 0.0)
    }

    /// Creates a voice that starts `offset_secs` into its note, used when
    /// seeking. The envelope continues from the new position instead of
    /// restarting the attack.
    pub fn from_event_at(event: &NoteEvent, sample_rate: f32, offset_secs: f32) -> Self {
        let (engine, distortion) = match &event.source {
            SoundSource::Oscillator {
                waveform,
                distortion,
            } => (VoiceEngine::Oscillator(*waveform), *distortion),
            SoundSource::Piano => (VoiceEngine::Piano, 1.0),
            SoundSource::Synth => (VoiceEngine::Synth, 1.0),
            SoundSource::Trumpet => (VoiceEngine::Trumpet, 1.0),
            SoundSource::Otamatone => (VoiceEngine::Otamatone, 1.0),
            SoundSource::Scratch => (VoiceEngine::Scratch, 1.0),
            SoundSource::Drum { kind, electronic } => (
                VoiceEngine::Drum {
                    kind: *kind,
                    electronic: *electronic,
                },
                1.0,
            ),
            SoundSource::Vocal(params) => (VoiceEngine::Vocal(VocalFormants::new(params)), 1.0),
        };

        let duration_samples = (event.duration_secs.max(0.0) * sample_rate)
            .round()
            .max(1.0) as usize;
        let offset_samples = (offset_secs.max(0.0) * sample_rate).round() as usize;
        let active = offset_samples < duration_samples;

        Self {
            track_id: event.track_id.clone(),
            pan: event.pan,
            channel: event.channel,
            active,
            freq: event.frequency,
            phase: 0.0,
            phase2: 0.0,
            sample_rate,
            duration_samples,
            current_sample: offset_samples.min(duration_samples),
            distortion,
            gain: event.gain,
            engine,
            filter: ResonantFilter::new(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn next_sample(&mut self) -> f32 {
        if !self.active {
            return 0.0;
        }

        self.current_sample += 1;
        if self.current_sample >= self.duration_samples {
            self.active = false;
            return 0.0;
        }

        let t = self.current_sample as f32 / self.sample_rate;
        let t_norm = (self.current_sample as f32 / self.duration_samples as f32).clamp(0.0, 1.0);
        let env_lin = 1.0 - t_norm;

        match &mut self.engine {
            // Vinyl Scratch Effect Engine
            VoiceEngine::Scratch => {
                let base = if self.freq > 0.0 { self.freq } else { 350.0 };

                let pitch_mult = if t_norm < 0.25 {
                    0.8 + (t_norm / 0.25) * 1.4
                } else if t_norm < 0.55 {
                    2.2 - ((t_norm - 0.25) / 0.30) * 1.6
                } else {
                    0.6 + ((t_norm - 0.55) / 0.45) * 2.0
                };

                let current_freq = base * pitch_mult;
                self.phase = (self.phase + current_freq / self.sample_rate) % 1.0;

                let saw = 2.0 * self.phase - 1.0;
                let raw_scratch = saw * 0.8 + noise_bipolar() * 0.15;

                let filter_freq = 600.0 + 1800.0 * (pitch_mult / 2.2);
                let filtered =
                    self.filter
                        .process(raw_scratch, filter_freq, 140.0, self.sample_rate);

                filtered * 2.2 * env_lin.powf(0.5) * self.gain
            }

            // Electronic Synth Drums (TR-808/909 style)
            VoiceEngine::Drum {
                kind,
                electronic: true,
            } => match kind {
                DrumType::Kick => {
                    let env_sub = (-t * 12.0).exp();
                    let freq_sweep = 140.0 * (-t * 35.0).exp() + 38.0;
                    self.phase = (self.phase + freq_sweep / self.sample_rate) % 1.0;
                    let sub_sine = (self.phase * 2.0 * std::f32::consts::PI).sin();
                    let click = if self.current_sample < (self.sample_rate * 0.002) as usize {
                        0.4
                    } else {
                        0.0
                    };
                    (sub_sine * env_sub + click) * 1.4 * self.gain
                }
                DrumType::Snare => {
                    let env_body = (-t * 28.0).exp();
                    let env_snap = (-t * 22.0).exp();
                    self.phase = (self.phase + 210.0 / self.sample_rate) % 1.0;
                    let body = (self.phase * 2.0 * std::f32::consts::PI).sin() * env_body;
                    let noise = noise_bipolar() * env_snap * 0.6;
                    let filtered_noise =
                        self.filter.process(noise, 2200.0, 300.0, self.sample_rate);
                    (body * 0.6 + filtered_noise * 0.8) * 1.2 * self.gain
                }
                DrumType::HiHat => {
                    let env = (-t * 55.0).exp();
                    let noise = noise_bipolar() * env;
                    let filtered = self.filter.process(noise, 7500.0, 400.0, self.sample_rate);
                    filtered * 0.8 * self.gain
                }
                DrumType::China => {
                    let env = (-t * 4.0).exp();
                    let noise = noise_bipolar() * env;
                    let filtered = self.filter.process(noise, 5000.0, 350.0, self.sample_rate);
                    filtered * 0.9 * self.gain
                }
                DrumType::Tom => {
                    let env = (-t * 14.0).exp();
                    let freq = 160.0 * (-t * 20.0).exp() + 70.0;
                    self.phase = (self.phase + freq / self.sample_rate) % 1.0;
                    (self.phase * 2.0 * std::f32::consts::PI).sin() * env * self.gain
                }
            },

            // Subtractive Synth Engine
            VoiceEngine::Synth => {
                let f = self.freq;
                self.phase = (self.phase + f / self.sample_rate) % 1.0;
                self.phase2 = (self.phase2 + (f * 1.002) / self.sample_rate) % 1.0;

                let tri = 2.0 * (self.phase - 0.5).abs() - 1.0;
                let saw = 2.0 * self.phase2 - 1.0;
                let smooth_wave = tri * 0.6 + saw * 0.4;

                let cutoff = 600.0 + 2400.0 * env_lin.powf(0.8);
                let filtered = self
                    .filter
                    .process(smooth_wave, cutoff, 180.0, self.sample_rate);

                filtered * 1.8 * env_lin.powf(0.6) * self.gain
            }

            // Otamatone Instrument Engine
            VoiceEngine::Otamatone => {
                let vibrato = (t * 7.5 * 2.0 * std::f32::consts::PI).sin() * 7.0;
                let current_freq = (self.freq + vibrato).max(20.0);

                self.phase = (self.phase + current_freq / self.sample_rate) % 1.0;

                let sine = (self.phase * 2.0 * std::f32::consts::PI).sin();
                let tri = (2.0 * (self.phase - 0.5).abs() - 1.0) * 0.5;
                let raw_toy = sine * 0.75 + tri * 0.25;

                let mouth_wah = (t * 5.0 * std::f32::consts::PI).sin() * 0.5 + 0.5;
                let cutoff = 400.0 + 1800.0 * mouth_wah;

                let filtered = self
                    .filter
                    .process(raw_toy, cutoff, 160.0, self.sample_rate);
                (filtered * 24.0).clamp(-0.9, 0.9) * env_lin.powf(0.7) * self.gain
            }

            // Formant Vocal Engine
            VoiceEngine::Vocal(voc) => {
                let vibrato = if voc.vibrato_depth() > 0.0 {
                    (t * voc.vibrato_rate() * 2.0 * std::f32::consts::PI).sin()
                        * voc.vibrato_depth()
                        * 10.0
                } else {
                    0.0
                };

                let current_freq = self.freq + vibrato;
                self.phase = (self.phase + current_freq / self.sample_rate) % 1.0;
                self.phase2 = (self.phase2 + (current_freq * 0.5) / self.sample_rate) % 1.0;

                let open_ratio = 0.65f32;
                let glottal = if self.phase < open_ratio {
                    let p = self.phase / open_ratio;
                    (std::f32::consts::PI * p).sin().powi(2)
                } else {
                    let p = (self.phase - open_ratio) / (1.0 - open_ratio);
                    1.0 - p
                };

                let air_noise = noise_bipolar() * voc.breath() * 0.45;
                let sub_grit = (self.phase2 * 2.0 * std::f32::consts::PI).sin() * voc.grit() * 0.35;
                let excitation = (glottal * 2.0 - 1.0) + air_noise + sub_grit;

                let (o1, o2, o3, o4, nasal) = voc.process(excitation, self.sample_rate);
                let vocal_mix = o1 + o2 * 0.7 + o3 * 0.4 + o4 * 0.2 + nasal;
                let vocal_env = (t * 25.0).min(1.0) * env_lin.powf(0.8);

                (vocal_mix * 2.5).tanh() * vocal_env * self.gain
            }

            // Brass Instrument Engine
            VoiceEngine::Trumpet => {
                let f = self.freq;
                self.phase = (self.phase + f / self.sample_rate) % 1.0;
                self.phase2 = (self.phase2 + (f * 1.002) / self.sample_rate) % 1.0;

                let saw = 2.0 * self.phase - 1.0;
                let square = if self.phase2 < 0.5 { 0.7 } else { -0.7 };
                let raw = saw * 0.6 + square * 0.4;

                let attack_env = (t * 40.0).min(1.0);
                let cutoff = 600.0 + 3500.0 * attack_env;
                let filtered = self.filter.process(raw, cutoff, 250.0, self.sample_rate);

                (filtered * 35.0).clamp(-0.9, 0.9) * env_lin.powf(0.6) * self.gain
            }

            // Acoustic Piano Engine
            VoiceEngine::Piano => {
                let f = self.freq;
                let hammer = if self.current_sample < (self.sample_rate * 0.003) as usize {
                    noise_bipolar() * 0.35
                } else {
                    0.0
                };

                let env_f = (-t * (1.8 + f * 0.0015)).exp();
                let env_h2 = (-t * (4.5 + f * 0.004)).exp();
                let env_h3 = (-t * (9.0 + f * 0.008)).exp();

                self.phase = (self.phase + f / self.sample_rate) % 1.0;
                self.phase2 = (self.phase2 + (f * 2.0015) / self.sample_rate) % 1.0;

                let fundamental = (self.phase * 2.0 * std::f32::consts::PI).sin() * env_f;
                let partial2 = (self.phase2 * 2.0 * std::f32::consts::PI).sin() * 0.35 * env_h2;
                let partial3 =
                    (self.phase * 3.003 * 2.0 * std::f32::consts::PI).sin() * 0.15 * env_h3;

                let piano_sample = (fundamental + partial2 + partial3 + hammer) * 0.85;
                piano_sample * env_lin.powf(0.5) * self.gain
            }

            // Standard Acoustic Drums Engine
            VoiceEngine::Drum {
                kind,
                electronic: false,
            } => {
                let raw = match kind {
                    DrumType::Kick => {
                        let env = (-t * 18.0).exp();
                        let freq = 160.0 * (-t * 40.0).exp() + 45.0;
                        self.phase = (self.phase + freq / self.sample_rate) % 1.0;
                        let body = (self.phase * 2.0 * std::f32::consts::PI).sin();
                        let click = if self.current_sample < (self.sample_rate * 0.004) as usize {
                            noise_bipolar() * 0.8
                        } else {
                            0.0
                        };
                        (body * env + click) * 1.3
                    }
                    DrumType::Snare => {
                        let env_noise = (-t * 16.0).exp();
                        let env_body = (-t * 22.0).exp();

                        self.phase = (self.phase + 185.0 / self.sample_rate) % 1.0;
                        self.phase2 = (self.phase2 + 340.0 / self.sample_rate) % 1.0;
                        let body = ((self.phase * 2.0 * std::f32::consts::PI).sin()
                            + (self.phase2 * 2.0 * std::f32::consts::PI).sin() * 0.4)
                            * env_body;

                        let noise = noise_bipolar() * env_noise * 0.7;
                        (body * 0.5 + noise * 0.7) * 1.2
                    }
                    DrumType::HiHat => {
                        let env = (-t * 45.0).exp();
                        noise_bipolar() * env * 0.5
                    }
                    DrumType::China => {
                        let env = (-t * 6.0).exp();
                        let noise = noise_bipolar() * 0.6;
                        self.phase = (self.phase + 820.0 / self.sample_rate) % 1.0;
                        let metallic = (self.phase * 2.0 * std::f32::consts::PI).sin() * 0.3;
                        (noise + metallic) * env * 0.8
                    }
                    DrumType::Tom => {
                        let env = (-t * 10.0).exp();
                        let freq = 120.0 * (-t * 15.0).exp() + 60.0;
                        self.phase = (self.phase + freq / self.sample_rate) % 1.0;
                        (self.phase * 2.0 * std::f32::consts::PI).sin() * env
                    }
                };

                let processed = if self.distortion > 1.0 {
                    (raw * self.distortion).clamp(-0.85, 0.85)
                } else {
                    raw
                };

                processed * self.gain
            }

            // Plain oscillator engine
            VoiceEngine::Oscillator(waveform) => {
                self.phase = (self.phase + self.freq / self.sample_rate) % 1.0;

                let raw = match waveform {
                    Waveform::Sine => (self.phase * 2.0 * std::f32::consts::PI).sin(),
                    Waveform::Sawtooth => 2.0 * self.phase - 1.0,
                    Waveform::Square => {
                        if self.phase < 0.5 {
                            0.8
                        } else {
                            -0.8
                        }
                    }
                    Waveform::Noise => rand_noise() * 2.0 - 1.0,
                } * env_lin;

                let processed = if self.distortion > 1.0 {
                    (raw * self.distortion).clamp(-0.85, 0.85)
                } else {
                    raw
                };

                processed * self.gain
            }
        }
    }

    /// Adds this voice to the correct output bus channel(s).
    pub fn add_to_bus(&self, sample: f32, bus: &mut [f32], layout: &ChannelLayout) {
        if let Some(channel) = self.channel {
            if let Some(index) = layout.index_of(channel) {
                bus[index] += sample;
                return;
            }
        }

        if let Some((left, right)) = layout.front_pair() {
            let pan = (self.pan.clamp(-1.0, 1.0) + 1.0) * 0.5;
            let left_gain = (1.0 - pan).sqrt();
            let right_gain = pan.sqrt();
            bus[left] += sample * left_gain;
            bus[right] += sample * right_gain;
        } else if let Some(index) = layout.center_index() {
            bus[index] += sample;
        } else if let Some(first) = bus.first_mut() {
            *first += sample;
        }
    }
}
