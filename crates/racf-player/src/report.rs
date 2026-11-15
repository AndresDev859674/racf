//! Human-readable tree report for a parsed RACF document.
//!
//! The report is built as plain strings so it can be printed by the CLI and
//! exercised by tests without capturing terminal output.

use racf_core::{Document, ScheduledAction, Timeline};

/// One node in the report tree.
struct Node {
    label: String,
    children: Vec<Node>,
}

impl Node {
    fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            children: Vec::new(),
        }
    }

    fn branch(label: impl Into<String>, children: Vec<Node>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }
}

/// Builds the report, starting with the document path and followed by one
/// branch per document section.
pub fn render(path: &str, document: &Document, timeline: &Timeline) -> Vec<String> {
    let voices = timeline
        .events
        .iter()
        .filter(|event| matches!(event.action, ScheduledAction::Play(_)))
        .count();
    let audio_events = timeline
        .events
        .iter()
        .filter(|event| matches!(event.action, ScheduledAction::PlayAudio(_)))
        .count();

    let mut sections = vec![format_section(document, timeline)];
    if !document.metadata.is_empty() {
        sections.push(metadata_section(document));
    }
    if let Some(lyrics) = &document.lyrics {
        sections.push(Node::branch(
            "Lyrics",
            vec![
                Node::leaf(format!(
                    "Format: {} ({})",
                    lyrics.format.name(),
                    if lyrics.is_timed() { "timed" } else { "static" }
                )),
                Node::leaf(format!("Lines: {}", lyrics.line_count())),
            ],
        ));
    }
    if !document.audio_clips.is_empty() {
        sections.push(audio_section(document));
    }
    if !document.tracks.is_empty() {
        sections.push(tracks_section(document));
    }
    sections.push(Node::branch(
        "Timeline",
        vec![
            Node::leaf(format!(
                "Duration: {}",
                format_seconds(timeline.end_ms as f32 / 1000.0)
            )),
            Node::leaf(format!("Events: {}", timeline.events.len())),
            Node::leaf(format!("Voices: {voices}")),
            Node::leaf(format!("Embedded audio events: {audio_events}")),
        ],
    ));

    let mut lines = vec![path.to_string()];
    render_nodes(&sections, "", &mut lines);
    lines
}

fn format_section(document: &Document, timeline: &Timeline) -> Node {
    let mut children = Vec::new();
    if let Some(version) = &document.version {
        children.push(Node::leaf(format!("Version: {version}")));
    }
    children.push(Node::leaf(format!(
        "Tempo: {:.1} BPM",
        timeline.effective_tempo
    )));
    children.push(Node::leaf(format!(
        "Time signature: {}/{}",
        document.time_signature.numerator, document.time_signature.denominator
    )));
    children.push(Node::leaf(format!("Bit depth: {}-bit", document.bit_depth)));
    children.push(Node::leaf(format!(
        "Bitrate: {} kbps",
        document.bitrate_kbps
    )));
    children.push(Node::leaf(format!("Layout: {}", document.channel_layout)));
    if let Some(rate) = document.target_sample_rate {
        children.push(Node::leaf(format!(
            "Target sample rate: {}",
            format_hz(rate)
        )));
    }
    if let Some(target) = document.target_duration_secs {
        children.push(Node::leaf(format!(
            "Target duration: {} (tempo auto-scaled)",
            format_seconds(target)
        )));
    }

    Node::branch("Format", children)
}

fn metadata_section(document: &Document) -> Node {
    let mut children = Vec::new();
    if let Some(title) = &document.metadata.title {
        children.push(Node::leaf(format!("Title: {}", one_line(title))));
    }
    if let Some(artist) = &document.metadata.artist {
        children.push(Node::leaf(format!("Artist: {}", one_line(artist))));
    }
    if let Some(album) = &document.metadata.album {
        children.push(Node::leaf(format!("Album: {}", one_line(album))));
    }
    if let Some(description) = &document.metadata.description {
        children.push(Node::leaf(format!(
            "Description: {}",
            one_line(description)
        )));
    }

    Node::branch("Metadata", children)
}

