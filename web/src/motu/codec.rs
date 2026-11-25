//! CueMix 5 datastore 编解码。
//!
//! 线格式以 `tools/motu/cuemix_client.py` 为准：
//! - 设备→主机：`u16 id BE + u16 index BE + value`，一个 WebSocket 二进制帧一条消息
//!   （value 是帧里剩下的字节；定长类型只消费自己的宽度，多出来的字节不另拆消息）
//! - 主机→设备：`u16 id + u16 index + u16 len + value`，多条可拼进一帧，单帧 ≤ 1400 字节
//! - 类型：B=u8，I=i32 BE，I16=u16 BE，F=i32 BE / 2^24，S=32 字节 UTF-8 零填充，A=原始字节（只读）

use std::collections::BTreeMap;

use serde_json::{json, Value};

pub const FRAME_LIMIT: usize = 1400;
pub const F_SCALE: f64 = 16_777_216.0;
pub const FADER_EPS: f64 = 1e-4;
/// 写入上限。字节 n 表示 -n dB。
/// 依据：CueMix 自带前端源码 `kMainVolCtrlInfo = {k:'kMainTrim', u:"dB", min:100, max:0}`
/// （路径 `scratch/lead/cuemix-proto/src/iosetup.js`），即 0..=100 dB 衰减。
pub const MAIN_TRIM_MAX: u8 = 100;
pub const NUM_IN: u16 = 32;
pub const NUM_BUS: u16 = 14;
pub const PROXY_ID: u16 = 0xFFFE;
pub const PROXY_STATUS_ID: u16 = 0xFFFF;
pub const METERS_ID: u16 = 6000;
pub const PASSWORD_STATUS: u16 = 2;

/// 物理输入 0..17 与混响返回 30/31。final insert 只动这些源，共 14×20=280 格。
pub const DIRECT_SOURCES: [u16; 20] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 30, 31,
];
pub const FINAL_INSERT_CELLS: usize = 280;

pub const MIXER_KEYS: &[&str] = &[
    "kiMixFader",
    "kiMixPan",
    "kiMixSolo",
    "kiMixMute",
    "kiMixStereo",
    "koBusFader",
    "koBusMute",
    "kHostOutMixMode",
    "kFPGAPatch",
    "kLoopbackSource",
    "kLoopbackFirst",
    "kNetControlEnable",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
    B,
    I,
    I16,
    F,
    S,
    A,
}

