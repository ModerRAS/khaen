//! Datastore value codec (B / I / I16 / F / S / A wire types).

use crate::error::CueMixError;
use crate::props::PropType;

/// Numerator of the 8.24 fixed-point fader representation.
pub const FADER_SCALE: i64 = 1 << 24;

/// One decoded datastore value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// B: one unsigned byte.
    Byte(u8),
    /// I: big-endian i32.
    Int(i32),
    /// I16: big-endian u16.
    UInt16(u16),
    /// F: 8.24 fixed point, kept as f64 (exact binary fraction).
    Fader(f64),
    /// S: 32-byte NUL-padded UTF-8 string.
    Str(String),
    /// A: variable-length bytes.
    Bytes(Vec<u8>),
}

impl Value {
    /// Wire type of this value.
    pub const fn type_of(&self) -> PropType {
        match self {
            Self::Byte(_) => PropType::Byte,
            Self::Int(_) => PropType::Int,
            Self::UInt16(_) => PropType::UInt16,
            Self::Fader(_) => PropType::Fader,
            Self::Str(_) => PropType::Str,
            Self::Bytes(_) => PropType::Bytes,
        }
    }

    /// Decode a wire value of the given type; None when the bytes are too short.
    pub fn decode(ty: PropType, data: &[u8]) -> Option<Self> {
        match ty {
            PropType::Byte => data.first().map(|b| Self::Byte(*b)),
            PropType::Int => {
                if data.len() < 4 {
                    return None;
                }
                let raw = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                Some(Self::Int(raw))
            }
            PropType::UInt16 => {
                if data.len() < 2 {
                    return None;
                }
                Some(Self::UInt16(u16::from_be_bytes([data[0], data[1]])))
            }
            PropType::Fader => {
                if data.len() < 4 {
                    return None;
                }
                let raw = i32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                Some(Self::Fader(fader_from_raw(raw)))
            }
            PropType::Str => {
                let raw = if data.len() > 32 { &data[..32] } else { data };
                let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                Some(Self::Str(String::from_utf8_lossy(&raw[..end]).into_owned()))
            }
            PropType::Bytes => Some(Self::Bytes(data.to_vec())),
        }
    }

    /// Encode this value for a host-to-device write; the type must match the
    /// property type or the write is refused ([CueMixError::Unsupported]).
    pub fn encode(&self, ty: PropType) -> Result<Vec<u8>, CueMixError> {
        if ty != self.type_of() {
            return Err(CueMixError::Unsupported {
                what: "value type does not match datastore property type",
            });
        }
        match self {
            Self::Byte(b) => Ok(vec![*b]),
            Self::Int(i) => Ok(i.to_be_bytes().to_vec()),
            Self::UInt16(u) => Ok(u.to_be_bytes().to_vec()),
            Self::Fader(f) => {
                let raw = fader_to_raw(*f)?;
                Ok(raw.to_be_bytes().to_vec())
            }
            Self::Str(s) => {
                let bytes = s.as_bytes();
                let mut out = [0u8; 32];
                let n = bytes.len().min(31);
                out[..n].copy_from_slice(&bytes[..n]);
                Ok(out.to_vec())
            }
            Self::Bytes(b) => Ok(b.clone()),
        }
    }

    /// Byte view for B values.
    pub fn as_u8(&self) -> Option<u8> {
        match self {
            Self::Byte(b) => Some(*b),
            _ => None,
        }
    }

    /// Bool view for B values (0 = false, non-zero = true).
    pub fn as_bool(&self) -> Option<bool> {
        self.as_u8().map(|b| b != 0)
    }

    /// i32 view for I values.
    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Self::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// u16 view for I16 values.
    pub fn as_u16(&self) -> Option<u16> {
        match self {
            Self::UInt16(u) => Some(*u),
            _ => None,
        }
    }

    /// f64 view for F values.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Fader(f) => Some(*f),
            _ => None,
        }
    }

    /// str view for S values.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Bytes view for A values.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    /// Stable display form used in write reports and error messages.
    pub fn describe(&self) -> String {
        match self {
            Self::Byte(b) => format!("{}", b),
            Self::Int(i) => format!("{}", i),
            Self::UInt16(u) => format!("{}", u),
            Self::Fader(f) => format!("{}", f),
            Self::Str(s) => format!("\"{}\"", s),
            Self::Bytes(b) => format!("{} bytes", b.len()),
        }
    }
}

