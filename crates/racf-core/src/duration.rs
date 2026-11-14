/// Parses a RACF duration such as `1/4`, `3/8`, or `2/1` into beats.
///
/// RACF treats a quarter note (`1/4`) as one beat, so the result is
/// `(numerator / denominator) * 4`.
pub fn parse_duration(value: &str) -> f32 {
    let value = value.trim();
    let mut parts = value.split('/');

    let numerator = parts
        .next()
        .and_then(|part| part.trim().parse::<f32>().ok());
    let denominator = parts
        .next()
        .and_then(|part| part.trim().parse::<f32>().ok());

    match (numerator, denominator, parts.next()) {
        (Some(numerator), Some(denominator), None) if denominator != 0.0 => {
            (numerator / denominator) * 4.0
        }
        _ => 1.0,
    }
}

/// Parses a `targetDuration` value. Accepts `mm:ss`, `hh:mm:ss`, or plain
/// seconds.
pub fn parse_target_duration(value: &str) -> Option<f32> {
    let value = value.trim();
    let parts: Vec<&str> = value.split(':').collect();

    match parts.as_slice() {
        [minutes, seconds] => {
            let minutes = minutes.trim().parse::<f32>().ok()?;
            let seconds = seconds.trim().parse::<f32>().ok()?;
            Some(minutes * 60.0 + seconds)
        }
        [hours, minutes, seconds] => {
            let hours = hours.trim().parse::<f32>().ok()?;
            let minutes = minutes.trim().parse::<f32>().ok()?;
            let seconds = seconds.trim().parse::<f32>().ok()?;
            Some(hours * 3600.0 + minutes * 60.0 + seconds)
        }
        [seconds] => seconds.trim().parse::<f32>().ok(),
        _ => None,
    }
}

/// Parses an absolute time value used by audio clips.
///
/// Accepts milliseconds (`250ms`), seconds (`1.5s`), plain seconds (`1.5`), and
/// `mm:ss` / `hh:mm:ss` clock values.
pub fn parse_time_ms(value: &str) -> Option<u64> {
    let value = value.trim();

    if let Some(milliseconds) = value.strip_suffix("ms") {
        return milliseconds
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|ms| ms.is_finite() && *ms >= 0.0)
            .map(|ms| ms.round() as u64);
    }

    if let Some(seconds) = value.strip_suffix('s') {
        return seconds
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| (seconds * 1000.0).round() as u64);
    }

    parse_target_duration(value)
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| (seconds * 1000.0).round() as u64)
}

/// Converts a number of beats to milliseconds at the given tempo.
pub fn beats_to_ms(beats: f32, bpm: f32) -> f32 {
    if bpm <= 0.0 {
        return 0.0;
    }
    beats * (60_000.0 / bpm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_note_is_one_beat() {
        assert_eq!(parse_duration("1/4"), 1.0);
    }

    #[test]
    fn whole_note_is_four_beats() {
        assert_eq!(parse_duration("1/1"), 4.0);
    }

    #[test]
    fn malformed_duration_falls_back_to_one_beat() {
        assert_eq!(parse_duration("nope"), 1.0);
        assert_eq!(parse_duration("1/0"), 1.0);
    }

    #[test]
    fn target_duration_accepts_clock_and_seconds() {
        assert_eq!(parse_target_duration("01:15"), Some(75.0));
        assert_eq!(parse_target_duration("1:01:00"), Some(3660.0));
        assert_eq!(parse_target_duration("12.5"), Some(12.5));
    }

    #[test]
    fn absolute_times_accept_ms_seconds_and_clock() {
        assert_eq!(parse_time_ms("250ms"), Some(250));
        assert_eq!(parse_time_ms("1.5s"), Some(1500));
        assert_eq!(parse_time_ms("2"), Some(2000));
        assert_eq!(parse_time_ms("01:02.500"), Some(62_500));
        assert_eq!(parse_time_ms("nope"), None);
    }
}
