use std::env;
use std::fs;
use std::path::PathBuf;

use racf_core::{compile_timeline, parse_document, ScheduledAction};

fn main() {
    if let Err(error) = run() {
        eprintln!("racf-player: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    let mut no_mpris = false;
    let mut path: Option<PathBuf> = None;

    for argument in env::args().skip(1) {
        match argument.as_str() {
            "--dry-run" | "-n" => dry_run = true,
            "--no-mpris" => no_mpris = true,
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            value if value.starts_with('-') => {
                print_usage();
                return Err(format!("unknown option: {value}").into());
            }
            value if path.is_none() => path = Some(PathBuf::from(value)),
            value => {
                return Err(format!("unexpected extra argument: {value}").into());
            }
        }
    }

    let Some(path) = path else {
        print_usage();
        return Ok(());
    };

    let source = fs::read_to_string(&path)?;
    let document = parse_document(&source)?;
    let timeline = compile_timeline(&document);

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

    println!(
        "[RACF] {} | Tempo: {:.1} BPM | Bit depth: {}-bit | Bitrate: {} kbps | Layout: {}",
        path.display(),
        timeline.effective_tempo,
        document.bit_depth,
        document.bitrate_kbps,
        document.channel_layout,
    );
    println!(
        "[RACF] {} track(s) | {} audio clip(s) | {} timeline event(s) | {} voice(s) | ends at {:.2}s",
        document.tracks.len(),
        document.audio_clips.len(),
        timeline.events.len(),
        voices,
        timeline.end_ms as f32 / 1000.0,
    );

    if !document.metadata.is_empty() {
        let mut parts = Vec::new();
        if let Some(title) = &document.metadata.title {
            parts.push(format!("title={title:?}"));
        }
        if let Some(artist) = &document.metadata.artist {
            parts.push(format!("artist={artist:?}"));
        }
        if let Some(album) = &document.metadata.album {
            parts.push(format!("album={album:?}"));
        }
        println!("[RACF] Metadata: {}", parts.join(" | "));

        if let Some(description) = &document.metadata.description {
            println!("[RACF] Description: {description}");
        }
    }

    if audio_events > 0 && voices == 0 {
        println!(
            "[RACF] Embedded audio: {} clip(s), {:.2}s of sampled audio",
            document.audio_clips.len(),
            document
                .audio_clips
                .iter()
                .map(|clip| clip.duration_secs())
                .sum::<f32>(),
        );
    }

    if let Some(target) = document.target_duration_secs {
        println!(
            "[RACF] targetDuration: {:.1}s -> auto-scaled base tempo to {:.1} BPM",
            target, timeline.effective_tempo
        );
    }

    if dry_run {
        println!("[RACF] Dry run: input parsed and timeline compiled, audio not opened.");

        for event in timeline.events.iter().take(20) {
            match &event.action {
                ScheduledAction::Play(note) => println!(
                    "  {:>8.3}s  {}  {:>7.2} Hz  gain={:.2} pan={:+.2} channel={}",
                    event.timestamp_ms as f32 / 1000.0,
                    note.track_id,
                    note.frequency,
                    note.gain,
                    note.pan,
                    note.channel
                        .map(|channel| channel.to_string())
                        .unwrap_or_else(|| "auto".to_string()),
                ),
                ScheduledAction::PlayAudio(audio) => println!(
                    "  {:>8.3}s  [audio] {}  {:.2}s  gain={:.2} pan={:+.2} channel={}",
                    event.timestamp_ms as f32 / 1000.0,
                    audio.clip_id,
                    document
                        .audio_clips
                        .get(audio.clip_index)
                        .map(|clip| clip.duration_secs())
                        .unwrap_or(0.0),
                    audio.gain,
                    audio.pan,
                    audio
                        .channel
                        .map(|channel| channel.to_string())
                        .unwrap_or_else(|| "auto".to_string()),
                ),
                ScheduledAction::SetSeparation { active, target } => println!(
                    "  {:>8.3}s  separation={} target={:?}",
                    event.timestamp_ms as f32 / 1000.0,
                    active,
                    target
                ),
            }
        }

        if timeline.events.len() > 20 {
            println!("  ... {} more event(s)", timeline.events.len() - 20);
        }

        return Ok(());
    }

    racf_player::play(&document, &timeline, &path, !no_mpris)?;
    Ok(())
}

fn print_usage() {
    println!(
        "Usage: racf-player [--dry-run] [--no-mpris] <file.racf>\n\
         \n\
         Options:\n\
           -n, --dry-run   Parse and compile the file without opening audio\n\
               --no-mpris  Do not publish playback status over D-Bus\n\
           -h, --help      Show this help"
    );
}
