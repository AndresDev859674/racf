//! Parsing and formatting for the interactive `:` command line.

use crate::eq::{self, EqPreset};

/// A command entered at the `:` prompt.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Show the current volume or change it.
    Volume(Option<VolumeChange>),
    /// Seek to an absolute time or by a relative offset.
    Seek(SeekRequest),
    Pause,
    Resume,
    TogglePause,
    /// Show, disable, or modify the equalizer.
    Eq(EqRequest),
    /// Show the loop state or change it.
    Loop(Option<LoopAction>),
    Status,
    Help,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolumeChange {
    /// Set the master volume in percent (0–200).
    Set(f32),
    /// Add or subtract percentage points.
    Delta(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekRequest {
    Absolute(u64),
    Relative(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopAction {
    On,
    Off,
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EqRequest {
    Show,
    Off,
    Preset(EqPreset),
    Band { index: usize, gain_db: i32 },
}

/// Parses one command line. The leading `:` is optional, and `volume=90` is
/// accepted as an alias for `volume 90`.
pub fn parse(input: &str) -> Result<Command, String> {
    let trimmed = input.trim().trim_start_matches(':').trim();
    if trimmed.is_empty() {
        return Err("empty command; type help for a list of commands".to_string());
    }

    let tokens: Vec<&str> = trimmed
        .split_whitespace()
        .flat_map(|token| token.split('='))
        .filter(|token| !token.is_empty())
        .collect();

    let Some((name, args)) = tokens.split_first() else {
        return Err("empty command; type help for a list of commands".to_string());
    };
    let name = name.to_ascii_lowercase();

    match name.as_str() {
        "volume" | "vol" | "v" => parse_volume(args),
        "playback" | "seek" | "position" | "pos" | "goto" => parse_seek(args),
        "pause" => no_args(args, Command::Pause),
        "play" | "resume" | "unpause" => no_args(args, Command::Resume),
        "toggle" | "pp" => no_args(args, Command::TogglePause),
        "eq" | "equalizer" | "equaliser" => parse_eq(args),
        "loop" | "repeat" => parse_loop(args),
        "status" | "info" | "stat" => no_args(args, Command::Status),
        "help" | "?" | "h" => no_args(args, Command::Help),
        "quit" | "exit" | "q" => no_args(args, Command::Quit),
        other => Err(format!("unknown command: {other} (type help)")),
    }
}

fn no_args(args: &[&str], command: Command) -> Result<Command, String> {
    if args.is_empty() {
        Ok(command)
    } else {
        Err(format!("unexpected argument: {}", args[0]))
    }
}

fn parse_volume(args: &[&str]) -> Result<Command, String> {
    let Some(token) = args.first() else {
        return Ok(Command::Volume(None));
    };
    if args.len() > 1 {
        return Err("usage: volume [0-200|+N|-N]".to_string());
    }

    let value = token.trim().trim_end_matches('%');
    let parsed = value
        .parse::<f32>()
        .map_err(|_| format!("invalid volume: {token:?} (expected 0-200)"))?;
    if !parsed.is_finite() {
        return Err(format!("invalid volume: {token:?}"));
    }

    if value.starts_with('+') || value.starts_with('-') {
        Ok(Command::Volume(Some(VolumeChange::Delta(parsed))))
    } else {
        Ok(Command::Volume(Some(VolumeChange::Set(parsed))))
    }
}

fn parse_seek(args: &[&str]) -> Result<Command, String> {
    let Some(token) = args.first() else {
        return Err("usage: playback <time>, e.g. playback 1:00".to_string());
    };
    if args.len() > 1 {
        return Err("usage: playback <time>, e.g. playback 1:00".to_string());
    }

    if let Some(value) = token.strip_prefix('+') {
        return Ok(Command::Seek(SeekRequest::Relative(
            parse_time_ms(value)? as i64
        )));
    }

    if let Some(value) = token.strip_prefix('-') {
        return Ok(Command::Seek(SeekRequest::Relative(
            -(parse_time_ms(value)? as i64),
        )));
    }

    Ok(Command::Seek(SeekRequest::Absolute(parse_time_ms(token)?)))
}

fn parse_eq(args: &[&str]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Eq(EqRequest::Show));
    };

    let lower = first.to_ascii_lowercase();
    match lower.as_str() {
        "off" | "none" | "disable" => {
            if args.len() == 1 {
                return Ok(Command::Eq(EqRequest::Off));
            }
            return Err("usage: eq off".to_string());
        }
        "on" | "flat" | "neutral" => {
            if args.len() == 1 {
                return Ok(Command::Eq(EqRequest::Preset(EqPreset::Flat)));
            }
            return Err("usage: eq flat".to_string());
        }
        "preset" => {
            let Some(name) = args.get(1) else {
                return Err(format!(
                    "usage: eq preset <{}>",
                    EqPreset::names().replace(", ", "|")
                ));
            };
            let Some(preset) = EqPreset::parse(name) else {
                return Err(format!(
                    "unknown preset {name:?}; available: {}",
                    EqPreset::names()
                ));
            };
            return Ok(Command::Eq(EqRequest::Preset(preset)));
        }
        _ => {}
    }

    if let Some(preset) = EqPreset::parse(first) {
        if args.len() == 1 {
            return Ok(Command::Eq(EqRequest::Preset(preset)));
        }
        return Err(format!("usage: eq {}", preset.name()));
    }

    let frequency = parse_frequency(first)?;
    let Some(index) = eq::band_index(frequency) else {
        return Err(format!(
            "no band near {frequency:.0} Hz; bands: {}",
            eq::BAND_LABELS.join(", ")
        ));
    };

    let Some(gain) = args.get(1) else {
        return Err("usage: eq <band> <decibels>, e.g. eq 62 +3".to_string());
    };
    if args.len() > 2 {
        return Err("usage: eq <band> <decibels>, e.g. eq 62 +3".to_string());
    }

    let gain_db = parse_decibels(gain)?;
    if !(-60.0..=60.0).contains(&gain_db) {
        return Err(format!(
            "EQ gain must be between -{} and +{} dB",
            eq::MAX_BAND_GAIN_DB as i32,
            eq::MAX_BAND_GAIN_DB as i32
        ));
    }

    Ok(Command::Eq(EqRequest::Band {
        index,
        gain_db: gain_db.round() as i32,
    }))
}

fn parse_loop(args: &[&str]) -> Result<Command, String> {
    let Some(token) = args.first() else {
        return Ok(Command::Loop(None));
    };
    if args.len() > 1 {
        return Err("usage: loop [on|off|toggle]".to_string());
    }

    let action = match token.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => LoopAction::On,
        "off" | "false" | "0" | "no" => LoopAction::Off,
        "toggle" | "switch" => LoopAction::Toggle,
        other => return Err(format!("invalid loop state {other:?} (use on|off|toggle)")),
    };

    Ok(Command::Loop(Some(action)))
}

fn parse_frequency(token: &str) -> Result<f32, String> {
    let lower = token.trim().to_ascii_lowercase();
    let (number, multiplier) = if let Some(value) = lower.strip_suffix("khz") {
        (value, 1_000.0)
    } else if let Some(value) = lower.strip_suffix("hz") {
        (value, 1.0)
    } else if let Some(value) = lower.strip_suffix('k') {
        (value, 1_000.0)
    } else {
        (lower.as_str(), 1.0)
    };

    let value = number
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("invalid band frequency: {token:?}"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("invalid band frequency: {token:?}"));
    }

    Ok(value * multiplier)
}

fn parse_decibels(token: &str) -> Result<f32, String> {
    let lower = token.trim().to_ascii_lowercase();
    let value = lower.strip_suffix("db").unwrap_or(lower.as_str());
    value
        .trim()
        .parse::<f32>()
        .map_err(|_| format!("invalid gain: {token:?} (expected decibels)"))
}

/// Parses `M:SS`, `H:MM:SS`, seconds (`90`), or `1.5s` / `500ms`.
pub fn parse_time_ms(token: &str) -> Result<u64, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("missing time value".to_string());
    }

    if token.contains(':') {
        let parts: Vec<&str> = token.split(':').collect();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.is_empty()) {
            return Err(format!("invalid time {token:?} (use M:SS or H:MM:SS)"));
        }

        let mut total = 0.0f64;
        for part in parts {
            let value = part
                .parse::<f64>()
                .map_err(|_| format!("invalid time {token:?}"))?;
            if !value.is_finite() || value < 0.0 {
                return Err(format!("invalid time {token:?}"));
            }
            total = total * 60.0 + value;
        }

        return Ok((total * 1_000.0).round() as u64);
    }

    let lower = token.to_ascii_lowercase();
    let (number, multiplier) = if let Some(value) = lower.strip_suffix("ms") {
        (value, 1.0)
    } else if let Some(value) = lower.strip_suffix('s') {
        (value, 1_000.0)
    } else {
        (lower.as_str(), 1_000.0)
    };

    let value = number
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("invalid time {token:?}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err(format!("invalid time {token:?}"));
    }

    Ok((value * multiplier).round() as u64)
}