/// Convert an 8.24 raw fader word to f64 (exact binary fraction).
pub fn fader_from_raw(raw: i32) -> f64 {
    raw as f64 / FADER_SCALE as f64
}

/// Convert f64 to the nearest 8.24 raw word; refuses non-finite/out-of-range.
pub fn fader_to_raw(value: f64) -> Result<i32, CueMixError> {
    if !value.is_finite() {
        return Err(CueMixError::Range {
            field: "fader",
            reason: "value is not finite".to_string(),
        });
    }
    let scaled = value * FADER_SCALE as f64;
    if scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
        return Err(CueMixError::Range {
            field: "fader",
            reason: "value out of 8.24 fixed point range".to_string(),
        });
    }
    Ok(scaled.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_all_wire_types() {
        assert_eq!(Value::decode(PropType::Byte, &[7]), Some(Value::Byte(7)));
        assert_eq!(
            Value::decode(PropType::Int, &48000i32.to_be_bytes()),
            Some(Value::Int(48000))
        );
        assert_eq!(
            Value::decode(PropType::UInt16, &10u16.to_be_bytes()),
            Some(Value::UInt16(10))
        );
        let raw = (0.5f64 * FADER_SCALE as f64) as i32;
        assert_eq!(
            Value::decode(PropType::Fader, &raw.to_be_bytes()),
            Some(Value::Fader(0.5))
        );
        let mut s = [0u8; 32];
        s[..6].copy_from_slice(b"Line 7");
        assert_eq!(
            Value::decode(PropType::Str, &s),
            Some(Value::Str("Line 7".to_string()))
        );
        assert_eq!(
            Value::decode(PropType::Bytes, &[1, 2, 3]),
            Some(Value::Bytes(vec![1, 2, 3]))
        );
    }

    #[test]
    fn short_payloads_do_not_decode() {
        assert_eq!(Value::decode(PropType::Byte, &[]), None);
        assert_eq!(Value::decode(PropType::Int, &[0, 0]), None);
        assert_eq!(Value::decode(PropType::UInt16, &[0]), None);
        assert_eq!(Value::decode(PropType::Fader, &[0, 0, 0]), None);
        assert_eq!(
            Value::decode(PropType::Str, &[]),
            Some(Value::Str(String::new()))
        );
    }

    #[test]
    fn encode_roundtrips_and_rejects_mismatched_types() {
        let v = Value::Fader(0.5);
        let bytes = v.encode(PropType::Fader).expect("encode fader");
        assert_eq!(bytes, 0x0080_0000i32.to_be_bytes().to_vec());
        assert_eq!(
            Value::decode(PropType::Fader, &bytes),
            Some(Value::Fader(0.5))
        );

        let s = Value::Str("x".repeat(40));
        let bytes = s.encode(PropType::Str).expect("encode string");
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[31], 0);
        assert!(matches!(
            v.encode(PropType::Byte),
            Err(CueMixError::Unsupported { .. })
        ));
    }

    #[test]
    fn fader_raw_matches_reference_rounding() {
        // Python: int(round(value * 0x01000000)); -0.1 -> -1677722.
        assert_eq!(fader_to_raw(1.0).expect("raw"), 16_777_216);
        assert_eq!(fader_to_raw(0.5).expect("raw"), 8_388_608);
        assert_eq!(fader_to_raw(-0.1).expect("raw"), -1_677_722);
        assert_eq!(fader_from_raw(-1_677_722), -0.100_000_023_841_857_91);
        let mut v = fader_to_raw(0.3).expect("raw");
        let before = fader_from_raw(v);
        v = fader_to_raw(before).expect("raw again");
        assert_eq!(fader_from_raw(v), before);
    }

    #[test]
    fn fader_rejects_non_finite_and_huge() {
        assert!(matches!(
            fader_to_raw(f64::NAN),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            fader_to_raw(f64::INFINITY),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            fader_to_raw(1.0e9),
            Err(CueMixError::Range { .. })
        ));
    }

    #[test]
    fn string_decode_stops_at_first_nul() {
        let mut raw = [b'a'; 32];
        raw[3] = 0;
        assert_eq!(
            Value::decode(PropType::Str, &raw),
            Some(Value::Str("aaa".to_string()))
        );
    }
}
