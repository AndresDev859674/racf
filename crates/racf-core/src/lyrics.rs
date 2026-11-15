use std::fmt;
use std::path::Path;

use roxmltree::Node;

use crate::error::RacfError;

/// How the text inside a `<lyrics>` element is structured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricsFormat {
    /// Plain RACF lyrics: one line per entry. Timestamps are also accepted.
    Native,
    /// Standard LRC, with `[mm:ss.xx]` timestamps and `[key:value]` tags.
    Lrc,
    /// SubRip (`.srt`) cues with `HH:MM:SS,mmm --> HH:MM:SS,mmm` timing.
    Srt,
}

impl LyricsFormat {
    pub const fn name(self) -> &'static str {
        match self {
            LyricsFormat::Native => "native",
            LyricsFormat::Lrc => "lrc",
            LyricsFormat::Srt => "srt",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "native" | "racf" | "text" | "plain" | "txt" => Some(LyricsFormat::Native),
            "lrc" | "lyric" | "lyrics" => Some(LyricsFormat::Lrc),
            "srt" | "subrip" | "subtitle" | "subtitles" => Some(LyricsFormat::Srt),
            _ => None,
        }
    }

    /// Guesses the format from a file extension such as `.lrc` or `.srt`.
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        Self::parse(extension)
    }
}

impl fmt::Display for LyricsFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One displayed lyric line. Lines without a timestamp are static text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricLine {
    pub timestamp_ms: Option<u64>,
    pub text: String,
}

/// A metadata pair found in an LRC header, such as `[ar:Artist]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsTag {
    pub key: String,
    pub value: String,
}

/// Lyrics attached to a document.
///
/// `raw` keeps the original text so the player and converters can round-trip
/// the author's formatting, while `lines` holds the parsed representation used
/// for synchronized display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lyrics {
    pub format: LyricsFormat,
    pub raw: String,
    pub lines: Vec<LyricLine>,
    pub tags: Vec<LyricsTag>,
}

impl Lyrics {
    /// Parses lyrics in the given format. Parsing is deliberately forgiving:
    /// unrecognized lines are kept as untimed text instead of being rejected.
    pub fn parse(format: LyricsFormat, text: impl Into<String>) -> Self {
        let raw = text.into();
        let (lines, tags) = match format {
            LyricsFormat::Native => parse_timed_lines(&raw, false),
            LyricsFormat::Lrc => parse_timed_lines(&raw, true),
            LyricsFormat::Srt => parse_srt(&raw),
        };

        Self {
            format,
            raw,
            lines,
            tags,
        }
    }

    /// True when at least one line has a timestamp and can follow playback.
    pub fn is_timed(&self) -> bool {
        self.lines.iter().any(|line| line.timestamp_ms.is_some())
    }

    /// Number of non-empty lyric lines.
    pub fn line_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|line| !line.text.trim().is_empty())
            .count()
    }

    /// Returns the metadata tag with `key`, matching case-insensitively.
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|tag| tag.key.eq_ignore_ascii_case(key))
            .map(|tag| tag.value.as_str())
    }

    /// The index of the line that should be highlighted at `position_ms`.
    ///
    /// Only timed lines participate. If several lines share the same timestamp,
    /// the last one wins.
    pub fn active_index(&self, position_ms: u64) -> Option<usize> {
        let mut active = None;

        for (index, line) in self.lines.iter().enumerate() {
            let Some(timestamp) = line.timestamp_ms else {
                continue;
            };
            if timestamp <= position_ms {
                active = Some(index);
            }
        }

        active
    }

    /// The line to highlight at `position_ms`, if lyrics are timed.
    pub fn active_line(&self, position_ms: u64) -> Option<&LyricLine> {
        self.active_index(position_ms)
            .and_then(|index| self.lines.get(index))
    }
}

fn parse_timed_lines(text: &str, collect_tags: bool) -> (Vec<LyricLine>, Vec<LyricsTag>) {
    let mut lines = Vec::new();
    let mut tags = Vec::new();
    let mut offset_ms = 0i64;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // Standard LRC header tags: [ar:Artist], [offset:+250], ...
        if collect_tags {
            if let Some(tag) = parse_header_tag(line) {
                if tag.key.eq_ignore_ascii_case("offset") {
                    offset_ms = tag.value.trim().parse::<i64>().unwrap_or(0);
                }
                tags.push(tag);
                continue;
            }
        }

        let mut timestamps = Vec::new();
        let mut rest = line;

        while let Some((timestamp, remainder)) = take_leading_timestamp(rest) {
            timestamps.push(timestamp);
            rest = remainder;
        }

        if timestamps.is_empty() {
            lines.push(LyricLine {
                timestamp_ms: None,
                text: line.to_string(),
            });
            continue;
        }

        for timestamp in timestamps {
            lines.push(LyricLine {
                timestamp_ms: Some(timestamp),
                text: rest.trim().to_string(),
            });
        }
    }

    if offset_ms != 0 {
        for line in &mut lines {
            if let Some(timestamp) = line.timestamp_ms {
                line.timestamp_ms = Some(apply_offset(timestamp, offset_ms));
            }
        }
    }

    (lines, tags)
}