/// Formats milliseconds as `M:SS`, or `H:MM:SS` past one hour.
pub fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Values shown on the live status line.
#[derive(Clone, Copy, Debug)]
pub struct StatusInfo<'a> {
    pub position_ms: u64,
    pub duration_ms: u64,
    pub paused: bool,
    pub volume_percent: f32,
    pub eq_label: &'a str,
    pub looping: bool,
}

pub fn status_line(info: &StatusInfo<'_>) -> String {
    let position = info.position_ms.min(info.duration_ms);
    let remaining = info.duration_ms.saturating_sub(position);

    format!(
        "[RACF] {:<7} {} / {}  -{}  | vol {:.0}% | eq {} | loop {} | : commands",
        if info.paused { "paused" } else { "playing" },
        format_duration(position),
        format_duration(info.duration_ms),
        format_duration(remaining),
        info.volume_percent,
        info.eq_label,
        if info.looping { "on" } else { "off" },
    )
}

pub fn help_text() -> String {
    format!(
        "Commands (press : to open the prompt)\n\
         \x20 volume [0-200|+N|-N]      show or set master volume\n\
         \x20 playback <time|+/-time>    seek, e.g. 1:00, 90, 1.5s, +30\n\
         \x20 pause | play | toggle      control playback\n\
         \x20 eq                         show the ten bands\n\
         \x20 eq off                     bypass the equalizer\n\
         \x20 eq <preset>                {}\n\
         \x20 eq <band> <dB>             e.g. eq 62 +3, eq 8k -2\n\
         \x20 loop [on|off|toggle]       repeat the timeline\n\
         \x20 status | help | quit\n\
         Bands: {} Hz\n\
         Shortcuts: space pause, left/right seek 5s, up/down volume 5, q quit",
        EqPreset::names(),
        eq::BAND_LABELS.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_forms() {
        assert_eq!(parse("volume"), Ok(Command::Volume(None)));
        assert_eq!(
            parse("volume 90"),
            Ok(Command::Volume(Some(VolumeChange::Set(90.0))))
        );
        assert_eq!(
            parse("volume=90%"),
            Ok(Command::Volume(Some(VolumeChange::Set(90.0))))
        );
        assert_eq!(
            parse("vol +10"),
            Ok(Command::Volume(Some(VolumeChange::Delta(10.0))))
        );
        assert_eq!(
            parse(":volume -15"),
            Ok(Command::Volume(Some(VolumeChange::Delta(-15.0))))
        );
    }

    #[test]
    fn rejects_bad_volume_values() {
        assert!(parse("volume 90 100").is_err());
        assert!(parse("volume loud").is_err());
        assert!(parse("volume NaN").is_err());
    }

    #[test]
    fn parses_absolute_and_relative_seeks() {
        assert_eq!(
            parse("playback 1:00"),
            Ok(Command::Seek(SeekRequest::Absolute(60_000)))
        );
        assert_eq!(
            parse("playback=1:02:03"),
            Ok(Command::Seek(SeekRequest::Absolute(3_723_000)))
        );
        assert_eq!(
            parse("seek +30"),
            Ok(Command::Seek(SeekRequest::Relative(30_000)))
        );
        assert_eq!(
            parse("seek -1:30"),
            Ok(Command::Seek(SeekRequest::Relative(-90_000)))
        );
        assert_eq!(
            parse("seek 1.5s"),
            Ok(Command::Seek(SeekRequest::Absolute(1_500)))
        );
        assert_eq!(
            parse("seek 250ms"),
            Ok(Command::Seek(SeekRequest::Absolute(250)))
        );
    }

    #[test]
    fn parses_eq_commands() {
        assert_eq!(parse("eq"), Ok(Command::Eq(EqRequest::Show)));
        assert_eq!(parse("eq off"), Ok(Command::Eq(EqRequest::Off)));
        assert_eq!(
            parse("eq bass"),
            Ok(Command::Eq(EqRequest::Preset(EqPreset::Bass)))
        );
        assert_eq!(
            parse("eq preset vocal"),
            Ok(Command::Eq(EqRequest::Preset(EqPreset::Vocal)))
        );
        assert_eq!(
            parse("eq 62 +3"),
            Ok(Command::Eq(EqRequest::Band {
                index: 1,
                gain_db: 3
            }))
        );
        assert_eq!(
            parse("eq 8k -2.4"),
            Ok(Command::Eq(EqRequest::Band {
                index: 8,
                gain_db: -2
            }))
        );
    }

    #[test]
    fn parses_loop_and_transport_commands() {
        assert_eq!(parse("loop"), Ok(Command::Loop(None)));
        assert_eq!(parse("loop on"), Ok(Command::Loop(Some(LoopAction::On))));
        assert_eq!(
            parse("repeat toggle"),
            Ok(Command::Loop(Some(LoopAction::Toggle)))
        );
        assert_eq!(parse("pause"), Ok(Command::Pause));
        assert_eq!(parse("resume"), Ok(Command::Resume));
        assert_eq!(parse("toggle"), Ok(Command::TogglePause));
        assert_eq!(parse("status"), Ok(Command::Status));
        assert_eq!(parse("help"), Ok(Command::Help));
        assert_eq!(parse("quit"), Ok(Command::Quit));
    }

    #[test]
    fn reports_unknown_commands_and_missing_arguments() {
        assert!(parse("warp 9").is_err());
        assert!(parse("playback").is_err());
        assert!(parse("eq 60").is_err());
        assert!(parse("eq 300 2").is_err());
        assert!(parse("loop maybe").is_err());
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(1_000), "0:01");
        assert_eq!(format_duration(65_000), "1:05");
        assert_eq!(format_duration(3_723_000), "1:02:03");
    }

    #[test]
    fn builds_a_live_status_line() {
        let line = status_line(&StatusInfo {
            position_ms: 61_000,
            duration_ms: 394_501,
            paused: false,
            volume_percent: 90.0,
            eq_label: "bass",
            looping: true,
        });

        assert!(line.contains("playing"));
        assert!(line.contains("1:01 / 6:34"));
        assert!(line.contains("-5:33"));
        assert!(line.contains("vol 90%"));
        assert!(line.contains("eq bass"));
        assert!(line.contains("loop on"));
    }
}
