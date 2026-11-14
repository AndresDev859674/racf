use std::fmt;
use std::str::FromStr;

/// A named audio channel/speaker role.
///
/// RACF uses roles rather than raw indices so a document remains meaningful
/// when it is played on a device with a different channel count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    FrontLeft,
    FrontRight,
    FrontCenter,
    LowFrequency,
    RearLeft,
    RearRight,
    SideLeft,
    SideRight,
}

impl Channel {
    pub const ALL: [Channel; 8] = [
        Channel::FrontLeft,
        Channel::FrontRight,
        Channel::FrontCenter,
        Channel::LowFrequency,
        Channel::RearLeft,
        Channel::RearRight,
        Channel::SideLeft,
        Channel::SideRight,
    ];

    pub const fn canonical_name(self) -> &'static str {
        match self {
            Channel::FrontLeft => "front-left",
            Channel::FrontRight => "front-right",
            Channel::FrontCenter => "front-center",
            Channel::LowFrequency => "low-frequency",
            Channel::RearLeft => "rear-left",
            Channel::RearRight => "rear-right",
            Channel::SideLeft => "side-left",
            Channel::SideRight => "side-right",
        }
    }

    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Channel::FrontLeft => &["l", "left", "fl", "front left"],
            Channel::FrontRight => &["r", "right", "fr", "front right"],
            Channel::FrontCenter => &["c", "center", "centre", "fc", "front center", "mono"],
            Channel::LowFrequency => &["lfe", "sub", "subwoofer", "low frequency", "bass"],
            Channel::RearLeft => &["rl", "rear left", "back-left", "back left", "bl"],
            Channel::RearRight => &["rr", "rear right", "back-right", "back right", "br"],
            Channel::SideLeft => &["sl", "side left", "surround-left", "surround left"],
            Channel::SideRight => &["sr", "side right", "surround-right", "surround right"],
        }
    }

    /// Parses a channel role. Matching is case-insensitive and ignores `_` and
    /// `-` separators, so `front-left`, `front_left`, and `Front Left` are
    /// equivalent.
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = normalize(value);
        Self::ALL.into_iter().find(|channel| {
            normalize(channel.canonical_name()) == normalized
                || channel
                    .aliases()
                    .iter()
                    .any(|alias| normalize(alias) == normalized)
        })
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.canonical_name())
    }
}

impl FromStr for Channel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Channel::parse(value).ok_or(())
    }
}

/// An ordered collection of named output channels.
///
/// The order is the order the channels appear in the document (and therefore
/// the order in which the player writes to a device with the same channel
/// count).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelLayout {
    channels: Vec<Channel>,
}

impl Default for ChannelLayout {
    fn default() -> Self {
        Self::stereo()
    }
}

impl ChannelLayout {
    pub fn new(channels: Vec<Channel>) -> Option<Self> {
        if channels.is_empty() {
            return None;
        }

        let mut unique = Vec::with_capacity(channels.len());
        for channel in channels {
            if unique.contains(&channel) {
                return None;
            }
            unique.push(channel);
        }

        Some(Self { channels: unique })
    }

    pub fn mono() -> Self {
        Self {
            channels: vec![Channel::FrontCenter],
        }
    }

    pub fn stereo() -> Self {
        Self {
            channels: vec![Channel::FrontLeft, Channel::FrontRight],
        }
    }

    pub fn quad() -> Self {
        Self {
            channels: vec![
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::RearLeft,
                Channel::RearRight,
            ],
        }
    }

    /// Standard WAV/ITU-R ordering: FL, FR, FC, LFE, RL, RR.
    pub fn surround_5_1() -> Self {
        Self {
            channels: vec![
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::FrontCenter,
                Channel::LowFrequency,
                Channel::RearLeft,
                Channel::RearRight,
            ],
        }
    }

    /// Standard WAV/ITU-R ordering: FL, FR, FC, LFE, RL, RR, SL, SR.
    pub fn surround_7_1() -> Self {
        Self {
            channels: vec![
                Channel::FrontLeft,
                Channel::FrontRight,
                Channel::FrontCenter,
                Channel::LowFrequency,
                Channel::RearLeft,
                Channel::RearRight,
                Channel::SideLeft,
                Channel::SideRight,
            ],
        }
    }

    /// Looks up a preset layout by name.
    pub fn preset(name: &str) -> Option<Self> {
        match normalize(name).as_str() {
            "mono" | "1" | "1.0" | "single" => Some(Self::mono()),
            "stereo" | "2" | "2.0" => Some(Self::stereo()),
            "quad" | "quadraphonic" | "4" | "4.0" => Some(Self::quad()),
            "5.1" | "surround" | "surround-5.1" | "surround-51" | "6" | "6.0" => {
                Some(Self::surround_5_1())
            }
            "7.1" | "surround-7.1" | "surround-71" | "8" | "8.0" => Some(Self::surround_7_1()),
            _ => None,
        }
    }

    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn contains(&self, channel: Channel) -> bool {
        self.channels.contains(&channel)
    }

    pub fn index_of(&self, channel: Channel) -> Option<usize> {
        self.channels.iter().position(|current| *current == channel)
    }

    pub fn front_pair(&self) -> Option<(usize, usize)> {
        Some((
            self.index_of(Channel::FrontLeft)?,
            self.index_of(Channel::FrontRight)?,
        ))
    }

    pub fn center_index(&self) -> Option<usize> {
        self.index_of(Channel::FrontCenter)
    }
}

impl fmt::Display for ChannelLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self
            .channels
            .iter()
            .map(|channel| channel.canonical_name())
            .collect();
        f.write_str(&names.join(", "))
    }
}

impl FromStr for ChannelLayout {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ChannelLayout::preset(value).ok_or(())
    }
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-")
        .replace("--", "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_aliases_are_case_and_separator_insensitive() {
        assert_eq!(Channel::parse("LEFT"), Some(Channel::FrontLeft));
        assert_eq!(Channel::parse("front_left"), Some(Channel::FrontLeft));
        assert_eq!(Channel::parse("Front Left"), Some(Channel::FrontLeft));
        assert_eq!(Channel::parse("C"), Some(Channel::FrontCenter));
        assert_eq!(Channel::parse("LFE"), Some(Channel::LowFrequency));
    }

    #[test]
    fn preset_layouts_have_expected_channel_counts() {
        assert_eq!(ChannelLayout::preset("mono").unwrap().len(), 1);
        assert_eq!(ChannelLayout::preset("stereo").unwrap().len(), 2);
        assert_eq!(ChannelLayout::preset("5.1").unwrap().len(), 6);
        assert_eq!(ChannelLayout::preset("7.1").unwrap().len(), 8);
    }

    #[test]
    fn layouts_reject_empty_and_duplicate_channels() {
        assert!(ChannelLayout::new(vec![]).is_none());
        assert!(ChannelLayout::new(vec![Channel::FrontLeft, Channel::FrontLeft]).is_none());
    }

    #[test]
    fn front_pair_is_found_by_role_not_position() {
        let layout = ChannelLayout::new(vec![
            Channel::RearLeft,
            Channel::FrontRight,
            Channel::FrontLeft,
        ])
        .unwrap();
        assert_eq!(layout.front_pair(), Some((2, 1)));
    }
}