impl PropType {
    pub fn writable(self) -> bool {
        !matches!(self, Self::A)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PropDef {
    pub key: &'static str,
    pub id: u16,
    pub ty: PropType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    B(u8),
    I(i32),
    I16(u16),
    F(f64),
    S(String),
    A(Vec<u8>),
}

impl PropValue {
    pub fn ty(&self) -> PropType {
        match self {
            Self::B(_) => PropType::B,
            Self::I(_) => PropType::I,
            Self::I16(_) => PropType::I16,
            Self::F(_) => PropType::F,
            Self::S(_) => PropType::S,
            Self::A(_) => PropType::A,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::B(v) => v.to_string(),
            Self::I(v) => v.to_string(),
            Self::I16(v) => v.to_string(),
            Self::F(v) => format!("{v:.6}"),
            Self::S(v) => v.clone(),
            Self::A(v) => format!("bytes:{}", v.len()),
        }
    }
}

const PROPS: &[PropDef] = &[
    PropDef {
        key: "kDeviceMAC",
        id: 0,
        ty: PropType::A,
    },
    PropDef {
        key: "kDevUID",
        id: 2,
        ty: PropType::A,
    },
    PropDef {
        key: "kDevName",
        id: 3,
        ty: PropType::S,
    },
    PropDef {
        key: "kDevVersion",
        id: 4,
        ty: PropType::S,
    },
    PropDef {
        key: "kiChannelNames",
        id: 6,
        ty: PropType::S,
    },
    PropDef {
        key: "koChannelNames",
        id: 7,
        ty: PropType::S,
    },
    PropDef {
        key: "kAPIVersion",
        id: 8,
        ty: PropType::I16,
    },
    PropDef {
        key: "kSampleRate",
        id: 10,
        ty: PropType::I,
    },
    PropDef {
        key: "kClockSource",
        id: 11,
        ty: PropType::B,
    },
    PropDef {
        key: "kFPGALocked",
        id: 15,
        ty: PropType::B,
    },
    PropDef {
        key: "kPLLLocked",
        id: 16,
        ty: PropType::B,
    },
    PropDef {
        key: "kMIDIThru",
        id: 19,
        ty: PropType::B,
    },
    PropDef {
        key: "kLoopbackFirst",
        id: 20,
        ty: PropType::B,
    },
    PropDef {
        key: "kModelID",
        id: 21,
        ty: PropType::S,
    },
    PropDef {
        key: "kNetControlEnable",
        id: 26,
        ty: PropType::B,
    },
    PropDef {
        key: "kiMixStereo",
        id: 1000,
        ty: PropType::B,
    },
    PropDef {
        key: "koMixStereo",
        id: 1001,
        ty: PropType::B,
    },
    PropDef {
        key: "kiEQMode",
        id: 1002,
        ty: PropType::B,
    },
    PropDef {
        key: "kiEQBypass",
        id: 1003,
        ty: PropType::B,
    },
    PropDef {
        key: "kiEQFreq",
        id: 1004,
        ty: PropType::I,
    },
    PropDef {
        key: "kiEQGain",
        id: 1005,
        ty: PropType::F,
    },
    PropDef {
        key: "kiEQBW",
        id: 1006,
        ty: PropType::F,
    },
    PropDef {
        key: "kiGateBypass",
        id: 1007,
        ty: PropType::B,
    },
    PropDef {
        key: "kiGateThreshold",
        id: 1008,
        ty: PropType::F,
    },
    PropDef {
        key: "kiGateAttack",
        id: 1009,
        ty: PropType::I16,
    },
    PropDef {
        key: "kiGateRelease",
        id: 1010,
        ty: PropType::I16,
    },
    PropDef {
        key: "kiCompBypass",
        id: 1011,
        ty: PropType::B,
    },
    PropDef {
        key: "kiCompThreshold",
        id: 1012,
        ty: PropType::F,
    },
    PropDef {
        key: "kiCompAttack",
        id: 1013,
        ty: PropType::I16,
    },
    PropDef {
        key: "kiCompRelease",
        id: 1014,
        ty: PropType::I16,
    },
    PropDef {
        key: "kiCompRatio",
        id: 1015,
        ty: PropType::F,
    },
    PropDef {
        key: "kiMixFader",
        id: 1016,
        ty: PropType::F,
    },
    PropDef {
        key: "kiMixPan",
        id: 1017,
        ty: PropType::F,
    },
    PropDef {
        key: "kiMixSolo",
        id: 1018,
        ty: PropType::B,
    },
    PropDef {
        key: "kiMixMute",
        id: 1019,
        ty: PropType::B,
    },
    PropDef {
        key: "kHostOutMixMode",
        id: 1020,
        ty: PropType::B,
    },
    PropDef {
        key: "kiCompMakeup",
        id: 1021,
        ty: PropType::F,
    },
    PropDef {
        key: "koEQMode",
        id: 1022,
        ty: PropType::B,
    },
    PropDef {
        key: "koEQBypass",
        id: 1023,
        ty: PropType::B,
    },
    PropDef {
        key: "koEQFreq",
        id: 1024,
        ty: PropType::I,
    },
    PropDef {
        key: "koEQGain",
        id: 1025,
        ty: PropType::F,
    },
    PropDef {
        key: "koEQBW",
        id: 1026,
        ty: PropType::F,
    },
    PropDef {
        key: "koBusFader",
        id: 1027,
        ty: PropType::F,
    },
    PropDef {
        key: "koBusMute",
        id: 1028,
        ty: PropType::B,
    },
    PropDef {
        key: "kReverbDecay",
        id: 1030,
        ty: PropType::B,
    },
    PropDef {
        key: "kReverbDamping",
        id: 1031,
        ty: PropType::B,
    },
    PropDef {
        key: "kReverbPreDelay",
        id: 1032,
        ty: PropType::B,
    },
    PropDef {
        key: "kReverbWidth",
        id: 1033,
        ty: PropType::B,
    },
    PropDef {
        key: "kReverbPreset",
        id: 1034,
        ty: PropType::B,
    },
    PropDef {
        key: "koTrim",
        id: 5000,
        ty: PropType::B,
    },
    PropDef {
        key: "kiGain",
        id: 5001,
        ty: PropType::B,
    },
    PropDef {
        key: "kiPhase",
        id: 5002,
        ty: PropType::B,
    },
    PropDef {
        key: "kPad",
        id: 5003,
        ty: PropType::B,
    },
    PropDef {
        key: "k48V",
        id: 5004,
        ty: PropType::B,
    },
    PropDef {
        key: "kJackDetect",
        id: 5005,
        ty: PropType::B,
    },
    PropDef {
        key: "kOpticalMode",
        id: 5006,
        ty: PropType::B,
    },
    PropDef {
        key: "kFPGAPatch",
        id: 5010,
        ty: PropType::B,
    },
    PropDef {
        key: "kMainTrim",
        id: 5011,
        ty: PropType::B,
    },
    PropDef {
        key: "kMainGroup",
        id: 5012,
        ty: PropType::I16,
    },
    PropDef {
        key: "kLoopbackSource",
        id: 5014,
        ty: PropType::B,
    },
    PropDef {
        key: "kABEnable",
        id: 5015,
        ty: PropType::B,
    },
    PropDef {
        key: "kAEnable",
        id: 5016,
        ty: PropType::B,
    },
    PropDef {
        key: "kBEnable",
        id: 5017,
        ty: PropType::B,
    },
    PropDef {
        key: "kMonoEnable",
        id: 5018,
        ty: PropType::B,
    },
    PropDef {
        key: "kMuteEnable",
        id: 5019,
        ty: PropType::B,
    },
    PropDef {
        key: "kOpticalExpanderPatch",
        id: 5020,
        ty: PropType::B,
    },
    PropDef {
        key: "kWordClockOut",
        id: 5024,
        ty: PropType::B,
    },
    PropDef {
        key: "kOpticalExpander",
        id: 5025,
        ty: PropType::B,
    },
    PropDef {
        key: "kTalkbackLatch",
        id: 5026,
        ty: PropType::B,
    },
    PropDef {
        key: "kTalkbackEnable",
        id: 5027,
        ty: PropType::B,
    },
    PropDef {
        key: "kTalkbackLevel",
        id: 5028,
        ty: PropType::F,
    },
    PropDef {
        key: "kTalkbackDim",
        id: 5029,
        ty: PropType::F,
    },
    PropDef {
        key: "kTalkbackSource",
        id: 5030,
        ty: PropType::B,
    },
    PropDef {
        key: "kTalkbackDestination",
        id: 5031,
        ty: PropType::I,
    },
    PropDef {
        key: "kFootSwitchEnabled",
        id: 5032,
        ty: PropType::B,
    },
    PropDef {
        key: "kFootSwitchDownKey",
        id: 5033,
        ty: PropType::S,
    },
    PropDef {
        key: "kFootSwitchUpKey",
        id: 5034,
        ty: PropType::S,
    },
];

pub fn prop_by_key(key: &str) -> Option<&'static PropDef> {
    PROPS.iter().find(|p| p.key == key)
}

pub fn prop_by_id(id: u16) -> Option<&'static PropDef> {
    PROPS.iter().find(|p| p.id == id)
}