fn audio_section(document: &Document) -> Node {
    let clips = document
        .audio_clips
        .iter()
        .enumerate()
        .map(|(index, clip)| {
            let name = if clip.id.trim().is_empty() {
                format!("clip {}", index + 1)
            } else {
                format!("clip {:?}", clip.id)
            };
            let channels = match clip.channels {
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                other => format!("{other} channels"),
            };

            Node::branch(
                name,
                vec![
                    Node::leaf(format!(
                        "Start: {}",
                        format_seconds(clip.at_ms as f32 / 1000.0)
                    )),
                    Node::leaf(format!(
                        "Duration: {}",
                        format_seconds(clip.duration_secs())
                    )),
                    Node::leaf(format!("Sample rate: {} Hz", clip.sample_rate)),
                    Node::leaf(format!("Channels: {channels}")),
                    Node::leaf(format!("Encoding: {}", clip.encoding.name())),
                    Node::leaf(format!("Size: {}", format_bytes(clip.data.len() as u64))),
                ],
            )
        })
        .collect();

    Node::branch(
        format!(
            "Audio ({} clip{})",
            document.audio_clips.len(),
            if document.audio_clips.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
        clips,
    )
}

fn tracks_section(document: &Document) -> Node {
    let tracks = document
        .tracks
        .iter()
        .map(|track| {
            let channel = track
                .channel
                .map(|channel| channel.to_string())
                .unwrap_or_else(|| "auto".to_string());

            Node::branch(
                track.instrument.clone(),
                vec![
                    Node::leaf(format!("Gain: {:.2}", track.gain)),
                    Node::leaf(format!("Pan: {:+.2}", track.pan)),
                    Node::leaf(format!("Channel: {channel}")),
                ],
            )
        })
        .collect();

    Node::branch(format!("Tracks ({})", document.tracks.len()), tracks)
}

fn render_nodes(nodes: &[Node], prefix: &str, lines: &mut Vec<String>) {
    for (index, node) in nodes.iter().enumerate() {
        let last = index + 1 == nodes.len();
        let connector = if last { "└─ " } else { "├─ " };
        lines.push(format!("{prefix}{connector}{}", node.label));

        if !node.children.is_empty() {
            let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
            render_nodes(&node.children, &child_prefix, lines);
        }
    }
}

fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_seconds(seconds: f32) -> String {
    format!("{:.2}s", seconds.max(0.0))
}

fn format_hz(rate: f32) -> String {
    if rate >= 1000.0 {
        format!("{:.1} kHz", rate / 1000.0)
    } else {
        format!("{rate:.0} Hz")
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use racf_core::{compile_timeline, parse_document};

    #[test]
    fn renders_tree_sections_for_metadata_lyrics_and_audio() {
        let xml = r#"
            <racf version="1.0" channelLayout="stereo" tempo="90">
                <metadata>
                    <title>Night Drive</title>
                    <description>First line
second line</description>
                </metadata>
                <lyrics format="lrc">[00:01.00]Hello</lyrics>
                <audio>
                    <clip id="main" at="0ms" sampleRate="8000" channels="1" encoding="mulaw">
                        <data>AAAA</data>
                    </clip>
                </audio>
            </racf>
        "#;
        let document = parse_document(xml).unwrap();
        let timeline = compile_timeline(&document);
        let lines = render("song.racf", &document, &timeline).join("\n");

        assert!(lines.starts_with("song.racf\n├─ Format"));
        assert!(lines.contains("├─ Metadata"));
        assert!(lines.contains("│  └─ Description: First line / second line"));
        assert!(lines.contains("├─ Lyrics"));
        assert!(lines.contains("│  └─ Lines: 1"));
        assert!(lines.contains("├─ Audio (1 clip)"));
        assert!(lines.contains("│  └─ clip \"main\""));
        assert!(lines.contains("└─ Timeline"));
    }

    #[test]
    fn omits_sections_that_do_not_exist() {
        let document = parse_document(r#"<racf></racf>"#).unwrap();
        let timeline = compile_timeline(&document);
        let lines = render("empty.racf", &document, &timeline).join("\n");

        assert!(!lines.contains("Metadata"));
        assert!(!lines.contains("Lyrics"));
        assert!(!lines.contains("Audio"));
        assert!(!lines.contains("Tracks"));
        assert!(lines.contains("Timeline"));
    }
}
