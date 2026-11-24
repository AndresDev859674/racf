use std::cell::Cell;

use racf_core::VocalParams;

/// A two-pole resonant filter used by the synth, drum, scratch, and vocal
/// engines.
#[derive(Clone, Debug)]
pub struct ResonantFilter {
    y1: f32,
    y2: f32,
}

impl Default for ResonantFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ResonantFilter {
    pub fn new() -> Self {
        Self { y1: 0.0, y2: 0.0 }
    }

    pub fn process(&mut self, input: f32, freq: f32, bandwidth: f32, sample_rate: f32) -> f32 {
        let safe_freq = freq.clamp(20.0, sample_rate * 0.48);
        let r = (-std::f32::consts::PI * (bandwidth / sample_rate)).exp();
        let theta = 2.0 * std::f32::consts::PI * (safe_freq / sample_rate);
        let a1 = -2.0 * r * theta.cos();
        let a2 = r * r;
        let b0 = (1.0 - r) * (1.0 - a2.sqrt());

        let output = b0 * input - a1 * self.y1 - a2 * self.y2;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

/// Runtime state for the formant vocal engine.
#[derive(Clone, Debug)]
pub struct VocalFormants {
    f1: f32,
    f2: f32,
    f3: f32,
    f4: f32,
    breath: f32,
    nasal: f32,
    grit: f32,
    vibrato_rate: f32,
    vibrato_depth: f32,
    f1_filt: ResonantFilter,
    f2_filt: ResonantFilter,
    f3_filt: ResonantFilter,
    f4_filt: ResonantFilter,
    nasal_filt: ResonantFilter,
}

impl VocalFormants {
    pub fn new(params: &VocalParams) -> Self {
        let (f1_base, f2_base, f3_base, f4_base) = match params.vowel.to_uppercase().as_str() {
            "AH" | "YEAH" => (730.0, 1090.0, 2440.0, 3400.0),
            "EH" => (530.0, 1840.0, 2480.0, 3500.0),
            "OH" => (570.0, 840.0, 2410.0, 3300.0),
            "EE" => (270.0, 2290.0, 3010.0, 3700.0),
            "OO" => (300.0, 870.0, 2240.0, 3300.0),
            "AY" => (610.0, 1900.0, 2600.0, 3500.0),
            _ => (700.0, 1200.0, 2500.0, 3400.0),
        };

        let shift = params.formant_shift.clamp(0.6, 1.6);

        Self {
            f1: f1_base * shift,
            f2: f2_base * shift,
            f3: f3_base * shift,
            f4: f4_base * shift,
            breath: params.breath,
            nasal: params.nasal,
            grit: params.grit,
            vibrato_rate: params.vibrato_rate,
            vibrato_depth: params.vibrato_depth,
            f1_filt: ResonantFilter::new(),
            f2_filt: ResonantFilter::new(),
            f3_filt: ResonantFilter::new(),
            f4_filt: ResonantFilter::new(),
            nasal_filt: ResonantFilter::new(),
        }
    }

    pub fn vibrato_rate(&self) -> f32 {
        self.vibrato_rate
    }

    pub fn vibrato_depth(&self) -> f32 {
        self.vibrato_depth
    }

    pub fn breath(&self) -> f32 {
        self.breath
    }

    pub fn grit(&self) -> f32 {
        self.grit
    }

    pub fn nasal(&self) -> f32 {
        self.nasal
    }

    pub fn process(&mut self, excitation: f32, sample_rate: f32) -> (f32, f32, f32, f32, f32) {
        let o1 = self.f1_filt.process(excitation, self.f1, 80.0, sample_rate);
        let o2 = self.f2_filt.process(excitation, self.f2, 90.0, sample_rate);
        let o3 = self
            .f3_filt
            .process(excitation, self.f3, 120.0, sample_rate);
        let o4 = self
            .f4_filt
            .process(excitation, self.f4, 160.0, sample_rate);

        let nasal = if self.nasal > 0.0 {
            self.nasal_filt
                .process(excitation, 2800.0, 110.0, sample_rate)
                * self.nasal
                * 1.2
        } else {
            0.0
        };

        (o1, o2, o3, o4, nasal)
    }
}

thread_local! {
    static NOISE_STATE: Cell<u32> = const { Cell::new(0x1234_5678) };
}

/// Deterministic, cheap white noise in the range `0.0..=1.0`.
pub fn rand_noise() -> f32 {
    NOISE_STATE.with(|state| {
        let seed = state
            .get()
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        state.set(seed);
        seed as f32 / u32::MAX as f32
    })
}

/// White noise in the range `-1.0..=1.0`.
pub fn noise_bipolar() -> f32 {
    rand_noise() * 2.0 - 1.0
}