pub fn prop_eq(a: &PropValue, b: &PropValue) -> bool {
    match (a, b) {
        (PropValue::F(x), PropValue::F(y)) => (x - y).abs() <= FADER_EPS,
        (PropValue::B(x), PropValue::B(y)) => x == y,
        (PropValue::I(x), PropValue::I(y)) => x == y,
        (PropValue::I16(x), PropValue::I16(y)) => x == y,
        (PropValue::S(x), PropValue::S(y)) => x == y,
        (PropValue::A(x), PropValue::A(y)) => x == y,
        _ => false,
    }
}

pub fn is_neutral(value: &PropValue) -> bool {
    match value {
        PropValue::F(v) => *v == 0.0,
        PropValue::B(0) | PropValue::I(0) | PropValue::I16(0) => true,
        PropValue::S(s) => s.is_empty(),
        _ => false,
    }
}

pub fn effective_eq(current: Option<&PropValue>, desired: &PropValue) -> bool {
    match current {
        Some(cur) => prop_eq(cur, desired),
        None => is_neutral(desired),
    }
}

pub fn trim_to_db(trim: u8) -> f64 {
    -(trim as f64)
}

pub fn encode_value(value: &PropValue) -> Result<Vec<u8>, String> {
    match value {
        PropValue::B(v) => Ok(vec![*v]),
        PropValue::I(v) => Ok(v.to_be_bytes().to_vec()),
        PropValue::I16(v) => Ok(v.to_be_bytes().to_vec()),
        PropValue::F(v) => {
            if !v.is_finite() {
                return Err("F 不是有限数".into());
            }
            let scaled = (*v * F_SCALE).round();
            if !(i32::MIN as f64..=i32::MAX as f64).contains(&scaled) {
                return Err("F 超出定点范围".into());
            }
            Ok((scaled as i32).to_be_bytes().to_vec())
        }
        PropValue::S(text) => {
            let mut raw = text.as_bytes().to_vec();
            if raw.len() > 31 {
                raw.truncate(31);
                while raw.last().copied().is_some_and(|b| b & 0xC0 == 0x80) {
                    raw.pop();
                }
            }
            raw.resize(32, 0);
            Ok(raw)
        }
        PropValue::A(_) => Err("type A is not writable".into()),
    }
}

pub fn decode_value(ty: PropType, data: &[u8]) -> Option<PropValue> {
    match ty {
        PropType::B => data.first().copied().map(PropValue::B),
        PropType::I => {
            if data.len() < 4 {
                return None;
            }
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&data[..4]);
            Some(PropValue::I(i32::from_be_bytes(buf)))
        }
        PropType::I16 => {
            if data.len() < 2 {
                return None;
            }
            Some(PropValue::I16(u16::from_be_bytes([data[0], data[1]])))
        }
        PropType::F => {
            if data.len() < 4 {
                return None;
            }
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&data[..4]);
            Some(PropValue::F(i32::from_be_bytes(buf) as f64 / F_SCALE))
        }
        PropType::S => {
            let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
            Some(PropValue::S(
                String::from_utf8_lossy(&data[..end]).into_owned(),
            ))
        }
        PropType::A => Some(PropValue::A(data.to_vec())),
    }
}

