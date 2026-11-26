//! CueMix 5 datastore message layer (inside WebSocket binary payloads).
//!
//! Two layouts (all integers big endian):
//!   device -> host: id:u16 index:u16 value            (length implied by type)
//!   host -> device: id:u16 index:u16 len:u16 value
//!
//! Special ids: 0xFFFE proxy/auth messages, 0xFFFF proxy status, 6000 meters.
//! A device frame normally carries exactly one datastore update; the parser
//! also accepts several fixed-size updates in one frame.

use crate::error::CueMixError;
use crate::props::{self, Prop};
use crate::value::Value;

/// Proxy/control message id (auth handshake).
pub const PROXY_ID: u16 = 0xFFFE;
/// Opaque proxy status message id.
pub const PROXY_STATUS_ID: u16 = 0xFFFF;
/// Meter stream message id (ignored by the control path).
pub const METERS_ID: u16 = 6000;

/// Proxy sub-message: device public key PEM.
pub const PROXY_SUB_PUBLIC_KEY: u16 = 6;
/// Proxy sub-message: password enabled flag (1 byte).
pub const PROXY_SUB_PASSWORD_STATUS: u16 = 2;
/// Proxy sub-message: auth request carrying the 10-byte device serial.
pub const PROXY_SUB_AUTH_REQUEST: u16 = 0;
/// Proxy sub-message: host auth response carrying the RSA-OAEP ciphertext.
pub const PROXY_SUB_AUTH_RESPONSE: u16 = 1;

/// One decoded item of a device frame.
///
/// Some variants carry diagnostic details (id/lengths) that the control client
/// only counts; they are kept for tests and for future diagnostics.
#[derive(Debug)]
#[allow(dead_code)]
pub enum Incoming {
    /// Datastore value update for a known property.
    Datastore {
        /// Property table entry.
        prop: &'static Prop,
        /// Datastore index.
        index: u16,
        /// Decoded value.
        value: Value,
    },
    /// Proxy/auth message.
    Proxy {
        /// Proxy sub-message id.
        sub_id: u16,
        /// Raw payload.
        payload: Vec<u8>,
    },
    /// Opaque proxy status byte.
    ProxyStatus {
        /// Status byte, when present.
        status: Option<u8>,
    },
    /// Meter payload (control path ignores it).
    Meters {
        /// Payload length of the meter message.
        len: usize,
    },
    /// Unknown property id; the rest of the frame is opaque.
    Unknown {
        /// Property id.
        id: u16,
        /// Datastore index.
        index: u16,
        /// Remaining payload length.
        len: usize,
    },
    /// Known property with a too-short value payload.
    Malformed {
        /// Property table entry.
        prop: &'static Prop,
        /// Datastore index.
        index: u16,
        /// Received payload length.
        len: usize,
    },
    /// Trailing bytes too short for a message header.
    Incomplete {
        /// Number of unconsumed bytes.
        len: usize,
    },
}