fn parse_header_tag(line: &str) -> Option<LyricsTag> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (key, value) = inner.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || !key.chars().all(|character| character.is_ascii_alphabetic()) {
        return None;
    }

    Some(LyricsTag {
        key: key.to_string(),
        value: value.trim().to_string(),
    })
}

/// Consumes one leading `[mm:ss.xx]`, `[mm:ss]`, or `[hh:mm:ss.xx]` timestamp.
fn take_leading_timestamp(line: &str) -> Option<(u64, &str)> {
    let rest = line.strip_prefix('[')?;
    let (inside, remainder) = rest.split_once(']')?;
    let timestamp = parse_clock_timestamp(inside)?;
    Some((timestamp, remainder))
}

fn parse_clock_timestamp(value: &str) -> Option<u64> {
    let parts: Vec<&str> = value.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit() || c == '.'))
    {
        return None;
    }

    let mut total = 0.0f64;
    for part in &parts {
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }

    if !total.is_finite() || total < 0.0 {
        return None;
    }
    Some((total * 1000.0).round() as u64)
}

fn apply_offset(timestamp_ms: u64, offset_ms: i64) -> u64 {
    if offset_ms >= 0 {
        timestamp_ms.saturating_add(offset_ms as u64)
    } else {
        timestamp_ms.saturating_sub(offset_ms.unsigned_abs())
    }
}

fn parse_srt(text: &str) -> (Vec<LyricLine>, Vec<LyricsTag>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();

    for block in normalized.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }

        let mut text_lines = Vec::new();
        let mut start_ms = None;

        for line in block.lines() {
            if let Some((start, _end)) = parse_srt_timing(line) {
                start_ms = Some(start);
                continue;
            }
            if start_ms.is_none() && line.trim().parse::<u32>().is_ok() {
                continue; // Cue number.
            }
            if start_ms.is_some() {
                text_lines.push(line.trim().to_string());
            }
        }

        let Some(timestamp_ms) = start_ms else {
            continue;
        };
        let text = text_lines.join("\n");
        if text.trim().is_empty() {
            continue;
        }

        lines.push(LyricLine {
            timestamp_ms: Some(timestamp_ms),
            text,
        });
    }

    (lines, Vec::new())
}

fn parse_srt_timing(line: &str) -> Option<(u64, u64)> {
    let (start, end) = line.split_once("-->")?;
    Some((
        parse_srt_timestamp(start.trim())?,
        parse_srt_timestamp(end.trim())?,
    ))
}

fn parse_srt_timestamp(value: &str) -> Option<u64> {
    let value = value.replace(',', ".");
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let hours = parts[0].parse::<f64>().ok()?;
    let minutes = parts[1].parse::<f64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;
    let total = hours * 3600.0 + minutes * 60.0 + seconds;

    if !total.is_finite() || total < 0.0 {
        return None;
    }
    Some((total * 1000.0).round() as u64)
}