pub fn encode_host_message(id: u16, index: u16, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + value.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out
}

pub fn encode_property(key: &str, index: u16, value: &PropValue) -> Result<Vec<u8>, String> {
    let def = prop_by_key(key).ok_or_else(|| format!("未知属性 {key}"))?;
    if def.ty != value.ty() {
        return Err(format!("{key} 类型不符"));
    }
    if !def.ty.writable() {
        return Err(format!("{key} 不可写"));
    }
    let body = encode_value(value)?;
    Ok(encode_host_message(def.id, index, &body))
}

/// 主机报文解码。设备读路径按「一帧一条」处理；这个函数给分帧往返测试和以后的读回核对用。
#[allow(dead_code)]
pub fn decode_host_stream(mut buf: &[u8]) -> Result<Vec<(u16, u16, Vec<u8>)>, String> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf.len() < 6 {
            return Err("主机报文头被截断".into());
        }
        let id = u16::from_be_bytes([buf[0], buf[1]]);
        let index = u16::from_be_bytes([buf[2], buf[3]]);
        let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
        if buf.len() < 6 + len {
            return Err("主机报文值被截断".into());
        }
        out.push((id, index, buf[6..6 + len].to_vec()));
        buf = &buf[6 + len..];
    }
    Ok(out)
}

/// 与 client.py `chunk_messages` 相同：已有内容放不下就先交出当前帧；单条超限仍单独成帧。
pub fn chunk_messages(msgs: &[Vec<u8>], limit: usize) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut frame: Vec<u8> = Vec::new();
    for msg in msgs {
        if !frame.is_empty() && frame.len() + msg.len() > limit {
            frames.push(std::mem::take(&mut frame));
        }
        frame.extend_from_slice(msg);
    }
    if !frame.is_empty() {
        frames.push(frame);
    }
    frames
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFrame {
    pub password_enabled: Option<bool>,
    pub property: Option<(u16, u16, PropValue)>,
    pub ignored: bool,
}

/// 一个设备帧只解一条消息。口令状态走 proxy id 0xFFFE / sub 2，不尝试 RSA。
pub fn decode_device_frame(frame: &[u8]) -> DecodedFrame {
    if frame.len() < 4 {
        return DecodedFrame {
            password_enabled: None,
            property: None,
            ignored: true,
        };
    }
    let id = u16::from_be_bytes([frame[0], frame[1]]);
    let index = u16::from_be_bytes([frame[2], frame[3]]);
    if id == PROXY_ID {
        if frame.len() < 6 {
            return DecodedFrame {
                password_enabled: None,
                property: None,
                ignored: true,
            };
        }
        let len = u16::from_be_bytes([frame[4], frame[5]]) as usize;
        if frame.len() < 6 + len {
            return DecodedFrame {
                password_enabled: None,
                property: None,
                ignored: true,
            };
        }
        if index == PASSWORD_STATUS {
            let on = frame[6] != 0;
            return DecodedFrame {
                password_enabled: Some(on),
                property: None,
                ignored: false,
            };
        }
        return DecodedFrame {
            password_enabled: None,
            property: None,
            ignored: true,
        };
    }
    if id == PROXY_STATUS_ID || id == METERS_ID {
        return DecodedFrame {
            password_enabled: None,
            property: None,
            ignored: true,
        };
    }
    let Some(def) = prop_by_id(id) else {
        return DecodedFrame {
            password_enabled: None,
            property: None,
            ignored: true,
        };
    };
    match decode_value(def.ty, &frame[4..]) {
        Some(value) => DecodedFrame {
            password_enabled: None,
            property: Some((id, index, value)),
            ignored: false,
        },
        None => DecodedFrame {
            password_enabled: None,
            property: None,
            ignored: true,
        },
    }
}

#[derive(Clone, Debug, Default)]
pub struct Datastore {
    pub values: BTreeMap<String, Vec<Option<PropValue>>>,
    pub message_count: u32,
    pub unknown_ids: u32,
}

impl Datastore {
    pub fn get(&self, key: &str, index: usize) -> Option<PropValue> {
        self.values.get(key)?.get(index)?.clone()
    }

    pub fn set(&mut self, key: &str, index: usize, value: PropValue) {
        let arr = self.values.entry(key.to_string()).or_default();
        if arr.len() <= index {
            arr.resize(index + 1, None);
        }
        arr[index] = Some(value);
    }

    pub fn ingest_frame(&mut self, frame: &[u8]) -> DecodedFrame {
        let decoded = decode_device_frame(frame);
        if let Some((id, index, value)) = &decoded.property {
            if let Some(def) = prop_by_id(*id) {
                self.set(def.key, *index as usize, value.clone());
                self.message_count = self.message_count.saturating_add(1);
            }
        } else if decoded.ignored && frame.len() >= 2 {
            let id = u16::from_be_bytes([frame[0], frame[1]]);
            if id != PROXY_ID
                && id != PROXY_STATUS_ID
                && id != METERS_ID
                && prop_by_id(id).is_none()
            {
                self.unknown_ids = self.unknown_ids.saturating_add(1);
            }
        }
        decoded
    }

