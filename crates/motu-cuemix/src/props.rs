//! UltraLite-mk5 datastore property table (CueMix 5 message layer).
//!
//! The table is the reverse-engineered UltraLite-mk5 datastore (device-side
//! reference: cuemix_client.py, PROPS). Each entry maps a datastore name to a
//! wire property id and wire type. The mapping is interface data and is kept
//! here as constants so the daemon does not hard-code magic numbers.

/// Wire type of one datastore property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropType {
    /// 1-byte unsigned integer (B).
    Byte,
    /// big-endian i32 (I).
    Int,
    /// big-endian u16 (I16).
    UInt16,
    /// 8.24 fixed point fader (F): raw i32 / 2^24.
    Fader,
    /// 32-byte NUL-padded UTF-8 string (S).
    Str,
    /// variable-length bytes (A).
    Bytes,
}

impl PropType {
    /// One-letter wire code used by the protocol/reference implementation.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Byte => "B",
            Self::Int => "I",
            Self::UInt16 => "I16",
            Self::Fader => "F",
            Self::Str => "S",
            Self::Bytes => "A",
        }
    }

    /// Fixed value length in bytes, or None for variable-length types.
    pub const fn fixed_len(self) -> Option<usize> {
        match self {
            Self::Byte => Some(1),
            Self::Int => Some(4),
            Self::UInt16 => Some(2),
            Self::Fader => Some(4),
            Self::Str => Some(32),
            Self::Bytes => None,
        }
    }
}

/// One datastore property: name, wire id and wire type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prop {
    /// Datastore key as used by CueMix 5 (e.g. "kMuteEnable").
    pub key: &'static str,
    /// Wire property id.
    pub id: u16,
    /// Wire value type.
    pub ty: PropType,
}

const fn p(key: &'static str, id: u16, ty: PropType) -> Prop {
    Prop { key, id, ty }
}

/// UltraLite-mk5 datastore property table (70 entries).
pub static PROPS: &[Prop] = &[
    p("kDeviceMAC", 0, PropType::Bytes),
    p("kDevUID", 2, PropType::Bytes),
    p("kDevName", 3, PropType::Str),
    p("kDevVersion", 4, PropType::Str),
    p("kiChannelNames", 6, PropType::Str),
    p("koChannelNames", 7, PropType::Str),
    p("kAPIVersion", 8, PropType::UInt16),
    p("kSampleRate", 10, PropType::Int),
    p("kClockSource", 11, PropType::Byte),
    p("kFPGALocked", 15, PropType::Byte),
    p("kPLLLocked", 16, PropType::Byte),
    p("kMIDIThru", 19, PropType::Byte),
    p("kLoopbackFirst", 20, PropType::Byte),
    p("kModelID", 21, PropType::Str),
    p("kNetControlEnable", 26, PropType::Byte),
    p("kiMixStereo", 1000, PropType::Byte),
    p("koMixStereo", 1001, PropType::Byte),
    p("kiEQMode", 1002, PropType::Byte),
    p("kiEQBypass", 1003, PropType::Byte),
    p("kiEQFreq", 1004, PropType::Int),
    p("kiEQGain", 1005, PropType::Fader),
    p("kiEQBW", 1006, PropType::Fader),
    p("kiGateBypass", 1007, PropType::Byte),
    p("kiGateThreshold", 1008, PropType::Fader),
    p("kiGateAttack", 1009, PropType::UInt16),
    p("kiGateRelease", 1010, PropType::UInt16),
    p("kiCompBypass", 1011, PropType::Byte),
    p("kiCompThreshold", 1012, PropType::Fader),
    p("kiCompAttack", 1013, PropType::UInt16),
    p("kiCompRelease", 1014, PropType::UInt16),
    p("kiCompRatio", 1015, PropType::Fader),
    p("kiCompMakeup", 1021, PropType::Fader),
    p("kiMixFader", 1016, PropType::Fader),
    p("kiMixPan", 1017, PropType::Fader),
    p("kiMixSolo", 1018, PropType::Byte),
    p("kiMixMute", 1019, PropType::Byte),
    p("kHostOutMixMode", 1020, PropType::Byte),
    p("koEQMode", 1022, PropType::Byte),
    p("koEQBypass", 1023, PropType::Byte),
    p("koEQFreq", 1024, PropType::Int),
    p("koEQGain", 1025, PropType::Fader),
    p("koEQBW", 1026, PropType::Fader),
    p("koBusFader", 1027, PropType::Fader),
    p("koBusMute", 1028, PropType::Byte),
    p("kReverbDecay", 1030, PropType::Byte),
    p("kReverbDamping", 1031, PropType::Byte),
    p("kReverbPreDelay", 1032, PropType::Byte),
    p("kReverbWidth", 1033, PropType::Byte),
    p("kReverbPreset", 1034, PropType::Byte),
    p("koTrim", 5000, PropType::Byte),
    p("kiGain", 5001, PropType::Byte),
    p("kiPhase", 5002, PropType::Byte),
    p("kPad", 5003, PropType::Byte),
    p("k48V", 5004, PropType::Byte),
    p("kJackDetect", 5005, PropType::Byte),
    p("kOpticalMode", 5006, PropType::Byte),
    p("kFPGAPatch", 5010, PropType::Byte),
    p("kMainTrim", 5011, PropType::Byte),
    p("kMainGroup", 5012, PropType::UInt16),
    p("kLoopbackSource", 5014, PropType::Byte),
    p("kABEnable", 5015, PropType::Byte),
    p("kAEnable", 5016, PropType::Byte),
    p("kBEnable", 5017, PropType::Byte),
    p("kMonoEnable", 5018, PropType::Byte),
    p("kMuteEnable", 5019, PropType::Byte),
    p("kOpticalExpanderPatch", 5020, PropType::Byte),
    p("kWordClockOut", 5024, PropType::Byte),
    p("kOpticalExpander", 5025, PropType::Byte),
    p("kTalkbackLatch", 5026, PropType::Byte),
    p("kTalkbackEnable", 5027, PropType::Byte),
    p("kTalkbackLevel", 5028, PropType::Fader),
    p("kTalkbackDim", 5029, PropType::Fader),
    p("kTalkbackSource", 5030, PropType::Byte),
    p("kTalkbackDestination", 5031, PropType::Int),
    p("kFootSwitchEnabled", 5032, PropType::Byte),
    p("kFootSwitchDownKey", 5033, PropType::Str),
    p("kFootSwitchUpKey", 5034, PropType::Str),
];