/// Reads the optional `<lyrics>` element.
///
/// The element may be a direct child of `<racf>` or live inside `<metadata>`.
pub(crate) fn parse_lyrics(root: Node<'_, '_>) -> Result<Option<Lyrics>, RacfError> {
    let direct = root.children().find(|node| node.has_tag_name("lyrics"));
    let nested = root
        .children()
        .find(|node| node.has_tag_name("metadata"))
        .and_then(|metadata| metadata.children().find(|node| node.has_tag_name("lyrics")));
    let Some(node) = direct.or(nested) else {
        return Ok(None);
    };

    let format_name = node
        .attribute("format")
        .or_else(|| node.attribute("type"))
        .unwrap_or(LyricsFormat::Native.name());
    let format = LyricsFormat::parse(format_name).ok_or_else(|| RacfError::InvalidLyrics {
        message: format!("unknown lyrics format {format_name:?}; use native, lrc, or srt"),
    })?;

    let mut text = String::new();
    for child in node.descendants().filter(|child| child.is_text()) {
        if let Some(value) = child.text() {
            text.push_str(value);
        }
    }
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    Ok(Some(Lyrics::parse(format, text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse_document;

    #[test]
    fn parses_lrc_timestamps_and_tags() {
        let lyrics = Lyrics::parse(
            LyricsFormat::Lrc,
            "[ar:Andres]\n[ti:Night Drive]\n[offset:+250]\n\
             [00:01.50]First line\n[00:05.00][00:09.25]Repeated\n",
        );

        assert_eq!(lyrics.tag("ar"), Some("Andres"));
        assert_eq!(lyrics.tag("TI"), Some("Night Drive"));
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].timestamp_ms, Some(1_750));
        assert_eq!(lyrics.lines[1].timestamp_ms, Some(5_250));
        assert_eq!(lyrics.lines[2].timestamp_ms, Some(9_500));
        assert_eq!(lyrics.lines[2].text, "Repeated");
        assert!(lyrics.is_timed());
    }

    #[test]
    fn lrc_offset_applies_to_every_timestamp() {
        let lyrics = Lyrics::parse(
            LyricsFormat::Lrc,
            "[00:01.00]One\n[offset:-500]\n[00:02.00]Two\n",
        );

        assert_eq!(lyrics.lines[0].timestamp_ms, Some(500));
        assert_eq!(lyrics.lines[1].timestamp_ms, Some(1_500));
    }

    #[test]
    fn active_line_tracks_playback_position() {
        let lyrics = Lyrics::parse(
            LyricsFormat::Lrc,
            "[00:00.00]Zero\n[00:02.00]Two\n[00:04.00]Four\n",
        );

        assert_eq!(
            lyrics.active_line(1_000).map(|line| line.text.as_str()),
            Some("Zero")
        );
        assert_eq!(
            lyrics.active_line(2_000).map(|line| line.text.as_str()),
            Some("Two")
        );
        assert_eq!(
            lyrics.active_line(3_999).map(|line| line.text.as_str()),
            Some("Two")
        );
        assert_eq!(
            lyrics.active_line(10_000).map(|line| line.text.as_str()),
            Some("Four")
        );
    }

    #[test]
    fn native_lyrics_without_timestamps_are_static() {
        let lyrics = Lyrics::parse(LyricsFormat::Native, "First line\n\nSecond line\n");

        assert!(!lyrics.is_timed());
        assert_eq!(lyrics.line_count(), 2);
        assert_eq!(lyrics.active_index(1_000), None);
    }

    #[test]
    fn native_lyrics_keep_section_markers() {
        let lyrics = Lyrics::parse(LyricsFormat::Native, "[Verse:1]\nFirst line\n");

        assert!(lyrics.tags.is_empty());
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "[Verse:1]");
    }

    #[test]
    fn parses_srt_cues() {
        let lyrics = Lyrics::parse(
            LyricsFormat::Srt,
            "1\n00:00:01,000 --> 00:00:03,000\nHello\n\n\
             2\n00:00:04,500 --> 00:00:06,000\nWorld\nsecond line\n",
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].timestamp_ms, Some(1_000));
        assert_eq!(lyrics.lines[0].text, "Hello");
        assert_eq!(lyrics.lines[1].timestamp_ms, Some(4_500));
        assert_eq!(lyrics.lines[1].text, "World\nsecond line");
    }

    #[test]
    fn document_reads_top_level_and_nested_lyrics() {
        let document = parse_document(
            r#"
            <racf>
                <lyrics format="lrc">[00:01.00]Top level</lyrics>
            </racf>
            "#,
        )
        .unwrap();
        let lyrics = document.lyrics.as_ref().unwrap();
        assert_eq!(lyrics.format, LyricsFormat::Lrc);
        assert_eq!(lyrics.lines[0].text, "Top level");

        let document = parse_document(
            r#"
            <racf>
                <metadata>
                    <lyrics>[00:02.00]Nested</lyrics>
                </metadata>
            </racf>
            "#,
        )
        .unwrap();
        let lyrics = document.lyrics.as_ref().unwrap();
        assert_eq!(lyrics.format, LyricsFormat::Native);
        assert_eq!(lyrics.lines[0].timestamp_ms, Some(2_000));
    }

    #[test]
    fn rejects_unknown_formats() {
        let error =
            parse_document(r#"<racf><lyrics format="karaoke">words</lyrics></racf>"#).unwrap_err();
        assert!(matches!(error, RacfError::InvalidLyrics { .. }));
    }
}
