use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use racf_core::{
    compile_timeline, parse_document, Document, Metadata, SampleEncoding, ScheduledAction,
};
use racf_utils::{convert_file, write_document, ConvertOptions};

#[derive(Parser)]
#[command(
    name = "racf-utils",
    version,
    about = "Convert, inspect, and validate RACF documents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert an MP3 (or WAV) into a RACF document with embedded audio
    Convert(ConvertArgs),
    /// Show metadata, tracks, and embedded audio details
    Inspect {
        /// RACF document to inspect
        file: PathBuf,
    },
    /// Parse documents and report any errors
    Validate {
        /// RACF documents to validate
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
}

#[derive(Args)]
struct ConvertArgs {
    /// Input audio file (MP3 or WAV)
    input: PathBuf,

    /// Output path (defaults to the input name with a .racf extension)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Target sample rate in Hz
    #[arg(long, default_value_t = 22_050)]
    sample_rate: u32,

    /// Target channel count (1 = mono, 2 = stereo)
    #[arg(long, default_value_t = 1)]
    channels: u8,

    /// Sample encoding used inside the RACF file
    #[arg(long, value_enum, default_value_t = EncodingArg::Mulaw)]
    encoding: EncodingArg,

    /// Document title (falls back to the file's tag, then its filename)
    #[arg(long)]
    title: Option<String>,

    /// Document description
    #[arg(long)]
    description: Option<String>,

    /// Artist name
    #[arg(long)]
    artist: Option<String>,

    /// Album name
    #[arg(long)]
    album: Option<String>,

    /// Refuse to write files larger than this many MiB (0 disables the limit)
    #[arg(long, default_value_t = 64)]
    max_size_mb: u64,

    /// Write even when the result exceeds --max-size-mb
    #[arg(long)]
    allow_large: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EncodingArg {
    /// G.711 µ-law, 1 byte per sample (recommended)
    Mulaw,
    /// Unsigned 8-bit PCM, 1 byte per sample
    Pcm8,
    /// Signed 16-bit little-endian PCM, 2 bytes per sample
    Pcm16,
}

impl From<EncodingArg> for SampleEncoding {
    fn from(value: EncodingArg) -> Self {
        match value {
            EncodingArg::Mulaw => SampleEncoding::Mulaw,
            EncodingArg::Pcm8 => SampleEncoding::PcmU8,
            EncodingArg::Pcm16 => SampleEncoding::PcmI16Le,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("racf-utils: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Convert(args) => convert(args),
        Command::Inspect { file } => inspect(&file),
        Command::Validate { files } => validate(&files),
    }
}

fn convert(args: ConvertArgs) -> Result<()> {
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.input.with_extension("racf"));

    let options = ConvertOptions {
        sample_rate: args.sample_rate,
        channels: args.channels,
        encoding: args.encoding.into(),
        metadata: Metadata {
            title: args.title.clone(),
            description: args.description.clone(),
            artist: args.artist.clone(),
            album: args.album.clone(),
        },
    };

    let conversion = convert_file(&args.input, &options)?;

    if args.max_size_mb > 0 && !args.allow_large {
        let limit = args.max_size_mb.saturating_mul(1024 * 1024);
        if conversion.xml.len() as u64 > limit {
            bail!(
                "output would be {} (limit is {}). Lower --sample-rate, use --channels 1, \
                 choose --encoding mulaw, raise --max-size-mb, or pass --allow-large",
                format_bytes(conversion.xml.len() as u64),
                format_bytes(limit)
            );
        }
    }

    write_document(&output, &conversion.xml)?;

    let source_kind = if conversion.source_channels == 1 {
        "mono".to_string()
    } else {
        format!("{} channels", conversion.source_channels)
    };
    let source_bytes = std::fs::metadata(&args.input)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    println!("Converted {} -> {}", args.input.display(), output.display());
    println!(
        "  source: {} | {} Hz | {} | {}",
        format_duration(conversion.source_duration_secs),
        conversion.source_sample_rate,
        source_kind,
        format_bytes(source_bytes),
    );
    println!(
        "  output: {} | {} Hz | {} channel(s) | {}",
        format_duration(conversion.output_duration_secs),
        args.sample_rate,
        args.channels,
        args.encoding_label(),
    );
    println!(
        "  embedded audio: {} | RACF file: {}",
        format_bytes(conversion.data_bytes as u64),
        format_bytes(conversion.xml.len() as u64),
    );

    if let Some(title) = &conversion.metadata.title {
        println!("  title: {title}");
    }
    if let Some(artist) = &conversion.metadata.artist {
        println!("  artist: {artist}");
    }
    if let Some(album) = &conversion.metadata.album {
        println!("  album: {album}");
    }
    if let Some(description) = &conversion.metadata.description {
        println!("  description: {description}");
    }

    Ok(())
}

fn inspect(path: &Path) -> Result<()> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let document = parse_document(&source)?;
    let timeline = compile_timeline(&document);