/// Look up a property by datastore key.
pub fn by_key(key: &str) -> Option<&'static Prop> {
    PROPS.iter().find(|p| p.key == key)
}

/// Look up a property by wire id.
pub fn by_id(id: u16) -> Option<&'static Prop> {
    PROPS.iter().find(|p| p.id == id)
}

/// Datastore keys used by the public control API.
pub mod keys {
    /// Global mute switch (written by set_mute).
    pub const MUTE_ENABLE: &str = "kMuteEnable";
    /// Device sample rate in Hz.
    pub const SAMPLE_RATE: &str = "kSampleRate";
    /// Per-bus output fader (written by set_volume).
    pub const KO_BUS_FADER: &str = "koBusFader";
    /// Per-bus output mute flag (read by read_state).
    pub const KO_BUS_MUTE: &str = "koBusMute";
    /// Main trim (front panel trim, read by read_state).
    pub const MAIN_TRIM: &str = "kMainTrim";
    /// Model id string (e.g. "UL5L").
    pub const MODEL_ID: &str = "kModelID";
    /// Device name string (e.g. "UltraLite-mk5").
    pub const DEV_NAME: &str = "kDevName";
    /// Firmware version string.
    pub const DEV_VERSION: &str = "kDevVersion";
    /// Control API version (u16).
    pub const API_VERSION: &str = "kAPIVersion";
    /// Device UID bytes.
    pub const DEV_UID: &str = "kDevUID";
    /// Device MAC bytes.
    pub const DEVICE_MAC: &str = "kDeviceMAC";
    /// Clock source selector.
    pub const CLOCK_SOURCE: &str = "kClockSource";
    /// Network control enable flag.
    pub const NET_CONTROL_ENABLE: &str = "kNetControlEnable";
}

/// Wire id of [keys::MUTE_ENABLE] (5019).
pub const ID_MUTE_ENABLE: u16 = 5019;
/// Wire id of [keys::SAMPLE_RATE] (10).
pub const ID_SAMPLE_RATE: u16 = 10;
/// Wire id of [keys::KO_BUS_FADER] (1027).
pub const ID_KO_BUS_FADER: u16 = 1027;
/// Wire id of [keys::KO_BUS_MUTE] (1028).
pub const ID_KO_BUS_MUTE: u16 = 1028;
/// Wire id of [keys::MAIN_TRIM] (5011).
pub const ID_MAIN_TRIM: u16 = 5011;
/// Wire id of [keys::MODEL_ID] (21).
pub const ID_MODEL_ID: u16 = 21;

/// Datastore index of the Main 1 / left main output bus fader.
pub const MAIN_FADER_LEFT_INDEX: u16 = 0;
/// Datastore index of the Main 2 / right main output bus fader.
pub const MAIN_FADER_RIGHT_INDEX: u16 = 1;
/// Number of main fader channels written by set_volume.
pub const MAIN_FADER_CHANNELS: u16 = 2;

/// Human-readable labels of the 14 output buses, indexed by bus number.
///
/// Index 0/1 are the two main outputs; 2..=9 are line 3..10; 10/11 are the two
/// phone outputs; 12/13 are the reverb sends.
pub static BUS_NAMES: [&str; 14] = [
    "Main 1", "Main 2", "Line 3", "Line 4", "Line 5", "Line 6", "Line 7", "Line 8", "Line 9",
    "Line 10", "Phones 1", "Phones 2", "Reverb 1", "Reverb 2",
];

/// Label for a main fader datastore index (0 -> "Main 1", 1 -> "Main 2").
pub fn bus_label(index: u16) -> Option<&'static str> {
    BUS_NAMES.get(index as usize).copied()
}
