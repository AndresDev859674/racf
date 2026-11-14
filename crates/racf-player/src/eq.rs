//! Ten-band graphic equalizer.
//!
//! Each band is a peaking biquad placed on an octave grid. The EQ is disabled
//! by default, so playback is bit-transparent until the listener selects a
//! preset or changes a band.

/// Center frequencies of the ten bands, in hertz.
pub const BAND_FREQUENCIES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Short labels matching [`BAND_FREQUENCIES`].
pub const BAND_LABELS: [&str; 10] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

/// Maximum boost or cut for a single band, in decibels.
pub const MAX_BAND_GAIN_DB: f32 = 12.0;

/// Filter quality: roughly one octave of influence per band.
const BAND_Q: f32 = 1.2;

/// A named equalizer curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqPreset {
    Flat,
    Bass,
    Vocal,
    Rock,
    Clarity,
}

impl EqPreset {
    pub const ALL: [EqPreset; 5] = [
        EqPreset::Flat,
        EqPreset::Bass,
        EqPreset::Vocal,
        EqPreset::Rock,
        EqPreset::Clarity,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            EqPreset::Flat => "flat",
            EqPreset::Bass => "bass",
            EqPreset::Vocal => "vocal",
            EqPreset::Rock => "rock",
            EqPreset::Clarity => "clarity",
        }
    }

    /// Gains in decibels, one per [`BAND_FREQUENCIES`] entry.
    pub const fn gains(self) -> [f32; 10] {
        match self {
            EqPreset::Flat => [0.0; 10],
            EqPreset::Bass => [6.0, 5.0, 3.5, 1.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            EqPreset::Vocal => [-3.0, -2.0, -1.0, 0.5, 2.5, 3.5, 3.0, 2.0, 0.5, -1.0],
            EqPreset::Rock => [5.0, 4.0, 2.0, 0.0, -1.0, 0.0, 1.5, 3.0, 4.0, 3.5],
            EqPreset::Clarity => [-3.0, -2.5, -1.5, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 4.0],
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "flat" | "neutral" | "none" => Some(EqPreset::Flat),
            "bass" | "low" | "lows" | "bass-boost" => Some(EqPreset::Bass),
            "vocal" | "voice" | "mid" | "mids" => Some(EqPreset::Vocal),
            "rock" => Some(EqPreset::Rock),
            "clarity" | "clear" | "treble" | "high" | "highs" | "bright" => Some(EqPreset::Clarity),
            _ => None,
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|preset| preset.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Returns the index of the band closest to `frequency`, accepting common
/// spellings such as `1000`, `1000hz`, and `1k`.
pub fn band_index(frequency: f32) -> Option<usize> {
    if !frequency.is_finite() || frequency <= 0.0 {
        return None;
    }

    BAND_FREQUENCIES
        .iter()
        .position(|band| (band - frequency).abs() / band <= 0.15)
}

#[derive(Clone, Copy, Debug)]
struct BiquadCoefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl BiquadCoefficients {
    const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    /// RBJ audio-EQ-cookbook peaking filter.
    ///
    /// Coefficients are kept in `f64`: the 31 Hz and 62 Hz bands have poles
    /// very close to the unit circle at 44.1/48 kHz, where `f32` rounding would
    /// otherwise wash out the intended gain.
    fn peaking(frequency: f32, gain_db: f32, q: f32, sample_rate: f32) -> Self {
        let sample_rate = sample_rate as f64;
        let frequency = frequency as f64;
        let gain_db = gain_db as f64;
        let q = q as f64;

        let nyquist_limit = sample_rate * 0.45;
        if gain_db.abs() < 0.01
            || !frequency.is_finite()
            || frequency <= 0.0
            || frequency >= nyquist_limit
            || sample_rate <= 0.0
        {
            return Self::identity();
        }

        let amplitude = 10.0f64.powf(gain_db / 40.0);
        let omega = 2.0 * std::f64::consts::PI * frequency / sample_rate;
        let alpha = omega.sin() / (2.0 * q);
        let cos = omega.cos();

        let b0 = 1.0 + alpha * amplitude;
        let b1 = -2.0 * cos;
        let b2 = 1.0 - alpha * amplitude;
        let a0 = 1.0 + alpha / amplitude;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha / amplitude;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Direct-form I processing.
    fn process(&self, state: &mut BiquadState, input: f64) -> f64 {
        let output = self.b0 * input + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;

        state.x2 = state.x1;
        state.x1 = input;
        state.y2 = state.y1;
        state.y1 = output;
        output
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

/// Per-channel cascaded biquad state for all ten bands.
#[derive(Clone, Debug)]
pub struct GraphicEq {
    enabled: bool,
    preset: Option<EqPreset>,
    gains_db: [f32; 10],
    coefficients: [BiquadCoefficients; 10],
    states: Vec<[BiquadState; 10]>,
    sample_rate: f32,
}

impl GraphicEq {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let mut eq = Self {
            enabled: false,
            preset: None,
            gains_db: [0.0; 10],
            coefficients: [BiquadCoefficients::identity(); 10],
            states: vec![[BiquadState::default(); 10]; channels.max(1)],
            sample_rate,
        };
        eq.recompute();
        eq
    }

    /// Ensures one filter chain exists per document channel.
    pub fn set_channels(&mut self, channels: usize) {
        let channels = channels.max(1);
        if self.states.len() != channels {
            self.states = vec![[BiquadState::default(); 10]; channels];
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn preset(&self) -> Option<EqPreset> {
        self.preset
    }

    pub fn gains(&self) -> [f32; 10] {
        self.gains_db
    }

    /// Human-readable state for the status line (`off`, `flat`, `bass`,
    /// `custom`, ...).
    pub fn label(&self) -> String {
        if !self.enabled {
            return "off".to_string();
        }
        self.preset
            .map(|preset| preset.name().to_string())
            .unwrap_or_else(|| "custom".to_string())
    }

    pub fn set_preset(&mut self, preset: EqPreset) {
        self.gains_db = preset.gains();
        self.preset = Some(preset);
        self.enabled = true;
        self.recompute();
    }

    /// Applies an absolute gain to one band. Returns `false` when the index is
    /// out of range.
    pub fn set_band_gain(&mut self, index: usize, gain_db: f32) -> bool {
        if index >= BAND_FREQUENCIES.len() {
            return false;
        }

        self.gains_db[index] = gain_db.clamp(-MAX_BAND_GAIN_DB, MAX_BAND_GAIN_DB);
        self.preset = None;
        self.enabled = true;
        self.coefficients[index] = BiquadCoefficients::peaking(
            BAND_FREQUENCIES[index],
            self.gains_db[index],
            BAND_Q,
            self.sample_rate,
        );
        true
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Clears filter memory. Call this after a seek so old samples do not ring
    /// into the new position.
    pub fn reset(&mut self) {
        for states in &mut self.states {
            *states = [BiquadState::default(); 10];
        }
    }

    /// Filters one interleaved frame of the document bus in place.
    pub fn process(&mut self, bus: &mut [f32]) {
        if !self.enabled {
            return;
        }

        for (channel, sample) in bus.iter_mut().enumerate() {
            let Some(states) = self.states.get_mut(channel) else {
                continue;
            };

            let mut value = f64::from(*sample);
            for (state, coefficients) in states.iter_mut().zip(self.coefficients.iter()) {
                value = coefficients.process(state, value);
            }
            *sample = value as f32;
        }
    }

    fn recompute(&mut self) {
        for (index, coefficients) in self.coefficients.iter_mut().enumerate() {
            *coefficients = BiquadCoefficients::peaking(
                BAND_FREQUENCIES[index],
                self.gains_db[index],
                BAND_Q,
                self.sample_rate,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn sine(frequency: f32, sample_rate: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                (2.0 * std::f32::consts::PI * frequency * frame as f32 / sample_rate).sin() * 0.5
            })
            .collect()
    }

    fn process_mono(eq: &mut GraphicEq, samples: &mut [f32]) {
        for sample in samples {
            eq.process(std::slice::from_mut(sample));
        }
    }

    #[test]
    fn disabled_eq_is_transparent() {
        let mut eq = GraphicEq::new(48_000.0, 1);
        let input = sine(1_000.0, 48_000.0, 1_000);
        let mut output = input.clone();

        process_mono(&mut eq, &mut output);

        assert_eq!(input, output);
    }

    #[test]
    fn bass_preset_boosts_lows_more_than_highs() {
        let mut eq = GraphicEq::new(48_000.0, 1);
        eq.set_preset(EqPreset::Bass);

        let low = sine(62.0, 48_000.0, 12_000);
        let high = sine(8_000.0, 48_000.0, 12_000);
        let mut low_out = low.clone();
        let mut high_out = high.clone();
        process_mono(&mut eq, &mut low_out);
        eq.reset();
        process_mono(&mut eq, &mut high_out);

        // Skip the filter's start-up transient.
        let low_gain = rms(&low_out[2_000..]) / rms(&low[2_000..]);
        let high_gain = rms(&high_out[2_000..]) / rms(&high[2_000..]);

        assert!(low_gain > 1.5, "low gain was {low_gain}");
        assert!((high_gain - 1.0).abs() < 0.1, "high gain was {high_gain}");
    }

    #[test]
    fn presets_and_custom_bands_update_the_label() {
        let mut eq = GraphicEq::new(44_100.0, 2);
        assert_eq!(eq.label(), "off");

        eq.set_preset(EqPreset::Rock);
        assert_eq!(eq.label(), "rock");
        assert!(eq.is_enabled());

        assert!(eq.set_band_gain(5, 4.0));
        assert_eq!(eq.label(), "custom");
        assert!((eq.gains()[5] - 4.0).abs() < f32::EPSILON);

        eq.disable();
        assert_eq!(eq.label(), "off");
    }

    #[test]
    fn band_gain_is_clamped_and_indexes_are_validated() {
        let mut eq = GraphicEq::new(48_000.0, 1);

        assert!(eq.set_band_gain(0, 40.0));
        assert!((eq.gains()[0] - MAX_BAND_GAIN_DB).abs() < f32::EPSILON);

        assert!(eq.set_band_gain(0, -40.0));
        assert!((eq.gains()[0] + MAX_BAND_GAIN_DB).abs() < f32::EPSILON);

        assert!(!eq.set_band_gain(99, 1.0));
    }

    #[test]
    fn parses_preset_names() {
        assert_eq!(EqPreset::parse("Bass"), Some(EqPreset::Bass));
        assert_eq!(EqPreset::parse("treble"), Some(EqPreset::Clarity));
        assert_eq!(EqPreset::parse("unknown"), None);
    }

    #[test]
    fn resolves_band_frequencies() {
        assert_eq!(band_index(62.0), Some(1));
        assert_eq!(band_index(60.0), Some(1));
        assert_eq!(band_index(1000.0), Some(5));
        assert_eq!(band_index(16_000.0), Some(9));
        assert_eq!(band_index(300.0), None);
    }
}
