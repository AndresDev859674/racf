use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use roxmltree::{Document as XmlDocument, Node};

use crate::audio::{parse_audio, AudioClip};
use crate::channel::{Channel, ChannelLayout};
use crate::duration::{parse_duration, parse_target_duration};
use crate::error::RacfError;
use crate::instrument::{DrumType, InstrumentDef, InstrumentKind, VocalParams, Waveform};
use crate::lyrics::{parse_lyrics, Lyrics};
use crate::metadata::{parse_metadata, Metadata};

/// Reads a RACF file into a pre-sized string.
///
/// `fs::read_to_string` grows its buffer geometrically, which briefly needs
/// almost twice the file size for large embedded clips. Reserving the exact
/// file length up front keeps the parse peak close to the file size.
pub fn read_document_file(path: impl AsRef<Path>) -> io::Result<String> {
    let mut file = File::open(path)?;
    let capacity = usize::try_from(file.metadata()?.len()).unwrap_or(0);
    let mut source = String::with_capacity(capacity);
    file.read_to_string(&mut source)?;
    Ok(source)
}

pub const DEFAULT_TEMPO: f32 = 128.0;
pub const DEFAULT_BIT_DEPTH: u8 = 16;
pub const DEFAULT_BITRATE_KBPS: u32 = 320;

/// The top-level RACF document.
#[derive(Clone, Debug)]
pub struct Document {
    pub version: Option<String>,
    pub tempo: f32,
    pub time_signature: TimeSignature,
    pub bit_depth: u8,
    pub bitrate_kbps: u32,
    pub target_sample_rate: Option<f32>,
    pub target_duration_secs: Option<f32>,
    pub channel_layout: ChannelLayout,
    pub metadata: Metadata,
    /// Optional lyrics, either timed (LRC/SRT) or static text.
    pub lyrics: Option<Lyrics>,
    pub imports: Vec<Import>,
    pub instruments: HashMap<String, InstrumentDef>,
    pub tracks: Vec<Track>,
    pub audio_clips: Vec<AudioClip>,
}

impl Document {
    /// Resolves a track's `instrument` reference through imports and local
    /// definitions. Unknown instruments fall back to a plain sawtooth
    /// oscillator.
    pub fn resolve_instrument(&self, name: &str) -> InstrumentDef {
        if let Some(instrument) = self.instruments.get(name) {
            return instrument.clone();
        }

        let target = self
            .imports
            .iter()
            .find(|import| import.alias == name)
            .map(|import| import.src.as_str())
            .unwrap_or(name);

        if let Some(instrument) = self.instruments.get(target) {
            return instrument.clone();
        }

        InstrumentDef::preset(target).unwrap_or_default()
    }

    /// The number of beats in the longest track. Used by `targetDuration`.
    pub fn max_track_beats(&self) -> f32 {
        self.tracks
            .iter()
            .map(Track::total_beats)
            .fold(0.0, f32::max)
    }

