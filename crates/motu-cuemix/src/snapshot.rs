//! Read-only JSON snapshot of the datastore mirror.
//!
//! Shape matches the device-side reference client's --save output so snapshots
//! are directly comparable with C:\\WorkSpace\\Audio\\tmp\\cuemix-baseline.json:
//!   {"meta": {...}, "values": {"kName": [values...]}}

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::props;
use crate::value::Value;

/// Metadata header of a snapshot.
#[derive(Clone, Debug, Default)]
pub struct SnapshotMeta {
    /// "ws:host:port" style transport description.
    pub transport: String,
    /// Model id (kModelID), e.g. "UL5L".
    pub model: Option<String>,
    /// Device name (kDevName), e.g. "UltraLite-mk5".
    pub name: Option<String>,
    /// Device UID bytes (kDevUID).
    pub uid: Option<Vec<u8>>,
    /// At least one datastore message was received.
    pub authenticated: bool,
    /// Datastore messages received.
    pub messages: u64,
    /// Unknown property ids ignored.
    pub unknown_ids: u64,
    /// Password enabled flag reported by the device, when known.
    pub password_required: Option<bool>,
}

/// Escape a string as a JSON string literal (including the surrounding quotes).
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// JSON number for an f64; non-finite values become null.
///
/// Integral values keep one decimal (like Python json.dump: 1.0 -> "1.0") so
/// snapshots diff cleanly against the reference client output.
pub fn json_f64(value: f64) -> String {
    if !value.is_finite() {
        return "null".to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1.0e16 {
        return format!("{:.1}", value);
    }
    format!("{}", value)
}

fn push_value(out: &mut String, value: Option<&Value>) {
    match value {
        None => out.push_str("null"),
        Some(Value::Byte(byte)) => out.push_str(&byte.to_string()),
        Some(Value::Int(int)) => out.push_str(&int.to_string()),
        Some(Value::UInt16(uint)) => out.push_str(&uint.to_string()),
        Some(Value::Fader(fader)) => out.push_str(&json_f64(*fader)),
        Some(Value::Str(text)) => out.push_str(&json_string(text)),
        Some(Value::Bytes(bytes)) => {
            out.push('[');
            for (index, byte) in bytes.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&byte.to_string());
            }
            out.push(']');
        }
    }
}

fn push_string_array(out: &mut String, values: &Option<Vec<u8>>) {
    match values {
        None => out.push_str("null"),
        Some(bytes) => {
            out.push('[');
            for (index, byte) in bytes.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&byte.to_string());
            }
            out.push(']');
        }
    }
}

fn push_optional_string(out: &mut String, name: &str, value: &Option<String>) {
    out.push_str(name);
    out.push_str(": ");
    match value {
        Some(text) => out.push_str(&json_string(text)),
        None => out.push_str("null"),
    }
    out.push_str(",\n");
}