    pub fn fader(&self, index: u16) -> f64 {
        match self.get("kiMixFader", index as usize) {
            Some(PropValue::F(v)) => v,
            _ => 0.0,
        }
    }

    pub fn muted(&self, index: u16) -> bool {
        match self.get("kiMixMute", index as usize) {
            Some(PropValue::B(v)) => v != 0,
            _ => false,
        }
    }

    pub fn string_at(&self, key: &str) -> Option<String> {
        match self.get(key, 0) {
            Some(PropValue::S(s)) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteOp {
    pub key: String,
    pub index: u16,
    pub value: PropValue,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Mismatch {
    pub key: String,
    pub index: u16,
    pub expected: String,
    pub actual: String,
}

pub fn diff_against(current: &Datastore, desired: &[WriteOp]) -> Vec<WriteOp> {
    desired
        .iter()
        .filter(|op| {
            let cur = current.get(&op.key, op.index as usize);
            !effective_eq(cur.as_ref(), &op.value)
        })
        .cloned()
        .collect()
}

pub fn apply_ops(store: &mut Datastore, ops: &[WriteOp]) {
    for op in ops {
        store.set(&op.key, op.index as usize, op.value.clone());
    }
}

pub fn mismatches(current: &Datastore, ops: &[WriteOp]) -> Vec<Mismatch> {
    let mut out = Vec::new();
    for op in ops {
        let actual = current.get(&op.key, op.index as usize);
        let ok = match &actual {
            Some(value) => prop_eq(value, &op.value),
            None => false,
        };
        if !ok {
            out.push(Mismatch {
                key: op.key.clone(),
                index: op.index,
                expected: op.value.display(),
                actual: actual
                    .map(|v| v.display())
                    .unwrap_or_else(|| "missing".into()),
            });
        }
        if out.len() >= 20 {
            break;
        }
    }
    out
}

pub fn rollback_ops(before: &Datastore, applied: &[WriteOp]) -> Vec<WriteOp> {
    let mut out = Vec::new();
    for op in applied {
        if let Some(prev) = before.get(&op.key, op.index as usize) {
            out.push(WriteOp {
                key: op.key.clone(),
                index: op.index,
                value: prev,
            });
        } else if !is_neutral(&op.value) {
            let neutral = match &op.value {
                PropValue::F(_) => Some(PropValue::F(0.0)),
                PropValue::B(_) => Some(PropValue::B(0)),
                PropValue::I(_) => Some(PropValue::I(0)),
                PropValue::I16(_) => Some(PropValue::I16(0)),
                _ => None,
            };
            if let Some(value) = neutral {
                out.push(WriteOp {
                    key: op.key.clone(),
                    index: op.index,
                    value,
                });
            }
        }
    }
    out
}

pub fn encode_ops(ops: &[WriteOp]) -> Result<Vec<Vec<u8>>, String> {
    let mut msgs = Vec::with_capacity(ops.len());
    for op in ops {
        msgs.push(encode_property(&op.key, op.index, &op.value)?);
    }
    Ok(chunk_messages(&msgs, FRAME_LIMIT))
}

pub fn mix_index(bus: u16, input: u16) -> u16 {
    bus * NUM_IN + input
}

pub fn is_host_input(input: u16) -> bool {
    (18..30).contains(&input)
}

pub fn is_direct_input(input: u16) -> bool {
    input < 18 || input == 30 || input == 31
}

pub fn is_protected_fader_op(op: &WriteOp) -> bool {
    op.key == "kiMixFader" && is_host_input(op.index % NUM_IN)
}

pub fn direct_cells() -> impl Iterator<Item = u16> {
    (0..NUM_BUS).flat_map(|bus| {
        DIRECT_SOURCES
            .into_iter()
            .map(move |input| mix_index(bus, input))
    })
}

pub fn host_faders_changed(before: &Datastore, after: &Datastore) -> bool {
    for bus in 0..NUM_BUS {
        for input in 18..30 {
            let index = mix_index(bus, input);
            if (before.fader(index) - after.fader(index)).abs() > FADER_EPS {
                return true;
            }
        }
    }
    false
}

pub fn plan_zero_direct(current: &Datastore) -> Vec<WriteOp> {
    let desired: Vec<WriteOp> = direct_cells()
        .map(|index| WriteOp {
            key: "kiMixFader".into(),
            index,
            value: PropValue::F(0.0),
        })
        .collect();
    debug_assert_eq!(desired.len(), FINAL_INSERT_CELLS);
    diff_against(current, &desired)
}

pub fn plan_restore_direct(current: &Datastore, snapshot: &Datastore) -> Vec<WriteOp> {
    let desired: Vec<WriteOp> = direct_cells()
        .map(|index| {
            let value = match snapshot.get("kiMixFader", index as usize) {
                Some(PropValue::F(v)) => PropValue::F(v),
                _ => PropValue::F(0.0),
            };
            WriteOp {
                key: "kiMixFader".into(),
                index,
                value,
            }
        })
        .collect();
    diff_against(current, &desired)
}

pub fn restore_desired(snapshot: &Datastore) -> Vec<WriteOp> {
    let mut desired = Vec::new();
    for key in MIXER_KEYS {
        let Some(arr) = snapshot.values.get(*key) else {
            continue;
        };
        for (idx, value) in arr.iter().enumerate() {
            let Some(value) = value else { continue };
            if idx > u16::MAX as usize {
                continue;
            }
            desired.push(WriteOp {
                key: (*key).to_string(),
                index: idx as u16,
                value: value.clone(),
            });
        }
    }
    desired
}

pub fn plan_restore(current: &Datastore, snapshot: &Datastore) -> (usize, Vec<WriteOp>) {
    let desired = restore_desired(snapshot);
    let count = desired.len();
    (count, diff_against(current, &desired))
}

pub fn plan_monitor(current: &Datastore, trim: u8) -> Vec<WriteOp> {
    diff_against(
        current,
        &[WriteOp {
            key: "kMainTrim".into(),
            index: 0,
            value: PropValue::B(trim),
        }],
    )
}

#[derive(Debug, Clone, Copy)]
pub struct BusInfo {
    pub index: u16,
    pub name: &'static str,
    pub jack: &'static str,
}

const BUSES: &[BusInfo] = &[
    BusInfo {
        index: 0,
        name: "Main 1",
        jack: "Main Out 1",
    },
    BusInfo {
        index: 1,
        name: "Main 2",
        jack: "Main Out 2",
    },
    BusInfo {
        index: 2,
        name: "Line 3",
        jack: "Line Out 3",
    },
    BusInfo {
        index: 3,
        name: "Line 4",
        jack: "Line Out 4",
    },
    BusInfo {
        index: 4,
        name: "Line 5",
        jack: "Line Out 5",
    },
    BusInfo {
        index: 5,
        name: "Line 6",
        jack: "Line Out 6",
    },
    BusInfo {
        index: 6,
        name: "Line 7",
        jack: "Line Out 7",
    },
    BusInfo {
        index: 7,
        name: "Line 8",
        jack: "Line Out 8",
    },
    BusInfo {
        index: 8,
        name: "Line 9",
        jack: "Line Out 9",
    },
    BusInfo {
        index: 9,
        name: "Line 10",
        jack: "Line Out 10",
    },
    BusInfo {
        index: 10,
        name: "Phones 1",
        jack: "Phones Out L",
    },
    BusInfo {
        index: 11,
        name: "Phones 2",
        jack: "Phones Out R",
    },
    BusInfo {
        index: 12,
        name: "Reverb 1",
        jack: "reverb send L",
    },
    BusInfo {
        index: 13,
        name: "Reverb 2",
        jack: "reverb send R",
    },
];

const INPUTS: [&str; 32] = [
    "Mic/Line 1",
    "Mic/Line 2",
    "Line 3",
    "Line 4",
    "Line 5",
    "Line 6",
    "Line 7",
    "Line 8",
    "S/PDIF 1",
    "S/PDIF 2",
    "Optical 1",
    "Optical 2",
    "Optical 3",
    "Optical 4",
    "Optical 5",
    "Optical 6",
    "Optical 7",
    "Optical 8",
    "USB Main L",
    "USB Main R",
    "USB Line 3",
    "USB Line 4",
    "USB Line 5",
    "USB Line 6",
    "USB Line 7",
    "USB Line 8",
    "USB Line 9",
    "USB Line 10",
    "USB Phones L",
    "USB Phones R",
    "Reverb Ret L",
    "Reverb Ret R",
];

pub fn buses() -> &'static [BusInfo] {
    BUSES
}

pub fn input_name(index: u16) -> &'static str {
    INPUTS.get(index as usize).copied().unwrap_or("?")
}

pub fn bus_name(index: u16) -> &'static str {
    BUSES.get(index as usize).map(|b| b.name).unwrap_or("?")
}

/// 模拟口 1..8 / 1..10 的别名。Line 7←Line 7 即 in7→out7。
pub fn analog_alias(bus: u16, input: u16) -> Option<String> {
    if input < 8 && bus < 10 {
        Some(format!("in{}→out{}", input + 1, bus + 1))
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct MixPoint {
    pub bus: u16,
    pub input: u16,
    pub index: u16,
    pub fader: f64,
    pub mute: bool,
    pub solo: bool,
    pub pan: Option<f64>,
}

pub fn active_points(store: &Datastore) -> Vec<MixPoint> {
    let mut out = Vec::new();
    for bus in 0..NUM_BUS {
        for input in 0..NUM_IN {
            let index = mix_index(bus, input);
            let fader = store.fader(index);
            let mute = store.muted(index);
            if fader > 0.0 && !mute {
                let solo = matches!(store.get("kiMixSolo", index as usize), Some(PropValue::B(v)) if v != 0);
                let pan = match store.get("kiMixPan", index as usize) {
                    Some(PropValue::F(v)) => Some(v),
                    _ => None,
                };
                out.push(MixPoint {
                    bus,
                    input,
                    index,
                    fader,
                    mute,
                    solo,
                    pan,
                });
            }
        }
    }
    out
}

pub fn prop_to_json(value: &PropValue) -> Value {
    match value {
        PropValue::B(v) => json!(v),
        PropValue::I(v) => json!(v),
        PropValue::I16(v) => json!(v),
        PropValue::F(v) => json!(v),
        PropValue::S(v) => json!(v),
        PropValue::A(v) => json!(v),
    }
}

pub fn json_to_prop(ty: PropType, value: &Value) -> Option<PropValue> {
    match ty {
        PropType::B => value
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .map(PropValue::B),
        PropType::I => value
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .map(PropValue::I),
        PropType::I16 => value
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .map(PropValue::I16),
        PropType::F => value.as_f64().map(PropValue::F),
        PropType::S => value.as_str().map(|s| PropValue::S(s.to_string())),
        PropType::A => value.as_array().and_then(|arr| {
            arr.iter()
                .map(|item| item.as_u64().and_then(|n| u8::try_from(n).ok()))
                .collect::<Option<Vec<_>>>()
                .map(PropValue::A)
        }),
    }
}

pub fn export_values(store: &Datastore) -> Value {
    let mut root = serde_json::Map::new();
    for (key, arr) in &store.values {
        let mut items: Vec<Value> = arr
            .iter()
            .map(|item| match item {
                Some(value) => prop_to_json(value),
                None => Value::Null,
            })
            .collect();
        while items.last().is_some_and(|v| v.is_null()) {
            items.pop();
        }
        if !items.is_empty() {
            root.insert(key.clone(), Value::Array(items));
        }
    }
    Value::Object(root)
}

pub fn import_values(value: &Value) -> Result<Datastore, String> {
    let obj = value.as_object().ok_or("快照 values 不是对象")?;
    let mut store = Datastore::default();
    for (key, arr) in obj {
        let Some(def) = prop_by_key(key) else {
            continue;
        };
        let Some(items) = arr.as_array() else {
            continue;
        };
        for (idx, item) in items.iter().enumerate() {
            if item.is_null() {
                continue;
            }
            let Some(prop) = json_to_prop(def.ty, item) else {
                return Err(format!("快照 {key}[{idx}] 无法按 {:?} 解析", def.ty));
            };
            store.set(key, idx, prop);
        }
    }
    Ok(store)
}

pub fn format_mac(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn format_uid(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let slice = &bytes[..end];
    if !slice.is_empty() && slice.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        String::from_utf8_lossy(slice).into_owned()
    } else {
        format_mac(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(value: PropValue) {
        let encoded = encode_value(&value).unwrap();
        let decoded = decode_value(value.ty(), &encoded).unwrap();
        assert!(prop_eq(&value, &decoded), "{value:?} -> {decoded:?}");
    }

    #[test]
    fn value_codec_roundtrip() {
        roundtrip(PropValue::B(7));
        roundtrip(PropValue::B(0));
        roundtrip(PropValue::B(255));
        roundtrip(PropValue::I(-123_456));
        roundtrip(PropValue::I(48_000));
        roundtrip(PropValue::I16(65535));
        roundtrip(PropValue::I16(10));
        roundtrip(PropValue::F(0.5));
        roundtrip(PropValue::F(-0.5));
        roundtrip(PropValue::F(0.0));
        roundtrip(PropValue::S("Line 7-8".into()));
        let long = "abcdefghijklmnopqrstuvwxyz012345EXTRA";
        let encoded = encode_value(&PropValue::S(long.into())).unwrap();
        assert_eq!(encoded.len(), 32);
        let decoded = decode_value(PropType::S, &encoded).unwrap();
        assert_eq!(
            decoded,
            PropValue::S("abcdefghijklmnopqrstuvwxyz01234".into())
        );
    }

    #[test]
    fn prop_ids_are_unique() {
        let mut ids = std::collections::BTreeSet::new();
        let mut keys = std::collections::BTreeSet::new();
        for prop in PROPS {
            assert!(ids.insert(prop.id), "duplicate id {}", prop.id);
            assert!(keys.insert(prop.key), "duplicate key {}", prop.key);
        }
        assert_eq!(prop_by_key("kMainTrim").unwrap().id, 5011);
        assert_eq!(prop_by_id(1016).unwrap().key, "kiMixFader");
        assert!(!PropType::A.writable());
    }

    #[test]
    fn host_messages_roundtrip_and_chunk() {
        let one = encode_property("kiMixFader", 6, &PropValue::F(0.5)).unwrap();
        let parsed = decode_host_stream(&one).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, 1016);
        assert_eq!(parsed[0].1, 6);
        let value = decode_value(PropType::F, &parsed[0].2).unwrap();
        assert!(prop_eq(&value, &PropValue::F(0.5)));

        let second = encode_property("kMainTrim", 0, &PropValue::B(28)).unwrap();
        let mut joined = one.clone();
        joined.extend_from_slice(&second);
        let both = decode_host_stream(&joined).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[1].0, 5011);

        let single = chunk_messages(&[vec![1, 2, 3]], FRAME_LIMIT);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0], vec![1, 2, 3]);

        let exact: Vec<Vec<u8>> = (0..14).map(|_| vec![0xAB; 100]).collect();
        let frames = chunk_messages(&exact, FRAME_LIMIT);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 1400);

        let mut cross = exact.clone();
        cross.push(vec![0xCD]);
        let frames = chunk_messages(&cross, FRAME_LIMIT);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].len(), 1400);
        assert_eq!(frames[1], vec![0xCD]);

        let huge = chunk_messages(&[vec![0x11; 1500]], FRAME_LIMIT);
        assert_eq!(huge.len(), 1);
        assert_eq!(huge[0].len(), 1500);
    }

    #[test]
    fn device_frame_is_one_message() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&5011u16.to_be_bytes());
        frame.extend_from_slice(&0u16.to_be_bytes());
        frame.push(28);
        frame.extend_from_slice(&[0xFF, 0xFF]);
        let decoded = decode_device_frame(&frame);
        assert_eq!(decoded.property, Some((5011, 0, PropValue::B(28))));
        assert!(decoded.password_enabled.is_none());

        let password = [0xFF, 0xFE, 0x00, 0x02, 0x00, 0x01, 0x01];
        let decoded = decode_device_frame(&password);
        assert_eq!(decoded.password_enabled, Some(true));
        assert!(decoded.property.is_none());

        let meters = [0x17, 0x70, 0x00, 0x00, 0x01];
        assert!(decode_device_frame(&meters).ignored);
        assert_eq!(METERS_ID, 6000);
    }

    #[test]
    fn mix_index_db_and_direct_plan() {
        assert_eq!(mix_index(6, 6), 198);
        assert_eq!(mix_index(7, 7), 231);
        assert_eq!(mix_index(0, 18), 18);
        assert_eq!(analog_alias(6, 6).as_deref(), Some("in7→out7"));
        assert_eq!(analog_alias(7, 7).as_deref(), Some("in8→out8"));
        assert_eq!(trim_to_db(28), -28.0);
        assert_eq!(trim_to_db(0), 0.0);
        assert_eq!(direct_cells().count(), FINAL_INSERT_CELLS);
        assert_eq!(DIRECT_SOURCES.len() * NUM_BUS as usize, FINAL_INSERT_CELLS);

        let mut store = Datastore::default();
        store.set("kiMixFader", mix_index(6, 6) as usize, PropValue::F(0.935));
        store.set("kiMixFader", mix_index(0, 18) as usize, PropValue::F(1.0));
        store.set("kMainTrim", 0, PropValue::B(28));
        let ops = plan_zero_direct(&store);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].index, 198);
        assert!(!ops.iter().any(is_protected_fader_op));
        let before = store.clone();
        apply_ops(&mut store, &ops);
        assert_eq!(store.fader(198), 0.0);
        assert!((store.fader(18) - 1.0).abs() < FADER_EPS);
        assert!(!host_faders_changed(&before, &store));
        let rb = rollback_ops(&before, &ops);
        apply_ops(&mut store, &rb);
        assert!(mismatches(&store, &rb).is_empty());
        assert!((store.fader(198) - 0.935).abs() < 1e-6);
        assert!(plan_monitor(&store, 28).is_empty());
        assert_eq!(plan_monitor(&store, 10).len(), 1);
    }

    #[test]
    fn values_json_roundtrip() {
        let mut store = Datastore::default();
        store.set("kMainTrim", 0, PropValue::B(28));
        store.set("kDevName", 0, PropValue::S("UltraLite-mk5".into()));
        store.set("kDevUID", 0, PropValue::A(b"UL5LFE7E6E".to_vec()));
        store.set("kiMixFader", 198, PropValue::F(0.935));
        let exported = export_values(&store);
        let back = import_values(&exported).unwrap();
        assert_eq!(back.get("kMainTrim", 0), Some(PropValue::B(28)));
        assert!(prop_eq(
            &back.get("kiMixFader", 198).unwrap(),
            &PropValue::F(0.935)
        ));
        assert_eq!(
            format_uid(&match back.get("kDevUID", 0).unwrap() {
                PropValue::A(bytes) => bytes,
                _ => unreachable!(),
            }),
            "UL5LFE7E6E"
        );
    }
}
