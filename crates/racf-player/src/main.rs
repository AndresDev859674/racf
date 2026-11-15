use std::env;
use std::fs;
use std::path::PathBuf;

use racf_core::{compile_timeline, parse_document, ScheduledAction};
use racf_player::{EqPreset, PlaybackOptions};

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
    let mut options = PlaybackOptions::default();
    let mut arguments = env::args().skip(1);

    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--dry-run" | "-n" => dry_run = true,
            "--no-mpris" => no_mpris = true,
            "--loop" | "-l" => options.looping = true,
            "--volume" => {
                let value = arguments
                    .next()
                    .ok_or("--volume needs a value between 0 and 200")?;
                options.volume_percent = parse_volume(&value)?;
            }
            "--eq" => {
                let value = arguments.next().ok_or("--eq needs a preset name")?;
                options.eq = Some(parse_eq(&value)?);
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            value if value.starts_with("--volume=") => {
                options.volume_percent = parse_volume(&value["--volume=".len()..])?;
            }
            value if value.starts_with("--eq=") => {
                options.eq = Some(parse_eq(&value["--eq=".len()..])?);
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

    for line in racf_player::report::render(&path.display().to_string(), &document, &timeline) {
        println!("[RACF] {line}");
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

    racf_player::play(&document, &timeline, &path, !no_mpris, &options)?;
    Ok(())
}

fn parse_volume(value: &str) -> Result<f32, String> {
    let parsed = value
        .trim()
        .trim_end_matches('%')
        .parse::<f32>()
        .map_err(|_| format!("invalid volume {value:?}; expected a number from 0 to 200"))?;

    if !parsed.is_finite() || !(0.0..=200.0).contains(&parsed) {
        return Err(format!(
            "invalid volume {value:?}; expected a number from 0 to 200"
        ));
    }

    Ok(parsed)
}

fn parse_eq(value: &str) -> Result<EqPreset, String> {
    EqPreset::parse(value).ok_or_else(|| {
        format!(
            "unknown EQ preset {value:?}; available: {}",
            EqPreset::names()
        )
    })
}

fn print_usage() {
    println!(
        "Usage: racf-player [options] <file.racf>\n\
         \n\
         Options:\n\
           -n, --dry-run     Parse and compile the file without opening audio\n\
               --no-mpris    Do not publish playback status over D-Bus\n\
               --volume <N>  Initial master volume, 0-200 (default 100)\n\
               --eq <name>   Initial EQ preset: flat, bass, vocal, rock, clarity\n\
           -l, --loop        Repeat the timeline until stopped\n\
           -h, --help        Show this help\n\
         \n\
         Interactive:\n\
           Press : during playback to open the command line. Type help there\n\
           for the full command list."
    );
}
