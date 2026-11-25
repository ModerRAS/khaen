//! Sample format conversion helpers (f32 <-> L24 / L16, big-endian on the wire).

/// Convert normalized f32 sample to 24-bit signed integer (AES67 L24).
/// Values are clamped to [-1.0, +1.0) and rounded to nearest.
pub fn f32_to_l24(x: f32) -> i32 {
    let v = (x.clamp(-1.0, 1.0 - 1.0 / 8_388_608.0) * 8_388_608.0).round();
    (v as i32).clamp(-8_388_608, 8_388_607)
}

pub fn l24_to_f32(v: i32) -> f32 {
    (v.clamp(-8_388_608, 8_388_607) as f32) / 8_388_608.0
}

pub fn f32_to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

pub fn i16_to_f32(v: i16) -> f32 {
    v as f32 / 32_768.0
}

/// Store a 24-bit two's complement sample in 3 bytes, big endian (wire format).
pub fn store_l24_be(v: i32, out: &mut [u8]) {
    debug_assert!(out.len() >= 3);
    let u = (v & 0x00FF_FFFF) as u32;
    out[0] = (u >> 16) as u8;
    out[1] = (u >> 8) as u8;
    out[2] = u as u8;
}

/// Load a 24-bit two's complement big-endian sample (sign-extended).
pub fn load_l24_be(b: &[u8]) -> i32 {
    debug_assert!(b.len() >= 3);
    let u = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
    if u & 0x0080_0000 != 0 {
        (u | 0xFF00_0000) as i32
    } else {
        u as i32
    }
}

/// Encode interleaved f32 samples as L24 big-endian `out` (must be 3x samples len).
pub fn encode_l24(samples: &[f32], out: &mut [u8]) -> usize {
    let n = samples.len();
    debug_assert!(out.len() >= n * 3);
    for (i, s) in samples.iter().enumerate() {
        store_l24_be(f32_to_l24(*s), &mut out[i * 3..i * 3 + 3]);
    }
    n * 3
}

/// Decode L24 big-endian payload into interleaved f32 (appended to `out`).
pub fn decode_l24(payload: &[u8], out: &mut Vec<f32>) -> usize {
    let n = payload.len() / 3;
    out.reserve(n);
    for i in 0..n {
        out.push(l24_to_f32(load_l24_be(&payload[i * 3..i * 3 + 3])));
    }
    n
}

/// Encode interleaved f32 samples as L16 little-endian (WAV/capture convenience).
pub fn encode_l16_le(samples: &[f32], out: &mut Vec<u8>) {
    out.reserve(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&f32_to_i16(*s).to_le_bytes());
    }
}

/// Parse a hex argument like `0x4d2` or decimal into u32 (used for SSRC/seq/ts).
pub fn parse_u32_arg(s: &str) -> anyhow::Result<u32> {
    let t = s.trim();
    let (radix, digits) = if let Some(rest) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))
    {
        (16, rest)
    } else {
        (10, t)
    };
    u32::from_str_radix(digits, radix).map_err(|_| anyhow::anyhow!("invalid u32 value {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l24_roundtrip_extremes_and_zero() {
        for v in [
            0, 1, -1, 8_388_607, -8_388_608, 123_456, -123_456, 4_194_304,
        ] {
            let mut b = [0u8; 3];
            store_l24_be(v, &mut b);
            assert_eq!(load_l24_be(&b), v, "value {v} bytes {b:?}");
        }
        assert_eq!(f32_to_l24(0.0), 0);
        assert_eq!(f32_to_l24(1.0), 8_388_607);
        assert_eq!(f32_to_l24(-1.0), -8_388_608);
        // out-of-range clamps
        assert_eq!(f32_to_l24(2.0), 8_388_607);
        assert_eq!(f32_to_l24(-2.0), -8_388_608);
    }

    #[test]
    fn l24_wire_order_is_big_endian() {
        let mut b = [0u8; 3];
        store_l24_be(0x123456, &mut b);
        assert_eq!(b, [0x12, 0x34, 0x56]);
        store_l24_be(-2, &mut b);
        assert_eq!(b, [0xFF, 0xFF, 0xFE]);
    }

    #[test]
    fn encode_decode_roundtrip_sine() {
        let n = 48 * 2;
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f32 / 48_000.0;
            samples.push((2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.5);
        }
        let mut bytes = vec![0u8; n * 3];
        assert_eq!(encode_l24(&samples, &mut bytes), n * 3);
        let mut back = Vec::new();
        decode_l24(&bytes, &mut back);
        assert_eq!(back.len(), n);
        for (a, b) in samples.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1.0 / 4_000_000.0, "{a} vs {b}");
        }
    }

    #[test]
    fn parses_u32_args() {
        assert_eq!(parse_u32_arg("1234").unwrap(), 1234);
        assert_eq!(parse_u32_arg("0x4d2").unwrap(), 1234);
        assert!(parse_u32_arg("xyz").is_err());
    }
}
