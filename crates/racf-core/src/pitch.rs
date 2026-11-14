/// Converts a scientific pitch name (`C4`, `F#5`, `Bb2`) into hertz.
///
/// `A4` is 440 Hz and the octave numbering matches MIDI (`C4` = MIDI 60).
pub fn parse_pitch(pitch: &str) -> f32 {
    let pitch = pitch.trim();
    let mut chars = pitch.chars().peekable();

    let note = chars.next().unwrap_or('C').to_ascii_uppercase();
    let mut alter = 0;

    match chars.peek().copied() {
        Some('#') | Some('♯') => {
            alter = 1;
            chars.next();
        }
        Some('b') | Some('♭') => {
            alter = -1;
            chars.next();
        }
        _ => {}
    }

    let octave = chars.collect::<String>().trim().parse::<i32>().unwrap_or(4);

    let semitone = match note {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => 0,
    };

    let midi_note = (octave + 1) * 12 + semitone + alter;
    440.0 * 2.0_f32.powf((midi_note - 69) as f32 / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.01,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn concert_a_is_440_hz() {
        assert_close(parse_pitch("A4"), 440.0);
    }

    #[test]
    fn middle_c_is_midi_60() {
        assert_close(parse_pitch("C4"), 261.6256);
    }

    #[test]
    fn accidentals_and_unicode_are_supported() {
        assert_close(parse_pitch("C#4"), parse_pitch("D♭4"));
    }

    #[test]
    fn invalid_notes_fall_back_to_c() {
        assert_close(parse_pitch("Q4"), parse_pitch("C4"));
    }
}