    println!("{}", path.display());
    print_metadata(&document);

    println!(
        "  {} track(s), {} embedded clip(s), {} event(s), {}",
        document.tracks.len(),
        document.audio_clips.len(),
        timeline.events.len(),
        format_duration(timeline.end_ms as f32 / 1000.0),
    );
    println!(
        "  tempo {:.1} BPM | {} | bit depth {} | bitrate {} kbps",
        timeline.effective_tempo,
        document.channel_layout,
        document.bit_depth,
        document.bitrate_kbps,
    );

    for clip in &document.audio_clips {
        println!(
            "  audio {:<12} at {:<8} | {:>6} Hz | {} ch | {:<5} | {:>10} | {}",
            clip.id,
            format_duration(clip.at_ms as f32 / 1000.0),
            clip.sample_rate,
            clip.channels,
            clip.encoding.name(),
            format_bytes(clip.data.len() as u64),
            format_duration(clip.duration_secs()),
        );
    }

    for track in &document.tracks {
        let channel = track
            .channel
            .map(|channel| channel.to_string())
            .unwrap_or_else(|| "auto".to_string());
        println!(
            "  track {:<16} | gain {:.2} | pan {:+.2} | channel {}",
            track.instrument, track.gain, track.pan, channel
        );
    }

    let audio_events = timeline
        .events
        .iter()
        .filter(|event| matches!(event.action, ScheduledAction::PlayAudio(_)))
        .count();
    if audio_events > 0 {
        println!("  {audio_events} embedded audio event(s)");
    }

    Ok(())
}

fn validate(files: &[PathBuf]) -> Result<()> {
    let mut failures = 0;

    for path in files {
        match std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|source| parse_document(&source).map_err(anyhow::Error::from))
        {
            Ok(document) => {
                println!(
                    "OK   {} ({} track(s), {} clip(s))",
                    path.display(),
                    document.tracks.len(),
                    document.audio_clips.len()
                );
            }
            Err(error) => {
                failures += 1;
                println!("FAIL {}: {error}", path.display());
            }
        }
    }

    if failures > 0 {
        bail!("{failures} document(s) failed validation");
    }

    Ok(())
}

fn print_metadata(document: &Document) {
    if let Some(title) = &document.metadata.title {
        println!("  title: {title}");
    }
    if let Some(description) = &document.metadata.description {
        println!("  description: {description}");
    }
    if let Some(artist) = &document.metadata.artist {
        println!("  artist: {artist}");
    }
    if let Some(album) = &document.metadata.album {
        println!("  album: {album}");
    }
}

fn format_duration(seconds: f32) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let milliseconds = total_ms % 1000;
    let total_seconds = total_ms / 1000;
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = total_seconds / 3600;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}.{milliseconds:03}")
    } else {
        format!("{minutes}:{seconds:02}.{milliseconds:03}")
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

impl ConvertArgs {
    fn encoding_label(&self) -> &'static str {
        match self.encoding {
            EncodingArg::Mulaw => "mulaw",
            EncodingArg::Pcm8 => "pcm8",
            EncodingArg::Pcm16 => "pcm16",
        }
    }
}