/// Render a snapshot JSON document from mirror values and metadata.
pub fn render(values: &BTreeMap<&'static str, Vec<Option<Value>>>, meta: &SnapshotMeta) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    let mut out = String::with_capacity(4096);
    out.push_str("{\n \"meta\": {\n");
    out.push_str(&format!("  \"tool\": \"khaen-motu/{}\",\n", crate::VERSION));
    out.push_str(&format!(
        "  \"transport\": {},\n",
        json_string(&meta.transport)
    ));
    out.push_str(&format!("  \"saved_unix\": {},\n", json_f64(now)));
    push_optional_string(&mut out, "  \"model\"", &meta.model);
    push_optional_string(&mut out, "  \"name\"", &meta.name);
    out.push_str("  \"uid\": ");
    push_string_array(&mut out, &meta.uid);
    out.push_str(",\n");
    out.push_str(&format!("  \"authenticated\": {},\n", meta.authenticated));
    out.push_str(&format!("  \"messages\": {},\n", meta.messages));
    out.push_str(&format!("  \"unknown_ids\": {},\n", meta.unknown_ids));
    out.push_str(&format!(
        "  \"password_required\": {}\n",
        match meta.password_required {
            Some(value) => value.to_string(),
            None => "null".to_string(),
        }
    ));
    out.push_str(" },\n \"values\": {\n");
    let mut emitted = 0usize;
    let mut first_key = true;
    for prop in props::PROPS {
        let Some(array) = values.get(prop.key) else {
            continue;
        };
        if array.iter().all(|value| value.is_none()) {
            continue;
        }
        if first_key {
            first_key = false;
        } else {
            out.push_str(",\n");
        }
        out.push_str("  ");
        out.push_str(&json_string(prop.key));
        out.push_str(": [");
        let mut first = true;
        for value in array {
            if !first {
                out.push(',');
            }
            first = false;
            push_value(&mut out, value.as_ref());
        }
        out.push(']');
        emitted += 1;
    }
    if emitted > 0 {
        out.push('\n');
    }
    out.push_str(" }\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_values() -> BTreeMap<&'static str, Vec<Option<Value>>> {
        let mut values = BTreeMap::new();
        values.insert(crate::props::keys::MUTE_ENABLE, vec![Some(Value::Byte(0))]);
        values.insert(
            crate::props::keys::SAMPLE_RATE,
            vec![Some(Value::Int(48000))],
        );
        values.insert(
            crate::props::keys::KO_BUS_FADER,
            vec![Some(Value::Fader(1.0)), Some(Value::Fader(0.5))],
        );
        values.insert(
            crate::props::keys::DEV_NAME,
            vec![Some(Value::Str("UltraLite-mk5".to_string()))],
        );
        values.insert(
            crate::props::keys::DEV_UID,
            vec![Some(Value::Bytes(vec![85, 76, 53]))],
        );
        values.insert(crate::props::keys::MODEL_ID, vec![None, None]);
        values
    }

    fn sample_meta() -> SnapshotMeta {
        SnapshotMeta {
            transport: "ws:169.254.111.126:1280".to_string(),
            model: Some("UL5L".to_string()),
            name: Some("UltraLite-mk5".to_string()),
            uid: Some(vec![85, 76, 53]),
            authenticated: true,
            messages: 2498,
            unknown_ids: 3,
            password_required: Some(false),
        }
    }

    #[test]
    fn renders_meta_and_values() {
        let json = render(&sample_values(), &sample_meta());
        assert!(json.starts_with("{\n"));
        assert!(json.contains("\"tool\": \"khaen-motu/"));
        assert!(json.contains("\"transport\": \"ws:169.254.111.126:1280\""));
        assert!(json.contains("\"model\": \"UL5L\""));
        assert!(json.contains("\"authenticated\": true"));
        assert!(json.contains("\"messages\": 2498"));
        assert!(json.contains("\"kMuteEnable\": [0]"));
        assert!(json.contains("\"kSampleRate\": [48000]"));
        assert!(json.contains("\"koBusFader\": [1.0,0.5]"));
        assert!(json.contains("\"kDevName\": [\"UltraLite-mk5\"]"));
        assert!(json.contains("\"kDevUID\": [[85,76,53]]"));
        // All-null arrays are skipped, like the reference client.
        assert!(!json.contains("kModelID"));
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert_eq!(json.matches('[').count(), json.matches(']').count());
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_string("\u{01}"), "\"\\u0001\"");
    }

    #[test]
    fn formats_numbers_for_snapshot_diffing() {
        assert_eq!(json_f64(1.0), "1.0");
        assert_eq!(json_f64(0.5), "0.5");
        assert_eq!(json_f64(-0.0), "-0.0");
        assert_eq!(json_f64(f64::NAN), "null");
        assert_eq!(json_f64(f64::INFINITY), "null");
    }

    #[test]
    fn empty_mirror_renders_empty_values_object() {
        let json = render(&BTreeMap::new(), &SnapshotMeta::default());
        assert!(json.contains("\"values\": {\n }"));
        assert!(!json.contains("kMuteEnable"));
    }
}
