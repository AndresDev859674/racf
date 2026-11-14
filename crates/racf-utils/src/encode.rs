use racf_core::SampleEncoding;

/// Encodes normalized `f32` samples into the compact representation stored in
/// a RACF `<data>` element.
pub fn encode(samples: &[f32], encoding: SampleEncoding) -> Vec<u8> {
    match encoding {
        SampleEncoding::Mulaw => samples.iter().copied().map(encode_mulaw).collect(),
        SampleEncoding::PcmU8 => samples
            .iter()
            .map(|sample| (sample.clamp(-1.0, 1.0) * 127.0).round() as i32 + 128)
            .map(|sample| sample.clamp(0, 255) as u8)
            .collect(),
        SampleEncoding::PcmI16Le => samples
            .iter()
            .flat_map(|sample| {
                let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                value.to_le_bytes()
            })
            .collect(),
    }
}

/// Encodes one normalized sample as G.711 µ-law.
pub fn encode_mulaw(sample: f32) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32635;

    let mut value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i32;
    let sign = if value < 0 { 0x80u8 } else { 0 };
    if value < 0 {
        value = -value;
    }

    value = value.min(CLIP) + BIAS;

    let mut exponent = 7i32;
    let mut mask = 0x4000i32;
    while exponent > 0 && value & mask == 0 {
        exponent -= 1;
        mask >>= 1;
    }

    let mantissa = (value >> (exponent + 3)) & 0x0f;
    !(sign | ((exponent as u8) << 4) | mantissa as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use racf_core::decode_mulaw;

    #[test]
    fn mulaw_roundtrip_is_close_for_normal_levels() {
        for sample in [-0.9, -0.5, -0.1, 0.0, 0.1, 0.5, 0.9] {
            let decoded = decode_mulaw(encode_mulaw(sample));
            assert!(
                (decoded - sample).abs() < 0.05,
                "expected {sample}, got {decoded}"
            );
        }
    }

    #[test]
    fn pcm16_roundtrip_is_accurate() {
        let samples = [-0.75f32, 0.0, 0.25];
        let encoded = encode(&samples, SampleEncoding::PcmI16Le);
        let decoded: Vec<f32> = encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| i16::from_le_bytes(*bytes) as f32 / 32768.0)
            .collect();

        for (actual, expected) in decoded.iter().zip(samples) {
            assert!((actual - expected).abs() < 0.001);
        }
    }

    #[test]
    fn pcm8_has_one_byte_per_sample() {
        assert_eq!(encode(&[0.0, 0.5, -0.5], SampleEncoding::PcmU8).len(), 3);
    }
}