/// Build one host -> device datastore write.
///
/// The result is exactly one datastore message; the caller must put it in one
/// WebSocket frame and must not concatenate writes (device CDC link is fragile).
pub fn build_write(prop: &'static Prop, index: u16, value: &Value) -> Result<Vec<u8>, CueMixError> {
    let encoded = value.encode(prop.ty)?;
    if encoded.len() > u16::MAX as usize {
        return Err(CueMixError::Unsupported {
            what: "datastore value larger than u16 length",
        });
    }
    let mut out = Vec::with_capacity(6 + encoded.len());
    out.extend_from_slice(&prop.id.to_be_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
    out.extend_from_slice(&encoded);
    Ok(out)
}

/// Build one host -> device proxy message (0xFFFE sub_id len payload).
pub fn build_proxy(sub_id: u16, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + payload.len());
    out.extend_from_slice(&PROXY_ID.to_be_bytes());
    out.extend_from_slice(&sub_id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn be_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

/// Parse a device frame into decoded items. Never fails: malformed parts are
/// reported as [Incoming::Malformed] / [Incoming::Incomplete] so the caller can
/// keep the stream stable and count them.
pub fn parse_frame(payload: &[u8]) -> Vec<Incoming> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= payload.len() {
        let id = be_u16(payload, offset);
        let index = be_u16(payload, offset + 2);
        if id == PROXY_ID {
            if offset + 6 > payload.len() {
                out.push(Incoming::Incomplete {
                    len: payload.len() - offset,
                });
                break;
            }
            let sub_id = be_u16(payload, offset + 2);
            let length = be_u16(payload, offset + 4) as usize;
            let start = offset + 6;
            if start + length > payload.len() {
                out.push(Incoming::Malformed {
                    prop: &PROXY_PLACEHOLDER,
                    index,
                    len: payload.len() - start,
                });
                break;
            }
            out.push(Incoming::Proxy {
                sub_id,
                payload: payload[start..start + length].to_vec(),
            });
            offset = start + length;
            continue;
        }
        if id == PROXY_STATUS_ID {
            out.push(Incoming::ProxyStatus {
                status: payload.get(offset + 4).copied(),
            });
            break;
        }
        if id == METERS_ID {
            out.push(Incoming::Meters {
                len: payload.len() - offset,
            });
            break;
        }
        match props::by_id(id) {
            None => {
                out.push(Incoming::Unknown {
                    id,
                    index,
                    len: payload.len() - offset - 4,
                });
                break;
            }
            Some(prop) => {
                let data = &payload[offset + 4..];
                match prop.ty.fixed_len() {
                    Some(fixed) => {
                        if data.len() < fixed {
                            out.push(Incoming::Malformed {
                                prop,
                                index,
                                len: data.len(),
                            });
                            break;
                        }
                        match Value::decode(prop.ty, &data[..fixed]) {
                            Some(value) => out.push(Incoming::Datastore { prop, index, value }),
                            None => out.push(Incoming::Malformed {
                                prop,
                                index,
                                len: data.len(),
                            }),
                        }
                        offset += 4 + fixed;
                    }
                    None => {
                        let value = Value::decode(prop.ty, data)
                            .unwrap_or_else(|| Value::Bytes(Vec::new()));
                        out.push(Incoming::Datastore { prop, index, value });
                        offset = payload.len();
                        break;
                    }
                }
            }
        }
    }
    if offset < payload.len() && out.is_empty() {
        out.push(Incoming::Incomplete {
            len: payload.len() - offset,
        });
    }
    out
}

// Placeholder used only to describe a malformed proxy payload in the Malformed
// variant; it is not a datastore property and never reaches the mirror.
static PROXY_PLACEHOLDER: Prop = Prop {
    key: "proxy",
    id: PROXY_ID,
    ty: crate::props::PropType::Bytes,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::props::{by_key, keys, MAIN_FADER_LEFT_INDEX, MAIN_FADER_RIGHT_INDEX};

    fn datastore_of(items: &[Incoming]) -> Vec<(String, u16, Value)> {
        items
            .iter()
            .filter_map(|item| match item {
                Incoming::Datastore { prop, index, value } => {
                    Some((prop.key.to_string(), *index, value.clone()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn builds_single_key_write_with_length_prefix() {
        let prop = by_key(keys::MUTE_ENABLE).expect("mute property");
        let bytes = build_write(prop, 0, &Value::Byte(1)).expect("build");
        assert_eq!(bytes, vec![0x13, 0x9b, 0x00, 0x00, 0x00, 0x01, 0x01]);
    }

    #[test]
    fn builds_fader_write_for_both_main_channels() {
        let prop = by_key(keys::KO_BUS_FADER).expect("fader property");
        let left = build_write(prop, MAIN_FADER_LEFT_INDEX, &Value::Fader(0.5)).expect("left");
        assert_eq!(
            left,
            vec![0x04, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00, 0x80, 0x00, 0x00]
        );
        let right = build_write(prop, MAIN_FADER_RIGHT_INDEX, &Value::Fader(1.0)).expect("right");
        assert_eq!(
            right,
            vec![0x04, 0x03, 0x00, 0x01, 0x00, 0x04, 0x01, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn build_write_rejects_wrong_value_type() {
        let prop = by_key(keys::MUTE_ENABLE).expect("mute property");
        assert!(matches!(
            build_write(prop, 0, &Value::Fader(0.5)),
            Err(CueMixError::Unsupported { .. })
        ));
    }

    #[test]
    fn parses_device_sample_rate_frame() {
        let mut frame = vec![0x00, 0x0a, 0x00, 0x00];
        frame.extend_from_slice(&48000i32.to_be_bytes());
        let items = parse_frame(&frame);
        assert_eq!(items.len(), 1, "one datastore item, got {:?}", items);
        assert_eq!(
            datastore_of(&items),
            vec![(keys::SAMPLE_RATE.to_string(), 0, Value::Int(48000))]
        );
    }

    #[test]
    fn parses_mute_and_string_values() {
        let frame = vec![0x13, 0x9b, 0x00, 0x00, 0x01];
        assert_eq!(
            datastore_of(&parse_frame(&frame)),
            vec![(keys::MUTE_ENABLE.to_string(), 0, Value::Byte(1))]
        );

        let mut dev_name = vec![0x00, 0x03, 0x00, 0x00];
        let mut text = [0u8; 32];
        text[..13].copy_from_slice(b"UltraLite-mk5");
        dev_name.extend_from_slice(&text);
        assert_eq!(
            datastore_of(&parse_frame(&dev_name)),
            vec![(
                keys::DEV_NAME.to_string(),
                0,
                Value::Str("UltraLite-mk5".to_string())
            )]
        );
    }

    #[test]
    fn parses_several_fixed_size_messages_in_one_frame() {
        let mut frame = vec![0x00, 0x0a, 0x00, 0x00];
        frame.extend_from_slice(&44100i32.to_be_bytes());
        frame.extend_from_slice(&[0x13, 0x9b, 0x00, 0x00, 0x00]);
        let items = parse_frame(&frame);
        assert_eq!(datastore_of(&items).len(), 2);
    }

    #[test]
    fn parses_bytes_property_as_rest_of_frame() {
        let frame = vec![0x00, 0x02, 0x00, 0x00, 1, 2, 242, 254];
        assert_eq!(
            datastore_of(&parse_frame(&frame)),
            vec![(
                keys::DEV_UID.to_string(),
                0,
                Value::Bytes(vec![1, 2, 242, 254])
            )]
        );
    }

    #[test]
    fn parses_proxy_public_key_status_and_auth_request() {
        let pem = b"-----BEGIN PUBLIC KEY-----";
        let mut frame = PROXY_ID.to_be_bytes().to_vec();
        frame.extend_from_slice(&PROXY_SUB_PUBLIC_KEY.to_be_bytes());
        frame.extend_from_slice(&(pem.len() as u16).to_be_bytes());
        frame.extend_from_slice(pem);
        match &parse_frame(&frame)[0] {
            Incoming::Proxy { sub_id, payload } => {
                assert_eq!(*sub_id, PROXY_SUB_PUBLIC_KEY);
                assert_eq!(payload.as_slice(), pem);
            }
            other => panic!("expected proxy, got {:?}", other),
        }

        let status = [0xff, 0xff, 0x00, 0x00, 0x03];
        match &parse_frame(&status)[0] {
            Incoming::ProxyStatus { status } => assert_eq!(*status, Some(3)),
            other => panic!("expected proxy status, got {:?}", other),
        }

        let mut auth = PROXY_ID.to_be_bytes().to_vec();
        auth.extend_from_slice(&PROXY_SUB_AUTH_REQUEST.to_be_bytes());
        auth.extend_from_slice(&10u16.to_be_bytes());
        auth.extend_from_slice(b"UL5LFE7E6E");
        match &parse_frame(&auth)[0] {
            Incoming::Proxy { sub_id, payload } => {
                assert_eq!(*sub_id, PROXY_SUB_AUTH_REQUEST);
                assert_eq!(payload, b"UL5LFE7E6E");
            }
            other => panic!("expected auth request, got {:?}", other),
        }
    }

    #[test]
    fn ignores_meters_and_unknown_ids() {
        let mut meters = METERS_ID.to_be_bytes().to_vec();
        meters.extend_from_slice(&[0u8; 64]);
        match &parse_frame(&meters)[0] {
            Incoming::Meters { len } => assert_eq!(*len, 66),
            other => panic!("expected meters, got {:?}", other),
        }
        let unknown = vec![0x27, 0x0f, 0x00, 0x01, 0xaa];
        match &parse_frame(&unknown)[0] {
            Incoming::Unknown { id, index, len } => {
                assert_eq!(*id, 9999);
                assert_eq!(*index, 1);
                assert_eq!(*len, 1);
            }
            other => panic!("expected unknown, got {:?}", other),
        }
    }

    #[test]
    fn reports_malformed_and_incomplete_payloads() {
        let truncated = vec![0x00, 0x0a, 0x00, 0x00, 0x00, 0x00];
        match &parse_frame(&truncated)[0] {
            Incoming::Malformed { prop, .. } => assert_eq!(prop.key, keys::SAMPLE_RATE),
            other => panic!("expected malformed, got {:?}", other),
        }
        match &parse_frame(&[0x01, 0x02])[0] {
            Incoming::Incomplete { len } => assert_eq!(*len, 2),
            other => panic!("expected incomplete, got {:?}", other),
        }
        assert!(parse_frame(&[]).is_empty());
    }

    #[test]
    fn builds_proxy_auth_response() {
        let cipher = vec![0xaa; 256];
        let bytes = build_proxy(PROXY_SUB_AUTH_RESPONSE, &cipher);
        assert_eq!(&bytes[..2], &PROXY_ID.to_be_bytes());
        assert_eq!(&bytes[2..4], &PROXY_SUB_AUTH_RESPONSE.to_be_bytes());
        assert_eq!(&bytes[4..6], &256u16.to_be_bytes());
        assert_eq!(&bytes[6..], cipher.as_slice());
    }
}