    /// The tempo after applying `targetDuration`, if present.
    pub fn effective_tempo(&self) -> f32 {
        if let Some(target_secs) = self.target_duration_secs {
            let beats = self.max_track_beats();
            if beats > 0.0 && target_secs > 0.0 {
                return (beats / target_secs) * 60.0;
            }
        }
        self.tempo
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeSignature {
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self {
            numerator: 4,
            denominator: 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    pub src: String,
    pub alias: String,
}

#[derive(Clone, Debug)]
pub struct Track {
    /// The name written in the document (`instrument="..."`), kept as the
    /// stable track identity used by stem isolation.
    pub instrument: String,
    pub gain: f32,
    pub pan: f32,
    pub channel: Option<Channel>,
    pub items: Vec<TrackItem>,
}

impl Track {
    pub fn total_beats(&self) -> f32 {
        self.items.iter().map(TrackItem::beats).sum()
    }
}

#[derive(Clone, Debug)]
pub enum TrackItem {
    Tempo(f32),
    Gain(f32),
    FadeIn {
        beats: f32,
    },
    FadeOut {
        beats: f32,
    },
    Separation {
        active: bool,
        target: Option<String>,
    },
    Note(NoteSpec),
    Lyric(LyricSpec),
    Chord(ChordSpec),
    Rest(RestSpec),
    Sequence(SequenceSpec),
    Beat(BeatSpec),
}

impl TrackItem {
    fn beats(&self) -> f32 {
        match self {
            TrackItem::Note(note) => note.beats,
            TrackItem::Lyric(lyric) => lyric.beats,
            TrackItem::Chord(chord) => chord.beats,
            TrackItem::Rest(rest) => rest.beats,
            TrackItem::Sequence(sequence) => sequence.notes.iter().map(|note| note.beats).sum(),
            TrackItem::Beat(beat) => beat.steps.iter().map(|step| step.beats).sum(),
            TrackItem::Tempo(_)
            | TrackItem::Gain(_)
            | TrackItem::FadeIn { .. }
            | TrackItem::FadeOut { .. }
            | TrackItem::Separation { .. } => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NoteSpec {
    pub pitch: String,
    pub beats: f32,
    pub bpm: Option<f32>,
    pub pan: Option<f32>,
    pub channel: Option<Channel>,
    pub velocity: f32,
}

#[derive(Clone, Debug)]
pub struct ChordSpec {
    pub pitches: Vec<String>,
    pub beats: f32,
    pub bpm: Option<f32>,
    pub pan: Option<f32>,
    pub channel: Option<Channel>,
    pub velocity: f32,
}

#[derive(Clone, Debug)]
pub struct LyricSpec {
    pub pitch: String,
    pub vowel: String,
    pub beats: f32,
    pub bpm: Option<f32>,
    pub pan: Option<f32>,
    pub channel: Option<Channel>,
    pub velocity: f32,
    pub vocal: VocalParams,
}

#[derive(Clone, Debug)]
pub struct RestSpec {
    pub beats: f32,
    pub bpm: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct SequenceSpec {
    pub bpm: Option<f32>,
    pub notes: Vec<NoteSpec>,
}

#[derive(Clone, Debug)]
pub struct BeatSpec {
    pub bpm: Option<f32>,
    pub channel: Option<Channel>,
    pub steps: Vec<BeatStep>,
}

#[derive(Clone, Debug)]
pub struct BeatStep {
    pub drums: Vec<DrumType>,
    pub beats: f32,
    pub pan: Option<f32>,
}

/// Parses a complete RACF document.
pub fn parse_document(xml: &str) -> Result<Document, RacfError> {
    let xml_document = XmlDocument::parse(xml)?;
    let root = xml_document.root_element();

    if !root.has_tag_name("racf") {
        return Err(RacfError::InvalidRoot(root.tag_name().name().to_string()));
    }

    let tempo = attr_f32(root, "tempo").unwrap_or(DEFAULT_TEMPO);
    let bit_depth = attr_u8(root, "bitDepth").unwrap_or(DEFAULT_BIT_DEPTH);
    let bitrate_kbps = parse_bitrate(root.attribute("bitrate").unwrap_or("320k"));
    let time_signature = root
        .attribute("timeSignature")
        .map(parse_time_signature)
        .unwrap_or_default();
    let target_sample_rate = root
        .attribute("sampleRate")
        .and_then(|value| value.trim().parse::<f32>().ok())
        .or_else(|| attr_f32(root, "kHz").map(|khz| khz * 1000.0));
    let target_duration_secs = root
        .attribute("targetDuration")
        .and_then(parse_target_duration);
    validate_channel_attributes(root)?;
    let channel_layout = parse_channel_layout(root)?;
    let metadata = parse_metadata(root);
    let lyrics = parse_lyrics(root)?;
    let imports = parse_imports(root);
    let instruments = parse_definitions(root);
    let tracks = parse_score(root);
    let audio_clips = parse_audio(root)?;

    Ok(Document {
        version: root.attribute("version").map(str::to_string),
        tempo,
        time_signature,
        bit_depth,
        bitrate_kbps,
        target_sample_rate,
        target_duration_secs,
        channel_layout,
        metadata,
        lyrics,
        imports,
        instruments,
        tracks,
        audio_clips,
    })
}

fn parse_channel_layout(root: Node<'_, '_>) -> Result<ChannelLayout, RacfError> {
    if let Some(channels_node) = root.children().find(|node| node.has_tag_name("channels")) {
        let mut channels = Vec::new();

        for channel_node in channels_node
            .children()
            .filter(|node| node.has_tag_name("channel"))
        {
            let value = channel_node
                .attribute("role")
                .or_else(|| channel_node.attribute("name"))
                .or_else(|| channel_node.attribute("id"))
                .or_else(|| channel_node.text())
                .unwrap_or("");

            let channel = Channel::parse(value).ok_or_else(|| RacfError::InvalidAttribute {
                element: "channel".to_string(),
                attribute: "role".to_string(),
                value: value.to_string(),
            })?;
            channels.push(channel);
        }

        return ChannelLayout::new(channels).ok_or(RacfError::InvalidChannelLayout);
    }

    let name = root
        .attribute("channelLayout")
        .or_else(|| root.attribute("layout"))
        .unwrap_or("stereo");

    ChannelLayout::preset(name).ok_or_else(|| RacfError::InvalidAttribute {
        element: "racf".to_string(),
        attribute: "channelLayout".to_string(),
        value: name.to_string(),
    })
}

fn validate_channel_attributes(root: Node<'_, '_>) -> Result<(), RacfError> {
    for node in root.descendants().filter(|node| node.is_element()) {
        if let Some(value) = node.attribute("channel") {
            if Channel::parse(value).is_none() {
                return Err(RacfError::InvalidAttribute {
                    element: node.tag_name().name().to_string(),
                    attribute: "channel".to_string(),
                    value: value.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn parse_imports(root: Node<'_, '_>) -> Vec<Import> {
    let Some(imports_node) = root.children().find(|node| node.has_tag_name("imports")) else {
        return Vec::new();
    };

    imports_node
        .children()
        .filter(|node| node.has_tag_name("import"))
        .map(|node| {
            let src = node.attribute("src").unwrap_or("").trim().to_string();
            let alias = node
                .attribute("alias")
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| src.clone());
            Import { src, alias }
        })
        .collect()
}

fn parse_definitions(root: Node<'_, '_>) -> HashMap<String, InstrumentDef> {
    let mut instruments = HashMap::new();

    let Some(definitions_node) = root
        .children()
        .find(|node| node.has_tag_name("definitions"))
    else {
        return instruments;
    };

    for node in definitions_node
        .children()
        .filter(|node| node.has_tag_name("instrument"))
    {
        if let Some(id) = node.attribute("id") {
            instruments.insert(id.trim().to_string(), parse_instrument(node));
        }
    }

    instruments
}

fn parse_instrument(node: Node<'_, '_>) -> InstrumentDef {
    let kind = node
        .attribute("type")
        .map(InstrumentKind::from_name)
        .unwrap_or_default();

    let mut waveform = Waveform::default();
    let mut distortion = 1.0;

    for child in node.children() {
        if child.has_tag_name("oscillator") {
            if let Some(name) = child.attribute("type") {
                waveform = Waveform::from_name(name);
            }
        } else if child.has_tag_name("effect") && child.attribute("type") == Some("distortion") {
            distortion = attr_f32(child, "drive").unwrap_or(1.0);
        }
    }

    let vocal = (kind == InstrumentKind::Vocal).then(|| {
        let vowel = node.attribute("vowel").unwrap_or("AH").to_string();
        let gender = node.attribute("gender").unwrap_or("male");
        let default_shift = if gender.eq_ignore_ascii_case("female") {
            1.2
        } else {
            0.88
        };

        VocalParams {
            vowel,
            formant_shift: attr_f32(node, "formantShift").unwrap_or(default_shift),
            breath: attr_f32(node, "breath").unwrap_or(0.25),
            nasal: attr_f32(node, "nasal").unwrap_or(0.5),
            grit: attr_f32(node, "grit").unwrap_or(0.2),
            vibrato_rate: attr_f32(node, "vibratoRate").unwrap_or(6.5),
            vibrato_depth: attr_f32(node, "vibratoDepth").unwrap_or(0.35),
        }
    });

    InstrumentDef {
        waveform,
        distortion,
        kind,
        vocal,
    }
}

fn parse_score(root: Node<'_, '_>) -> Vec<Track> {
    let Some(score_node) = root.children().find(|node| node.has_tag_name("score")) else {
        return Vec::new();
    };

    score_node
        .children()
        .filter(|node| node.has_tag_name("track"))
        .map(parse_track)
        .collect()
}

fn parse_track(node: Node<'_, '_>) -> Track {
    let defaults = TrackDefaults {
        pan: parse_pan(node.attribute("pan").unwrap_or("0.0"), 0.0),
        channel: node.attribute("channel").and_then(Channel::parse),
    };

    Track {
        instrument: node
            .attribute("instrument")
            .unwrap_or("")
            .trim()
            .to_string(),
        gain: attr_f32(node, "gain").unwrap_or(1.0),
        pan: defaults.pan,
        channel: defaults.channel,
        items: node
            .children()
            .filter_map(|child| parse_track_item(child, defaults))
            .collect(),
    }
}

#[derive(Clone, Copy)]
struct TrackDefaults {
    pan: f32,
    channel: Option<Channel>,
}

fn parse_track_item(node: Node<'_, '_>, defaults: TrackDefaults) -> Option<TrackItem> {
    if node.has_tag_name("tempo") {
        return node
            .attribute("bpm")
            .or_else(|| node.attribute("value"))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(TrackItem::Tempo);
    }

    if node.has_tag_name("force") || node.has_tag_name("gain") {
        return node
            .attribute("value")
            .or_else(|| node.attribute("val"))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(TrackItem::Gain);
    }

    if node.has_tag_name("fade-in")
        || (node.has_tag_name("fade") && node.attribute("mode") == Some("in"))
    {
        return Some(TrackItem::FadeIn {
            beats: parse_duration(node.attribute("duration").unwrap_or("1/1")),
        });
    }

    if node.has_tag_name("fade-out")
        || (node.has_tag_name("fade") && node.attribute("mode") == Some("out"))
    {
        return Some(TrackItem::FadeOut {
            beats: parse_duration(node.attribute("duration").unwrap_or("1/1")),
        });
    }

    if node.has_tag_name("separation") {
        let mode = node.attribute("mode").unwrap_or("start");
        return Some(TrackItem::Separation {
            active: matches!(mode, "start" | "on" | "true"),
            target: node.attribute("target").map(str::to_string),
        });
    }

    if node.has_tag_name("lyric") {
        return Some(TrackItem::Lyric(parse_lyric(node, defaults)));
    }

    if node.has_tag_name("note") {
        return Some(TrackItem::Note(parse_note(node, defaults)));
    }

    if node.has_tag_name("chord") {
        return Some(TrackItem::Chord(parse_chord(node, defaults)));
    }

    if node.has_tag_name("rest") {
        return Some(TrackItem::Rest(RestSpec {
            beats: parse_duration(node.attribute("duration").unwrap_or("1/4")),
            bpm: attr_f32(node, "bpm"),
        }));
    }

    if node.has_tag_name("sequence") {
        return Some(TrackItem::Sequence(parse_sequence(node, defaults)));
    }

    if node.has_tag_name("beat") {
        return Some(TrackItem::Beat(parse_beat(node, defaults)));
    }

    None
}

fn parse_note(node: Node<'_, '_>, defaults: TrackDefaults) -> NoteSpec {
    NoteSpec {
        pitch: node.attribute("pitch").unwrap_or("C4").to_string(),
        beats: parse_duration(node.attribute("duration").unwrap_or("1/4")),
        bpm: attr_f32(node, "bpm"),
        pan: node
            .attribute("pan")
            .map(|value| parse_pan(value, defaults.pan)),
        channel: node.attribute("channel").and_then(Channel::parse),
        velocity: attr_f32(node, "velocity").unwrap_or(1.0),
    }
}

fn parse_chord(node: Node<'_, '_>, defaults: TrackDefaults) -> ChordSpec {
    ChordSpec {
        pitches: node
            .attribute("pitch")
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|pitch| !pitch.is_empty())
            .map(str::to_string)
            .collect(),
        beats: parse_duration(node.attribute("duration").unwrap_or("1/4")),
        bpm: attr_f32(node, "bpm"),
        pan: node
            .attribute("pan")
            .map(|value| parse_pan(value, defaults.pan)),
        channel: node.attribute("channel").and_then(Channel::parse),
        velocity: attr_f32(node, "velocity").unwrap_or(1.0),
    }
}

fn parse_lyric(node: Node<'_, '_>, defaults: TrackDefaults) -> LyricSpec {
    LyricSpec {
        pitch: node.attribute("pitch").unwrap_or("E3").to_string(),
        vowel: node.attribute("vowel").unwrap_or("AH").to_string(),
        beats: parse_duration(node.attribute("duration").unwrap_or("1/4")),
        bpm: attr_f32(node, "bpm"),
        pan: node
            .attribute("pan")
            .map(|value| parse_pan(value, defaults.pan)),
        channel: node.attribute("channel").and_then(Channel::parse),
        velocity: attr_f32(node, "velocity").unwrap_or(1.0),
        vocal: VocalParams {
            vowel: node.attribute("vowel").unwrap_or("AH").to_string(),
            formant_shift: attr_f32(node, "formantShift").unwrap_or(0.9),
            breath: attr_f32(node, "breath").unwrap_or(0.3),
            nasal: attr_f32(node, "nasal").unwrap_or(0.5),
            grit: attr_f32(node, "grit").unwrap_or(0.2),
            vibrato_rate: attr_f32(node, "vibratoRate").unwrap_or(6.5),
            vibrato_depth: attr_f32(node, "vibratoDepth").unwrap_or(0.35),
        },
    }
}

fn parse_sequence(node: Node<'_, '_>, defaults: TrackDefaults) -> SequenceSpec {
    let base_pan = node
        .attribute("pan")
        .map(|value| parse_pan(value, defaults.pan));
    let channel = node.attribute("channel").and_then(Channel::parse);
    let bpm = attr_f32(node, "bpm");
    let velocity = attr_f32(node, "velocity").unwrap_or(1.0);

    let notes = node
        .attribute("pattern")
        .unwrap_or("")
        .split(',')
        .filter_map(|token| {
            let parts: Vec<&str> = token.trim().split(':').collect();
            if parts.len() < 2 {
                return None;
            }

            let pan = parts
                .get(2)
                .map(|value| parse_pan(value, base_pan.unwrap_or(defaults.pan)))
                .or(base_pan);

            Some(NoteSpec {
                pitch: parts[0].trim().to_string(),
                beats: parse_duration(parts[1].trim()),
                bpm,
                pan,
                channel,
                velocity,
            })
        })
        .collect();

    SequenceSpec { bpm, notes }
}

fn parse_beat(node: Node<'_, '_>, defaults: TrackDefaults) -> BeatSpec {
    let base_pan = node
        .attribute("pan")
        .map(|value| parse_pan(value, defaults.pan));
    let channel = node.attribute("channel").and_then(Channel::parse);
    let bpm = attr_f32(node, "bpm");

    let steps = node
        .attribute("pattern")
        .unwrap_or("")
        .split(',')
        .filter_map(|token| {
            let parts: Vec<&str> = token.trim().split(':').collect();
            if parts.is_empty() || parts[0].trim().is_empty() {
                return None;
            }

            let drums = parts[0]
                .split('/')
                .filter_map(DrumType::from_name)
                .collect();

            // A bare drum hit (no `:duration`) is a sixteenth note.
            let beats = parts
                .get(1)
                .map(|value| parse_duration(value.trim()))
                .unwrap_or(0.25);

            let pan = parts
                .get(2)
                .map(|value| parse_pan(value, base_pan.unwrap_or(defaults.pan)))
                .or(base_pan);

            Some(BeatStep { drums, beats, pan })
        })
        .collect();

    BeatSpec {
        bpm,
        channel,
        steps,
    }
}

fn attr_f32(node: Node<'_, '_>, name: &str) -> Option<f32> {
    node.attribute(name)
        .and_then(|value| value.trim().parse::<f32>().ok())
}

fn attr_u8(node: Node<'_, '_>, name: &str) -> Option<u8> {
    node.attribute(name)
        .and_then(|value| value.trim().parse::<u8>().ok())
}

fn parse_pan(value: &str, default: f32) -> f32 {
    match value.trim().to_ascii_uppercase().as_str() {
        "L" | "LEFT" => -1.0,
        "R" | "RIGHT" => 1.0,
        "C" | "CENTER" | "CENTRE" | "" => 0.0,
        other => other.parse::<f32>().unwrap_or(default).clamp(-1.0, 1.0),
    }
}

fn parse_bitrate(value: &str) -> u32 {
    let cleaned = value
        .trim()
        .to_ascii_lowercase()
        .replace("kbps", "")
        .replace('k', "");
    cleaned
        .trim()
        .parse::<u32>()
        .unwrap_or(DEFAULT_BITRATE_KBPS)
}

fn parse_time_signature(value: &str) -> TimeSignature {
    let mut parts = value.trim().split('/');
    let numerator = parts
        .next()
        .and_then(|part| part.trim().parse::<u8>().ok())
        .unwrap_or(4);
    let denominator = parts
        .next()
        .and_then(|part| part.trim().parse::<u8>().ok())
        .unwrap_or(4);

    TimeSignature {
        numerator,
        denominator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = r#"
        <racf version="1.0" tempo="120" channelLayout="stereo">
            <imports>
                <import src="import::piano" alias="Keys" />
            </imports>
            <definitions>
                <instrument id="Lead">
                    <oscillator type="square" />
                    <effect type="distortion" drive="4.5" />
                </instrument>
            </definitions>
            <score>
                <track instrument="Lead" gain="0.5" pan="-0.6" channel="left">
                    <note pitch="C4" duration="1/4" velocity="0.5" />
                    <rest duration="1/4" />
                    <chord pitch="C4,E4,G4" duration="1/2" />
                    <sequence pattern="D4:1/8, E4:1/8" />
                    <beat pattern="KICK/SNARE:1/4" />
                </track>
            </score>
        </racf>
    "#;

    #[test]
    fn parses_root_metadata_and_instruments() {
        let document = parse_document(SIMPLE).expect("valid document");

        assert_eq!(document.tempo, 120.0);
        assert_eq!(document.bit_depth, 16);
        assert_eq!(document.bitrate_kbps, 320);
        assert_eq!(
            document.time_signature,
            TimeSignature {
                numerator: 4,
                denominator: 4
            }
        );
        assert_eq!(document.channel_layout, ChannelLayout::stereo());
        assert_eq!(document.tracks.len(), 1);

        let lead = document.resolve_instrument("Lead");
        assert_eq!(lead.waveform, Waveform::Square);
        assert_eq!(lead.distortion, 4.5);

        let keys = document.resolve_instrument("Keys");
        assert_eq!(keys.kind, InstrumentKind::Piano);
    }

    #[test]
    fn parses_track_items_and_channels() {
        let document = parse_document(SIMPLE).expect("valid document");
        let track = &document.tracks[0];

        assert_eq!(track.channel, Some(Channel::FrontLeft));
        assert_eq!(track.items.len(), 5);
        assert!(matches!(track.items[0], TrackItem::Note(_)));
        assert!(matches!(track.items[1], TrackItem::Rest(_)));
        assert!(matches!(track.items[2], TrackItem::Chord(_)));
        assert!(matches!(track.items[3], TrackItem::Sequence(_)));
        assert!(matches!(track.items[4], TrackItem::Beat(_)));
    }

    #[test]
    fn custom_channel_layouts_are_read_in_order() {
        let xml = r#"
            <racf>
                <channels>
                    <channel role="front-left" />
                    <channel role="front-right" />
                    <channel role="low-frequency" />
                </channels>
            </racf>
        "#;

        let document = parse_document(xml).expect("valid document");
        assert_eq!(
            document.channel_layout.channels(),
            &[
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::LowFrequency
            ]
        );
    }

    #[test]
    fn unknown_channel_names_are_rejected() {
        let xml = r#"
            <racf>
                <score>
                    <track instrument="x" channel="font-left">
                        <note pitch="C4" duration="1/4" />
                    </track>
                </score>
            </racf>
        "#;

        let error = parse_document(xml).expect_err("typo must be rejected");
        assert!(matches!(
            error,
            RacfError::InvalidAttribute {
                attribute,
                value,
                ..
            } if attribute == "channel" && value == "font-left"
        ));
    }

    #[test]
    fn invalid_custom_channel_roles_are_rejected() {
        let xml = r#"
            <racf>
                <channels>
                    <channel role="font-left" />
                </channels>
            </racf>
        "#;

        assert!(matches!(
            parse_document(xml),
            Err(RacfError::InvalidAttribute { .. })
        ));
    }

    #[test]
    fn target_duration_scales_tempo() {
        let xml = r#"
            <racf tempo="120" targetDuration="00:10">
                <score>
                    <track instrument="x">
                        <note pitch="C4" duration="1/1" />
                    </track>
                </score>
            </racf>
        "#;

        let document = parse_document(xml).expect("valid document");
        assert_eq!(document.target_duration_secs, Some(10.0));
        assert_eq!(document.effective_tempo(), 24.0);
    }

    #[test]
    fn bare_beat_hits_are_sixteenth_notes() {
        let xml = r#"
            <racf tempo="120">
                <score>
                    <track instrument="Drums">
                        <beat pattern="CHINA, KICK:1/4" />
                    </track>
                </score>
            </racf>
        "#;

        let document = parse_document(xml).expect("valid document");
        let TrackItem::Beat(beat) = &document.tracks[0].items[0] else {
            panic!("expected a beat item");
        };

        assert_eq!(beat.steps[0].beats, 0.25);
        assert_eq!(beat.steps[1].beats, 1.0);
    }

    #[test]
    fn bitrate_accepts_k_and_kbps_suffixes() {
        assert_eq!(parse_bitrate("128k"), 128);
        assert_eq!(parse_bitrate("320kbps"), 320);
        assert_eq!(parse_bitrate("192"), 192);
        assert_eq!(parse_bitrate("nope"), DEFAULT_BITRATE_KBPS);
    }
}
