//! Fader linear <-> dB helpers and linear range validation.
//!
//! The UltraLite-mk5 bus faders are 0.0..=1.0 linear amplitude (snapshot
//! baseline: koBusFader = [1.0; 14] at unity). dB is 20*log10(linear), so
//! linear 1.0 = 0 dB and linear 0.0 = silence (-inf).

use crate::error::CueMixError;
use crate::value::{fader_from_raw, fader_to_raw};

/// Highest accepted linear fader value (0 dB).
pub const FADER_MAX: f64 = 1.0;
/// Lowest accepted linear fader value (silence).
pub const FADER_MIN: f64 = 0.0;
/// Approximate dB floor of the 8.24 fixed point format (one raw LSB).
pub const FADER_DB_FLOOR: f64 = -144.49;

/// Linear amplitude -> dB; None for silence or non-positive/non-finite input.
pub fn fader_to_db(linear: f64) -> Option<f64> {
    if !linear.is_finite() || linear <= 0.0 {
        return None;
    }
    Some(20.0 * linear.log10())
}

/// dB -> linear amplitude in 0.0..=1.0; refuses positive dB and non-finite input.
///
/// Values at or below the 8.24 floor collapse to 0.0 (silence).
pub fn db_to_fader(db: f64) -> Result<f64, CueMixError> {
    if !db.is_finite() || db > 0.0 {
        return Err(CueMixError::Range {
            field: "volume_db",
            reason: "dB must be finite and <= 0".to_string(),
        });
    }
    let linear = 10f64.powf(db / 20.0);
    Ok(linear.clamp(FADER_MIN, FADER_MAX))
}

/// Validate a linear volume and quantize it to the device 8.24 grid.
///
/// This is the value written to koBusFader and the value compared on readback;
/// both sides use the same quantized f64 so equality is exact.
pub fn quantize_linear(linear: f64) -> Result<f64, CueMixError> {
    if !linear.is_finite() {
        return Err(CueMixError::Range {
            field: "volume",
            reason: "volume is not finite".to_string(),
        });
    }
    if !(FADER_MIN..=FADER_MAX).contains(&linear) {
        return Err(CueMixError::Range {
            field: "volume",
            reason: format!("volume {} outside 0.0..=1.0", linear),
        });
    }
    let raw = fader_to_raw(linear)?;
    Ok(fader_from_raw(raw))
}

/// Human-readable dB label for a linear value ("0.00 dB", "-6.02 dB", "-inf dB").
pub fn format_db(linear: f64) -> String {
    match fader_to_db(linear) {
        Some(db) => format!("{:.2} dB", db),
        None => "-inf dB".to_string(),
    }
}

/// Human-readable volume description used by the CLI ("0.500000 (-6.02 dB)").
pub fn describe_linear(linear: f64) -> String {
    format!("{:.6} ({})", linear, format_db(linear))
}

/// Parse a CLI volume target: either a linear 0..=1 value or a dB string
/// ending in "db" (case-insensitive, e.g. "-6dB", "-6.02 db").
pub fn parse_volume_text(text: &str) -> Result<f64, CueMixError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CueMixError::Range {
            field: "volume",
            reason: "empty volume argument".to_string(),
        });
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(db_text) = lower.strip_suffix("db") {
        let db: f64 = db_text.trim().parse().map_err(|_| CueMixError::Range {
            field: "volume_db",
            reason: format!("cannot parse dB value {:?}", trimmed),
        })?;
        return db_to_fader(db);
    }
    let linear: f64 = trimmed.parse().map_err(|_| CueMixError::Range {
        field: "volume",
        reason: format!("cannot parse volume {:?}", trimmed),
    })?;
    quantize_linear(linear)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_is_zero_db() {
        assert_eq!(fader_to_db(1.0), Some(0.0));
        assert_eq!(format_db(1.0), "0.00 dB");
    }

    #[test]
    fn half_is_minus_six_db() {
        let db = fader_to_db(0.5).expect("db");
        assert!((db - -6.020_599_913_3).abs() < 1e-6, "db = {}", db);
        let back = db_to_fader(db).expect("fader");
        assert!((back - 0.5).abs() < 1e-12);
    }

    #[test]
    fn silence_has_no_db() {
        assert_eq!(fader_to_db(0.0), None);
        assert_eq!(format_db(0.0), "-inf dB");
    }

    #[test]
    fn db_to_fader_rejects_positive_and_non_finite() {
        assert!(matches!(db_to_fader(0.1), Err(CueMixError::Range { .. })));
        assert!(matches!(
            db_to_fader(f64::NAN),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            db_to_fader(f64::INFINITY),
            Err(CueMixError::Range { .. })
        ));
        assert_eq!(db_to_fader(0.0).expect("unity"), 1.0);
    }

    #[test]
    fn very_low_db_collapses_to_silence_after_quantization() {
        let linear = db_to_fader(-1000.0).expect("fader");
        assert!(linear > 0.0 && linear < 1e-24, "linear = {}", linear);
        assert_eq!(quantize_linear(linear).expect("quantize"), 0.0);
    }

    #[test]
    fn quantize_is_idempotent_and_range_checked() {
        let q = quantize_linear(0.5).expect("quantize");
        assert_eq!(quantize_linear(q).expect("re-quantize"), q);
        assert!(matches!(
            quantize_linear(1.5),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            quantize_linear(-0.1),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            quantize_linear(f64::NAN),
            Err(CueMixError::Range { .. })
        ));
        assert_eq!(quantize_linear(0.0).expect("zero"), 0.0);
        assert_eq!(quantize_linear(1.0).expect("one"), 1.0);
    }

    #[test]
    fn parses_linear_and_db_text() {
        assert_eq!(parse_volume_text("0.5").expect("linear"), 0.5);
        assert_eq!(parse_volume_text(" 1 ").expect("unity"), 1.0);
        let half_db = parse_volume_text("-6.0206dB").expect("db");
        assert!((half_db - 0.5).abs() < 1e-4);
        assert_eq!(parse_volume_text("0dB").expect("0db"), 1.0);
        assert!(matches!(
            parse_volume_text("3dB"),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            parse_volume_text("1.2"),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            parse_volume_text("loud"),
            Err(CueMixError::Range { .. })
        ));
        assert!(matches!(
            parse_volume_text(""),
            Err(CueMixError::Range { .. })
        ));
    }

    #[test]
    fn describe_linear_includes_db() {
        assert_eq!(describe_linear(0.5), "0.500000 (-6.02 dB)");
        assert_eq!(describe_linear(0.0), "0.000000 (-inf dB)");
    }
}
