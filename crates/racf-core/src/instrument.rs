/// A periodic waveform used by the oscillator engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Waveform {
    Sine,
    #[default]
    Sawtooth,
    Square,
    Noise,
}

impl Waveform {
    pub fn from_name(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "sine" | "sin" => Waveform::Sine,
            "square" => Waveform::Square,
            "noise" | "white-noise" | "white_noise" => Waveform::Noise,
            _ => Waveform::Sawtooth,
        }
    }
}

/// The percussion voices understood by the drum engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrumType {
    Kick,
    Snare,
    HiHat,
    China,
    Tom,
}

impl DrumType {
    pub fn from_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "KICK" | "BD" | "BASSDRUM" => Some(DrumType::Kick),
            "SNARE" | "SD" => Some(DrumType::Snare),
            "HIHAT" | "HH" | "CLOSEDHAT" => Some(DrumType::HiHat),
            "CHINA" | "CRASH" | "CYMBAL" => Some(DrumType::China),
            "TOM" | "TT" => Some(DrumType::Tom),
            _ => None,
        }
    }
}

/// Parameters for the formant vocal engine.
#[derive(Clone, Debug, PartialEq)]
pub struct VocalParams {
    pub vowel: String,
    pub formant_shift: f32,
    pub breath: f32,
    pub nasal: f32,
    pub grit: f32,
    pub vibrato_rate: f32,
    pub vibrato_depth: f32,
}

impl Default for VocalParams {
    fn default() -> Self {
        Self {
            vowel: "AH".to_string(),
            formant_shift: 0.9,
            breath: 0.25,
            nasal: 0.5,
            grit: 0.2,
            vibrato_rate: 6.5,
            vibrato_depth: 0.35,
        }
    }
}

impl VocalParams {
    pub fn new(vowel: impl Into<String>) -> Self {
        Self {
            vowel: vowel.into(),
            ..Self::default()
        }
    }
}

/// The family of synthesis engine used by an instrument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstrumentKind {
    #[default]
    Oscillator,
    Piano,
    Synth,
    Trumpet,
    Otamatone,
    Scratch,
    ElectroDrum,
    Vocal,
}

impl InstrumentKind {
    pub fn from_name(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "piano" | "grand-piano" => InstrumentKind::Piano,
            "synth" | "synthesizer" => InstrumentKind::Synth,
            "trumpet" | "brass" => InstrumentKind::Trumpet,
            "otamatone" => InstrumentKind::Otamatone,
            "scratch" | "turntable" => InstrumentKind::Scratch,
            "electrodrums" | "edrums" | "electronic-drums" => InstrumentKind::ElectroDrum,
            "vocal" | "voice" => InstrumentKind::Vocal,
            _ => InstrumentKind::Oscillator,
        }
    }
}

/// A parsed `<instrument>` definition, or one of the built-in presets.
#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentDef {
    pub waveform: Waveform,
    pub distortion: f32,
    pub kind: InstrumentKind,
    pub vocal: Option<VocalParams>,
}

impl Default for InstrumentDef {
    fn default() -> Self {
        Self {
            waveform: Waveform::Sawtooth,
            distortion: 1.0,
            kind: InstrumentKind::Oscillator,
            vocal: None,
        }
    }
}

impl InstrumentDef {
    /// Resolves the built-in `import::` presets.
    pub fn preset(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        let kind = match name.as_str() {
            "import::piano" => InstrumentKind::Piano,
            "import::synth" => InstrumentKind::Synth,
            "import::trumpet" | "import::brass" => InstrumentKind::Trumpet,
            "import::otamatone" => InstrumentKind::Otamatone,
            "import::scratch" | "import::turntable" => InstrumentKind::Scratch,
            "import::electrodrums" | "import::edrums" => InstrumentKind::ElectroDrum,
            "import::voice" | "import::vocal" => InstrumentKind::Vocal,
            "import::battery" | "import::bass" => InstrumentKind::Oscillator,
            _ if name.starts_with("import::") => InstrumentKind::Oscillator,
            _ => return None,
        };

        Some(Self {
            kind,
            vocal: (kind == InstrumentKind::Vocal).then(VocalParams::default),
            ..Self::default()
        })
    }

    pub fn source_is_vocal(&self) -> bool {
        self.kind == InstrumentKind::Vocal
    }
}
