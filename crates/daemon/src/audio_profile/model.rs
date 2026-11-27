//! 音频通道 profile 数据模型（docs/API.md §12.1，schema khaen-audio-profile/v1）。
//!
//! - outputs 行数 = device.channels（缺省 22；1..=22）：{dac, enabled, role, gain_db, custom_label}；
//!   enabled=false 的行在规范化时把 role 收成 None（禁用行不产生 lane）。
//! - inputs.sources：aes67（RTP 流）与 device（声卡采集：模拟 + 光口都算）两类，
//!   各自可启用、每通道一个角色标签，引擎按角色自动 fan-out / 求和。
//! - downmix：缺 C -> C 输入下混到 L/R；缺 LFE/Sub -> LFE 输入分配到其它 lane。
//! - fanout：同角色多 lane 的 fold 增益（unity=1.0；group=1/sqrt(N)）。
//!
//! 校验只拒绝 4 类结构性矛盾（§12.1）：启用但角色 None、dac 越界、全部禁用、
//! Custom 无 custom_label。重复角色是正常设计（MIMO/多执行器），缺少角色只给 warning。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

/// profile schema（GET 顶层 schema 与 profile.schema 同源）。
pub const SCHEMA: &str = "khaen-audio-profile/v1";
/// 默认/兼容设备通道数（MOTU UltraLite-mk5 的 22 路）。
///
/// 这是默认值，不是固定上限：profile 的 device.channels 可以在 1..=MAX_DEVICE_CHANNELS
/// 内选择，plan 的 content.in_channels / routing.dac_channels 跟随该值。plan ABI v3 的
/// 输出通道上限仍是 22（见 khaen_core::rt::MAX_OUTPUT_CHANNELS）。
pub const DEVICE_CHANNELS: u16 = 22;
/// plan ABI v3 允许的最大输出通道数（= khaen_core::rt::MAX_OUTPUT_CHANNELS）。
pub const MAX_DEVICE_CHANNELS: u16 = 22;
/// downmix 默认增益（dB）。
pub const DEFAULT_DOWNMIX_GAIN_DB: f32 = -3.0;
/// 输入源通道上限（source.channels / fold.src 必须 < 设备通道数，且 <= ABI 输入上限）。
pub const MAX_INPUT_CHANNEL: u16 = DEVICE_CHANNELS;

/// 角色枚举（canonical 19 个；顺序即 §12.1 的 lane 排序：地面 -> 低频 -> 天空 -> 其它）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpeakerRole {
    /// 左
    L,
    /// 右
    R,
    /// 中置
    C,
    /// 左前宽
    Lw,
    /// 右前宽
    Rw,
    /// 左环绕
    Ls,
    /// 右环绕
    Rs,
    /// 左后环绕
    Lb,
    /// 右后环绕
    Rb,
    /// 低音炮（LFE）
    Lfe,
    /// 辅助超低音
    Sub,
    /// 左前天空
    Tfl,
    /// 右前天空
    Tfr,
    /// 左中天空
    Tml,
    /// 右中天空
    Tmr,
    /// 左后天空
    Trl,
    /// 右后天空
    Trr,
    /// 自定义（必须写 custom_label）
    Custom,
    /// 未使用（禁用行）
    None,
}

impl SpeakerRole {
    /// 全部 canonical 角色（顺序固定，供 lane 排序与遍历）。
    pub const ALL: [SpeakerRole; 19] = [
        SpeakerRole::L,
        SpeakerRole::R,
        SpeakerRole::C,
        SpeakerRole::Lw,
        SpeakerRole::Rw,
        SpeakerRole::Ls,
        SpeakerRole::Rs,
        SpeakerRole::Lb,
        SpeakerRole::Rb,
        SpeakerRole::Lfe,
        SpeakerRole::Sub,
        SpeakerRole::Tfl,
        SpeakerRole::Tfr,
        SpeakerRole::Tml,
        SpeakerRole::Tmr,
        SpeakerRole::Trl,
        SpeakerRole::Trr,
        SpeakerRole::Custom,
        SpeakerRole::None,
    ];

    /// canonical 字符串（代码/JSON 只用它；中文显示名走前端 i18n）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L => "L",
            Self::R => "R",
            Self::C => "C",
            Self::Lw => "Lw",
            Self::Rw => "Rw",
            Self::Ls => "Ls",
            Self::Rs => "Rs",
            Self::Lb => "Lb",
            Self::Rb => "Rb",
            Self::Lfe => "LFE",
            Self::Sub => "Sub",
            Self::Tfl => "Tfl",
            Self::Tfr => "Tfr",
            Self::Tml => "Tml",
            Self::Tmr => "Tmr",
            Self::Trl => "Trl",
            Self::Trr => "Trr",
            Self::Custom => "Custom",
            Self::None => "None",
        }
    }

    /// 解析 canonical 字符串（未知值返回 None，不静默归类）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "L" => Some(Self::L),
            "R" => Some(Self::R),
            "C" => Some(Self::C),
            "Lw" => Some(Self::Lw),
            "Rw" => Some(Self::Rw),
            "Ls" => Some(Self::Ls),
            "Rs" => Some(Self::Rs),
            "Lb" => Some(Self::Lb),
            "Rb" => Some(Self::Rb),
            "LFE" => Some(Self::Lfe),
            "Sub" => Some(Self::Sub),
            "Tfl" => Some(Self::Tfl),
            "Tfr" => Some(Self::Tfr),
            "Tml" => Some(Self::Tml),
            "Tmr" => Some(Self::Tmr),
            "Trl" => Some(Self::Trl),
            "Trr" => Some(Self::Trr),
            "Custom" => Some(Self::Custom),
            "None" => Some(Self::None),
            _ => None,
        }
    }

    /// 排序权重（§12.1 的 canonical 顺序）。
    pub fn rank(self) -> usize {
        Self::ALL
            .iter()
            .position(|role| *role == self)
            .unwrap_or(Self::ALL.len())
    }

    /// 地面角色（9.1.6 上限 9）。
    pub fn is_ground(self) -> bool {
        matches!(
            self,
            Self::L
                | Self::R
                | Self::C
                | Self::Lw
                | Self::Rw
                | Self::Ls
                | Self::Rs
                | Self::Lb
                | Self::Rb
        )
    }

    /// 天空角色（上限 6）。
    pub fn is_height(self) -> bool {
        matches!(
            self,
            Self::Tfl | Self::Tfr | Self::Tml | Self::Tmr | Self::Trl | Self::Trr
        )
    }

    /// 低频组（LFE 与 Sub 都算低音炮角色组）。
    pub fn is_low(self) -> bool {
        matches!(self, Self::Lfe | Self::Sub)
    }
}

/// 输入源种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceKind {
    /// 网络 AES67 RTP 输入。
    Aes67,
    /// 声卡采集输入（模拟 + 光口都算）。
    Device,
}

impl SourceKind {
    /// 全部来源（canonical 顺序）。
    pub const ALL: [SourceKind; 2] = [SourceKind::Aes67, SourceKind::Device];

    /// canonical 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aes67 => "aes67",
            Self::Device => "device",
        }
    }

    /// 解析 canonical 字符串。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "aes67" => Some(Self::Aes67),
            "device" => Some(Self::Device),
            _ => None,
        }
    }
}

/// 一条物理输出（22 行固定）。
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    /// DAC 序号 0..21。
    pub dac: u16,
    /// 是否启用（启用才产生 lane）。
    pub enabled: bool,
    /// 角色（禁用行为 None）。
    pub role: SpeakerRole,
    /// 该物理输出的电平微调（dB，生成方案时写进对应 lane 的 protection gain）。
    pub gain_db: f32,
    /// Custom 角色的自定义名称。
    pub custom_label: Option<String>,
}

/// 一个输入通道的角色标签。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputChannel {
    /// 该源内的通道序号（0-based；也是内容折叠的 src）。
    pub channel: u16,
    /// 角色标签（None = 未使用）。
    pub role: SpeakerRole,
}

/// 一个输入源。
#[derive(Clone, Debug, PartialEq)]
pub struct InputSource {
    /// 来源种类。
    pub kind: SourceKind,
    /// 是否启用（只有启用源的通道参与 folds）。
    pub enabled: bool,
    /// 该源的整体增益（dB；fold 增益 = 源增益 x fan-out 增益）。
    pub gain_db: f32,
    /// 每通道角色。
    pub channels: Vec<InputChannel>,
    /// AES67 布局预设（stereo / 5.1 / ...；只作记录，顺序来源见 capability）。
    pub preset: Option<String>,
}

/// 下混开关。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownmixMode {
    /// 自动下混。
    Auto,
    /// 关闭下混（缺失角色的输入保持静音）。
    Off,
}

impl DownmixMode {
    /// canonical 字符串（auto / none）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "none",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "none" => Some(Self::Off),
            _ => None,
        }
    }
}

/// 缺中置的下混配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CenterDownmix {
    /// 开关。
    pub mode: DownmixMode,
    /// 每条 L/R lane 的增益（dB）。
    pub gain_db: f32,
}

/// 缺低音炮的下混配置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfeDownmix {
    /// 开关。
    pub mode: DownmixMode,
    /// 每条目标 lane 的增益（dB）。
    pub gain_db: f32,
    /// 是否把 LFE 也分配到天空 lane。
    pub include_heights: bool,
}

/// 下混段（默认全 auto / -3 dB / 不含天空）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Downmix {
    /// 缺 C。
    pub center: CenterDownmix,
    /// 缺 LFE/Sub。
    pub lfe: LfeDownmix,
}

impl Default for Downmix {
    fn default() -> Self {
        Self {
            center: CenterDownmix {
                mode: DownmixMode::Auto,
                gain_db: DEFAULT_DOWNMIX_GAIN_DB,
            },
            lfe: LfeDownmix {
                mode: DownmixMode::Auto,
                gain_db: DEFAULT_DOWNMIX_GAIN_DB,
                include_heights: false,
            },
        }
    }
}

/// 同角色 fan-out 模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FanoutMode {
    /// 每 lane 1.0（用每路 gain_db 微调）。
    Unity,
    /// 同角色组按 1/sqrt(N) 归一化（被动并联音箱）。
    Group,
}

impl FanoutMode {
    /// canonical 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unity => "unity",
            Self::Group => "group",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "unity" => Some(Self::Unity),
            "group" => Some(Self::Group),
            _ => None,
        }
    }

    /// 一条 fold 的 fan-out 增益（同一角色组有 lane_count 条 lane）。
    pub fn lane_gain(self, lane_count: usize) -> f32 {
        match self {
            Self::Unity => 1.0,
            Self::Group => {
                if lane_count == 0 {
                    1.0
                } else {
                    1.0 / (lane_count as f32).sqrt()
                }
            }
        }
    }
}

/// 一条启用 lane（lane 序号 = §12.1 排序后的位置）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lane {
    /// lane 序号 0..N-1。
    pub lane: u16,
    /// 该 lane 的物理输出 DAC。
    pub dac: u16,
    /// 角色。
    pub role: SpeakerRole,
    /// 该 lane 的 protection 电平（dB）。
    pub gain_db: f32,
}

/// 完整 profile。
#[derive(Clone, Debug, PartialEq)]
pub struct AudioProfile {
    /// 设备通道数（1..=22；缺省 22）。
    pub device_channels: u16,
    /// device_channels 行物理输出。
    pub outputs: Vec<OutputRow>,
    /// 输入源（aes67 + device，各自一条）。
    pub sources: Vec<InputSource>,
    /// 下混配置。
    pub downmix: Downmix,
    /// fan-out 模式。
    pub fanout: FanoutMode,
}

impl Default for AudioProfile {
    /// 首次运行默认 = 2.0：DAC0=L、DAC1=R，其余禁用；只启用 AES67 ch0=L/ch1=R。
    fn default() -> Self {
        let mut outputs: Vec<OutputRow> = (0..DEVICE_CHANNELS)
            .map(|dac| OutputRow {
                dac,
                enabled: dac < 2,
                role: match dac {
                    0 => SpeakerRole::L,
                    1 => SpeakerRole::R,
                    _ => SpeakerRole::None,
                },
                gain_db: 0.0,
                custom_label: None,
            })
            .collect();
        outputs.sort_by_key(|row| row.dac);
        Self {
            device_channels: DEVICE_CHANNELS,
            outputs,
            sources: default_sources(DEVICE_CHANNELS),
            downmix: Downmix::default(),
            fanout: FanoutMode::Unity,
        }
    }
}

/// 某个来源的默认配置。
///
/// 默认输入通道数不能超过当前设备通道数：plan 的 content.in_channels 跟随
/// profile.device.channels，越界的默认通道会让 profile 自身不合法。
fn default_source(kind: SourceKind, enabled: bool, device_channels: u16) -> InputSource {
    match kind {
        SourceKind::Aes67 => {
            let count = device_channels.min(2);
            InputSource {
                kind,
                enabled,
                gain_db: 0.0,
                channels: (0..count)
                    .map(|channel| InputChannel {
                        channel,
                        role: if channel == 0 {
                            SpeakerRole::L
                        } else {
                            SpeakerRole::R
                        },
                    })
                    .collect(),
                preset: Some("stereo".to_string()),
            }
        }
        SourceKind::Device => {
            let count = device_channels.min(4);
            InputSource {
                kind,
                enabled,
                gain_db: 0.0,
                channels: (0..count)
                    .map(|channel| InputChannel {
                        channel,
                        role: SpeakerRole::None,
                    })
                    .collect(),
                preset: None,
            }
        }
    }
}

/// 首次运行的两个输入源（AES67 启用、声卡输入关闭）。
fn default_sources(device_channels: u16) -> Vec<InputSource> {
    vec![
        default_source(SourceKind::Aes67, true, device_channels),
        default_source(SourceKind::Device, false, device_channels),
    ]
}

/// 请求形状错误（不是 profile 校验错误；HTTP 400 bad_request）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 字段路径（如 outputs[3].dac）。
    pub field: String,
    /// 人读说明。
    pub message: String,
}

impl ParseError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

/// 校验问题（errors/warnings 的形状）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// 稳定机器码（如 enabled_without_role）。
    pub code: &'static str,
    /// 字段路径（如 outputs[3]）。
    pub path: String,
    /// 人读说明。
    pub message: String,
}

impl Issue {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({ "code": self.code, "path": self.path, "message": self.message })
    }
}

/// profile 校验结果。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Validation {
    /// 结构性错误（只来自 §12.1 的 4 类）。
    pub errors: Vec<Issue>,
    /// 信息提示（缺角色/下混/上限；不阻塞保存）。
    pub warnings: Vec<Issue>,
}

impl Validation {
    /// 是否可保存/生成方案。
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// REST 形状 {ok, errors, warnings}。
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok(),
            "errors": self.errors.iter().map(Issue::to_json).collect::<Vec<_>>(),
            "warnings": self.warnings.iter().map(Issue::to_json).collect::<Vec<_>>(),
        })
    }
}

/// prepare 的 diff 段（四块是否变化）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ProfileDiff {
    /// outputs 段变化。
    pub outputs_changed: bool,
    /// inputs 段变化。
    pub inputs_changed: bool,
    /// downmix 段变化。
    pub downmix_changed: bool,
    /// fanout 段变化。
    pub fanout_changed: bool,
}

impl ProfileDiff {
    /// REST 形状。
    pub fn to_json(self) -> Value {
        json!({
            "outputs_changed": self.outputs_changed,
            "inputs_changed": self.inputs_changed,
            "downmix_changed": self.downmix_changed,
            "fanout_changed": self.fanout_changed,
        })
    }
}

impl AudioProfile {
    /// 从 JSON 解析成规范化 profile（宽松：缺省段补默认值、缺 dac 行补禁用行；
    /// 形状错误返回 ParseError，profile 语义错误交给 validate）。
    pub fn parse(value: &Value) -> Result<Self, ParseError> {
        let object = value
            .as_object()
            .ok_or_else(|| ParseError::new("profile", "profile must be a JSON object"))?;
        if let Some(schema) = object.get("schema") {
            match schema {
                Value::String(text) if text == SCHEMA => {}
                Value::String(text) => {
                    return Err(ParseError::new(
                        "profile.schema",
                        format!("unsupported profile schema {text:?} (expected {SCHEMA})"),
                    ))
                }
                _ => {
                    return Err(ParseError::new(
                        "profile.schema",
                        "profile.schema must be a string",
                    ))
                }
            }
        }
        let device_channels = parse_device_channels(object.get("device"))?;
        let outputs = canonical_outputs(parse_outputs(object.get("outputs"))?, device_channels);
        let sources = match object.get("inputs") {
            None => default_sources(device_channels),
            Some(inputs) => {
                canonical_sources(parse_sources(inputs, device_channels)?, device_channels)
            }
        };
        let downmix = parse_downmix(object.get("downmix"))?;
        let fanout = parse_fanout(object.get("fanout"))?;
        Ok(Self {
            device_channels,
            outputs,
            sources,
            downmix,
            fanout,
        })
    }

    /// REST 形状（GET / prepare 回执里的 profile；行数 = device_channels）。
    pub fn to_json(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "device": { "channels": self.device_channels },
            "outputs": self.outputs.iter().map(output_json).collect::<Vec<_>>(),
            "inputs": {
                "sources": self.sources.iter().map(source_json).collect::<Vec<_>>(),
            },
            "downmix": {
                "center": { "mode": self.downmix.center.mode.as_str(), "gain_db": f64::from(self.downmix.center.gain_db) },
                "lfe": {
                    "mode": self.downmix.lfe.mode.as_str(),
                    "gain_db": f64::from(self.downmix.lfe.gain_db),
                    "include_heights": self.downmix.lfe.include_heights,
                },
            },
            "fanout": { "mode": self.fanout.as_str() },
        })
    }

    /// 启用 lane（§12.1：角色 canonical 顺序，同角色内 DAC 升序）。
    pub fn enabled_lanes(&self) -> Vec<Lane> {
        let mut rows: Vec<&OutputRow> = self
            .outputs
            .iter()
            .filter(|row| row.enabled && row.role != SpeakerRole::None)
            .collect();
        rows.sort_by(|left, right| {
            left.role
                .rank()
                .cmp(&right.role.rank())
                .then(left.dac.cmp(&right.dac))
        });
        rows.into_iter()
            .enumerate()
            .map(|(lane, row)| Lane {
                lane: lane as u16,
                dac: row.dac,
                role: row.role,
                gain_db: row.gain_db,
            })
            .collect()
    }

    /// 某角色启用的 lane（lane 序号升序）。
    pub fn lanes_with_role(&self, role: SpeakerRole) -> Vec<Lane> {
        self.enabled_lanes()
            .into_iter()
            .filter(|lane| lane.role == role)
            .collect()
    }

    /// 启用输入通道（来源, 通道）；只含启用源里角色非 None 的通道。
    pub fn enabled_inputs(&self) -> Vec<(SourceKind, InputChannel)> {
        let mut inputs = Vec::new();
        for source in &self.sources {
            if !source.enabled {
                continue;
            }
            for channel in &source.channels {
                if channel.role != SpeakerRole::None {
                    inputs.push((source.kind, *channel));
                }
            }
        }
        inputs
    }

    /// 某角色组的启用输入通道数。
    pub fn input_count(&self, roles: &[SpeakerRole]) -> usize {
        self.enabled_inputs()
            .iter()
            .filter(|(_, channel)| roles.contains(&channel.role))
            .count()
    }

    /// 某来源的整体增益（dB -> 线性）。
    pub fn source_gain(&self, kind: SourceKind) -> f32 {
        self.sources
            .iter()
            .find(|source| source.kind == kind)
            .map(|source| db_to_gain(source.gain_db))
            .unwrap_or(1.0)
    }

    /// §12.1 的 4 类结构性错误 + 信息提示。
    pub fn validate(&self) -> Validation {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        let mut enabled_count = 0_usize;
        for row in &self.outputs {
            let path = format!("outputs[{}]", row.dac);
            if row.dac >= self.device_channels {
                errors.push(Issue::new(
                    "dac_out_of_range",
                    format!("{path}.dac"),
                    format!(
                        "DAC {} is outside 0..{} (device channels = {})",
                        row.dac,
                        self.device_channels.saturating_sub(1),
                        self.device_channels
                    ),
                ));
                continue;
            }
            if !row.enabled {
                continue;
            }
            enabled_count += 1;
            if row.role == SpeakerRole::None {
                errors.push(Issue::new(
                    "enabled_without_role",
                    path.clone(),
                    "an enabled output must have a role other than None",
                ));
            }
            if row.role == SpeakerRole::Custom {
                let labelled = row
                    .custom_label
                    .as_deref()
                    .is_some_and(|label| !label.trim().is_empty());
                if !labelled {
                    errors.push(Issue::new(
                        "custom_without_label",
                        format!("{path}.custom_label"),
                        "a Custom role needs a non-empty custom_label",
                    ));
                }
            }
        }
        if enabled_count == 0 {
            errors.push(Issue::new(
                "no_enabled_outputs",
                "outputs",
                "all physical outputs are disabled: at least one speaker must be enabled",
            ));
        }

        let lanes = self.enabled_lanes();
        let enabled_sources = self.sources.iter().filter(|source| source.enabled).count();

        let center_inputs = self.input_count(&[SpeakerRole::C]);
        let center_lanes = lanes
            .iter()
            .filter(|lane| lane.role == SpeakerRole::C)
            .count();
        if center_lanes == 0 && center_inputs > 0 {
            match self.downmix.center.mode {
                DownmixMode::Auto => {
                    let lr_lanes = lanes
                        .iter()
                        .filter(|lane| matches!(lane.role, SpeakerRole::L | SpeakerRole::R))
                        .count();
                    if lr_lanes > 0 {
                        warnings.push(Issue::new(
                            "downmix_center",
                            "downmix.center",
                            format!(
                                "no center lane is enabled: {center_inputs} center input channel(s) will be folded into {lr_lanes} L/R lane(s) at {:.1} dB each",
                                self.downmix.center.gain_db
                            ),
                        ));
                    } else {
                        warnings.push(Issue::new(
                            "unmapped_center",
                            "downmix.center",
                            format!(
                                "no center lane and no L/R lane are enabled: {center_inputs} center input channel(s) stay silent"
                            ),
                        ));
                    }
                }
                DownmixMode::Off => warnings.push(Issue::new(
                    "unmapped_center",
                    "downmix.center",
                    format!(
                        "center downmix is off and no center lane is enabled: {center_inputs} center input channel(s) stay silent"
                    ),
                )),
            }
        }

        let low_inputs = self.input_count(&[SpeakerRole::Lfe, SpeakerRole::Sub]);
        let low_lanes = lanes.iter().filter(|lane| lane.role.is_low()).count();
        if low_lanes == 0 && low_inputs > 0 {
            match self.downmix.lfe.mode {
                DownmixMode::Auto => {
                    let targets = lfe_downmix_targets(&lanes, self.downmix.lfe.include_heights);
                    if targets.is_empty() {
                        warnings.push(Issue::new(
                            "unmapped_lfe",
                            "downmix.lfe",
                            format!(
                                "no LFE/Sub lane is enabled and there is no other lane to receive the low-frequency content (include_heights={}): {low_inputs} LFE input channel(s) stay silent",
                                self.downmix.lfe.include_heights
                            ),
                        ));
                    } else {
                        warnings.push(Issue::new(
                            "downmix_lfe",
                            "downmix.lfe",
                            format!(
                                "no LFE/Sub lane is enabled: {low_inputs} LFE input channel(s) will be distributed to {} lane(s) at {:.1} dB each (include_heights={})",
                                targets.len(),
                                self.downmix.lfe.gain_db,
                                self.downmix.lfe.include_heights
                            ),
                        ));
                    }
                }
                DownmixMode::Off => warnings.push(Issue::new(
                    "unmapped_lfe",
                    "downmix.lfe",
                    format!(
                        "LFE downmix is off and no LFE/Sub lane is enabled: {low_inputs} LFE input channel(s) stay silent"
                    ),
                )),
            }
        }

        for role in SpeakerRole::ALL {
            if matches!(
                role,
                SpeakerRole::None | SpeakerRole::C | SpeakerRole::Lfe | SpeakerRole::Sub
            ) {
                continue;
            }
            let inputs = self.input_count(&[role]);
            let lanes_for_role = lanes.iter().filter(|lane| lane.role == role).count();
            if inputs > 0 && lanes_for_role == 0 {
                warnings.push(Issue::new(
                    "unmapped_input_role",
                    "outputs",
                    format!(
                        "input role {} has no enabled output lane and no automatic downmix: {inputs} input channel(s) stay silent",
                        role.as_str()
                    ),
                ));
            } else if inputs == 0 && lanes_for_role > 0 {
                warnings.push(Issue::new(
                    "role_without_input",
                    "outputs",
                    format!(
                        "role {} has {lanes_for_role} enabled lane(s) but no enabled input channel: they stay silent",
                        role.as_str()
                    ),
                ));
            }
        }
        if low_inputs == 0 && low_lanes > 0 {
            warnings.push(Issue::new(
                "role_without_input",
                "outputs",
                format!(
                    "the low-frequency group has {low_lanes} enabled lane(s) but no enabled LFE/Sub input channel: they stay silent"
                ),
            ));
        }
        if center_inputs == 0 && center_lanes > 0 {
            warnings.push(Issue::new(
                "role_without_input",
                "outputs",
                format!(
                    "role C has {center_lanes} enabled lane(s) but no enabled input channel: they stay silent"
                ),
            ));
        }

        if enabled_sources == 0 {
            warnings.push(Issue::new(
                "no_input_source",
                "inputs.sources",
                "no input source is enabled: the generated plan carries no content (all lanes stay silent)",
            ));
        } else if self.enabled_inputs().is_empty() {
            warnings.push(Issue::new(
                "no_content",
                "inputs.sources",
                "no enabled input channel has a role: the generated plan carries no content (all lanes stay silent)",
            ));
        }

        let ground = lanes.iter().filter(|lane| lane.role.is_ground()).count();
        let height = lanes.iter().filter(|lane| lane.role.is_height()).count();
        if ground > 9 {
            warnings.push(Issue::new(
                "ground_role_cap",
                "outputs",
                format!("{ground} ground speakers are enabled; the 9.1.6 upper bound is 9"),
            ));
        }
        if height > 6 {
            warnings.push(Issue::new(
                "height_role_cap",
                "outputs",
                format!("{height} height speakers are enabled; the 9.1.6 upper bound is 6"),
            ));
        }

        Validation { errors, warnings }
    }

    /// §12.2 prepare 的 diff（四块分别比较）。
    pub fn diff(&self, next: &AudioProfile) -> ProfileDiff {
        ProfileDiff {
            outputs_changed: self.outputs != next.outputs,
            inputs_changed: self.sources != next.sources,
            downmix_changed: self.downmix != next.downmix,
            fanout_changed: self.fanout != next.fanout,
        }
    }
}

/// dBFS -> 线性增益（0 dB = 1.0）。
pub fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// LFE 下混的目标 lane（§12.3：其它所有启用 lane；默认不含天空）。
pub fn lfe_downmix_targets(lanes: &[Lane], include_heights: bool) -> Vec<Lane> {
    lanes
        .iter()
        .copied()
        .filter(|lane| !lane.role.is_low())
        .filter(|lane| include_heights || !lane.role.is_height())
        .collect()
}

/// 输出行 REST 形状。
fn output_json(row: &OutputRow) -> Value {
    json!({
        "dac": row.dac,
        "enabled": row.enabled,
        "role": row.role.as_str(),
        "gain_db": f64::from(row.gain_db),
        "custom_label": row.custom_label,
    })
}

/// 输入源 REST 形状。
fn source_json(source: &InputSource) -> Value {
    json!({
        "kind": source.kind.as_str(),
        "enabled": source.enabled,
        "gain_db": f64::from(source.gain_db),
        "channels": source
            .channels
            .iter()
            .map(|channel| json!({ "channel": channel.channel, "role": channel.role.as_str() }))
            .collect::<Vec<_>>(),
        "preset": source.preset,
    })
}

fn parse_device_channels(value: Option<&Value>) -> Result<u16, ParseError> {
    let Some(value) = value else {
        return Ok(DEVICE_CHANNELS);
    };
    let device = value
        .as_object()
        .ok_or_else(|| ParseError::new("profile.device", "device must be an object"))?;
    let Some(channels) = device.get("channels") else {
        return Ok(DEVICE_CHANNELS);
    };
    let channels = integer_field(channels, "profile.device.channels")?;
    let channels = u16::try_from(channels).map_err(|_| {
        ParseError::new(
            "profile.device.channels",
            "device channel count does not fit u16",
        )
    })?;
    if channels == 0 || channels > MAX_DEVICE_CHANNELS {
        return Err(ParseError::new(
            "profile.device.channels",
            format!(
                "device channel count {channels} is outside 1..={MAX_DEVICE_CHANNELS} (plan ABI v3 output limit; {DEVICE_CHANNELS} is only the MOTU default)"
            ),
        ));
    }
    Ok(channels)
}

fn integer_field(value: &Value, field: &str) -> Result<u64, ParseError> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| ParseError::new(field, "must be a non-negative integer")),
        _ => Err(ParseError::new(field, "must be a non-negative integer")),
    }
}

fn bool_field(object: &Map<String, Value>, name: &str, field: &str) -> Result<bool, ParseError> {
    match object.get(name) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ParseError::new(field, "must be a boolean")),
        None => Err(ParseError::new(field, "missing field")),
    }
}

fn optional_bool(
    object: &Map<String, Value>,
    name: &str,
    field: &str,
    default: bool,
) -> Result<bool, ParseError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ParseError::new(field, "must be a boolean")),
    }
}

fn optional_f32(
    object: &Map<String, Value>,
    name: &str,
    field: &str,
    default: f32,
) -> Result<f32, ParseError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(number)) => {
            let value = number
                .as_f64()
                .ok_or_else(|| ParseError::new(field, "must be a finite number"))?;
            let value = value as f32;
            if value.is_finite() {
                Ok(value)
            } else {
                Err(ParseError::new(field, "must be a finite number"))
            }
        }
        Some(_) => Err(ParseError::new(field, "must be a number")),
    }
}

fn parse_role(object: &Map<String, Value>, field: &str) -> Result<SpeakerRole, ParseError> {
    match object.get("role") {
        None | Some(Value::Null) => Ok(SpeakerRole::None),
        Some(Value::String(text)) => SpeakerRole::parse(text)
            .ok_or_else(|| ParseError::new(field, format!("unknown role {text:?}"))),
        Some(_) => Err(ParseError::new(field, "must be a canonical role string")),
    }
}

fn parse_outputs(value: Option<&Value>) -> Result<Vec<OutputRow>, ParseError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| ParseError::new("profile.outputs", "outputs must be an array"))?;
    let mut rows = Vec::with_capacity(array.len());
    let mut seen: BTreeSet<u16> = BTreeSet::new();
    for (index, item) in array.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            ParseError::new(
                format!("profile.outputs[{index}]"),
                "output row must be an object",
            )
        })?;
        let dac_field = format!("profile.outputs[{index}].dac");
        let dac = integer_field(
            object
                .get("dac")
                .ok_or_else(|| ParseError::new(&dac_field, "missing field"))?,
            &dac_field,
        )?;
        let dac = u16::try_from(dac).map_err(|_| {
            ParseError::new(
                &dac_field,
                "DAC index does not fit the device channel range",
            )
        })?;
        if !seen.insert(dac) {
            return Err(ParseError::new(
                &dac_field,
                format!("duplicate DAC {dac}: outputs must list each physical output once"),
            ));
        }
        let enabled = bool_field(
            object,
            "enabled",
            &format!("profile.outputs[{index}].enabled"),
        )?;
        let role = parse_role(object, &format!("profile.outputs[{index}].role"))?;
        let gain_db = optional_f32(
            object,
            "gain_db",
            &format!("profile.outputs[{index}].gain_db"),
            0.0,
        )?;
        let custom_label = match object.get("custom_label") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                return Err(ParseError::new(
                    format!("profile.outputs[{index}].custom_label"),
                    "must be a string or null",
                ))
            }
        };
        rows.push(OutputRow {
            dac,
            enabled,
            role,
            gain_db,
            custom_label,
        });
    }
    Ok(rows)
}

/// 规范化 outputs：禁用行 role 归 None、非 Custom 行丢 label、补齐 device_channels 行、按 dac 排序。
fn canonical_outputs(rows: Vec<OutputRow>, device_channels: u16) -> Vec<OutputRow> {
    let mut by_dac: BTreeMap<u16, OutputRow> = BTreeMap::new();
    let mut extra: Vec<OutputRow> = Vec::new();
    for mut row in rows {
        if !row.enabled {
            row.role = SpeakerRole::None;
        }
        if row.role != SpeakerRole::Custom {
            row.custom_label = None;
        }
        if row.dac < device_channels {
            by_dac.insert(row.dac, row);
        } else {
            extra.push(row);
        }
    }
    let mut outputs: Vec<OutputRow> = (0..device_channels)
        .map(|dac| {
            by_dac.remove(&dac).unwrap_or(OutputRow {
                dac,
                enabled: false,
                role: SpeakerRole::None,
                gain_db: 0.0,
                custom_label: None,
            })
        })
        .collect();
    outputs.extend(extra);
    outputs.sort_by_key(|row| row.dac);
    outputs
}

fn parse_sources(inputs: &Value, device_channels: u16) -> Result<Vec<InputSource>, ParseError> {
    let object = inputs
        .as_object()
        .ok_or_else(|| ParseError::new("profile.inputs", "inputs must be an object"))?;
    let Some(value) = object.get("sources") else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| ParseError::new("profile.inputs.sources", "sources must be an array"))?;
    let mut sources = Vec::with_capacity(array.len());
    let mut seen: BTreeSet<SourceKind> = BTreeSet::new();
    for (index, item) in array.iter().enumerate() {
        let source = item.as_object().ok_or_else(|| {
            ParseError::new(
                format!("profile.inputs.sources[{index}]"),
                "input source must be an object",
            )
        })?;
        let field = format!("profile.inputs.sources[{index}]");
        let kind = match source.get("kind") {
            Some(Value::String(text)) => SourceKind::parse(text).ok_or_else(|| {
                ParseError::new(
                    format!("{field}.kind"),
                    format!("unknown source kind {text:?} (expected aes67 or device)"),
                )
            })?,
            Some(_) => return Err(ParseError::new(format!("{field}.kind"), "must be a string")),
            None => return Err(ParseError::new(format!("{field}.kind"), "missing field")),
        };
        if !seen.insert(kind) {
            return Err(ParseError::new(
                format!("{field}.kind"),
                format!(
                    "duplicate source kind {:?}: list each input source once",
                    kind.as_str()
                ),
            ));
        }
        let enabled = optional_bool(source, "enabled", &format!("{field}.enabled"), false)?;
        let gain_db = optional_f32(source, "gain_db", &format!("{field}.gain_db"), 0.0)?;
        let preset = match source.get("preset") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => {
                return Err(ParseError::new(
                    format!("{field}.preset"),
                    "must be a string or null",
                ))
            }
        };
        let mut channels = Vec::new();
        if let Some(value) = source.get("channels") {
            let array = value.as_array().ok_or_else(|| {
                ParseError::new(format!("{field}.channels"), "channels must be an array")
            })?;
            let mut seen_channels: BTreeSet<u16> = BTreeSet::new();
            for (channel_index, item) in array.iter().enumerate() {
                let channel_object = item.as_object().ok_or_else(|| {
                    ParseError::new(
                        format!("{field}.channels[{channel_index}]"),
                        "input channel must be an object",
                    )
                })?;
                let channel_field = format!("{field}.channels[{channel_index}].channel");
                let channel = integer_field(
                    channel_object
                        .get("channel")
                        .ok_or_else(|| ParseError::new(&channel_field, "missing field"))?,
                    &channel_field,
                )?;
                let channel = u16::try_from(channel).map_err(|_| {
                    ParseError::new(&channel_field, "input channel index out of range")
                })?;
                if channel >= device_channels || channel >= MAX_INPUT_CHANNEL {
                    return Err(ParseError::new(
                        &channel_field,
                        format!(
                            "input channel {channel} is outside 0..{}: the plan content container has {device_channels} channel(s) (device.channels), and the ABI input cap is {MAX_INPUT_CHANNEL}",
                            device_channels.min(MAX_INPUT_CHANNEL).saturating_sub(1)
                        ),
                    ));
                }
                if !seen_channels.insert(channel) {
                    return Err(ParseError::new(
                        &channel_field,
                        format!("duplicate input channel {channel} in this source"),
                    ));
                }
                let role = parse_role(
                    channel_object,
                    &format!("{field}.channels[{channel_index}].role"),
                )?;
                channels.push(InputChannel { channel, role });
            }
        }
        sources.push(InputSource {
            kind,
            enabled,
            gain_db,
            channels,
            preset,
        });
    }
    Ok(sources)
}

/// 规范化 sources：两类来源各一条（缺的补默认禁用配置）、按 kind 排序、通道按序号排序。
fn canonical_sources(sources: Vec<InputSource>, device_channels: u16) -> Vec<InputSource> {
    let mut by_kind: BTreeMap<SourceKind, InputSource> = BTreeMap::new();
    for source in sources {
        by_kind.insert(source.kind, source);
    }
    let mut canonical = Vec::with_capacity(SourceKind::ALL.len());
    for kind in SourceKind::ALL {
        let mut source = by_kind
            .remove(&kind)
            .unwrap_or_else(|| default_source(kind, false, device_channels));
        source.channels.sort_by_key(|channel| channel.channel);
        canonical.push(source);
    }
    canonical
}

fn parse_downmix(value: Option<&Value>) -> Result<Downmix, ParseError> {
    let Some(value) = value else {
        return Ok(Downmix::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| ParseError::new("profile.downmix", "downmix must be an object"))?;
    let center = match object.get("center") {
        None | Some(Value::Null) => Downmix::default().center,
        Some(value) => {
            let center = value.as_object().ok_or_else(|| {
                ParseError::new("profile.downmix.center", "center must be an object")
            })?;
            CenterDownmix {
                mode: parse_downmix_mode(center, "profile.downmix.center")?,
                gain_db: optional_f32(
                    center,
                    "gain_db",
                    "profile.downmix.center.gain_db",
                    DEFAULT_DOWNMIX_GAIN_DB,
                )?,
            }
        }
    };
    let lfe = match object.get("lfe") {
        None | Some(Value::Null) => Downmix::default().lfe,
        Some(value) => {
            let lfe = value
                .as_object()
                .ok_or_else(|| ParseError::new("profile.downmix.lfe", "lfe must be an object"))?;
            LfeDownmix {
                mode: parse_downmix_mode(lfe, "profile.downmix.lfe")?,
                gain_db: optional_f32(
                    lfe,
                    "gain_db",
                    "profile.downmix.lfe.gain_db",
                    DEFAULT_DOWNMIX_GAIN_DB,
                )?,
                include_heights: optional_bool(
                    lfe,
                    "include_heights",
                    "profile.downmix.lfe.include_heights",
                    false,
                )?,
            }
        }
    };
    Ok(Downmix { center, lfe })
}

fn parse_downmix_mode(object: &Map<String, Value>, field: &str) -> Result<DownmixMode, ParseError> {
    match object.get("mode") {
        None | Some(Value::Null) => Ok(DownmixMode::Auto),
        Some(Value::String(text)) => DownmixMode::parse(text).ok_or_else(|| {
            ParseError::new(
                format!("{field}.mode"),
                format!("unknown mode {text:?} (expected auto or none)"),
            )
        }),
        Some(_) => Err(ParseError::new(format!("{field}.mode"), "must be a string")),
    }
}

fn parse_fanout(value: Option<&Value>) -> Result<FanoutMode, ParseError> {
    let Some(value) = value else {
        return Ok(FanoutMode::Unity);
    };
    let object = value
        .as_object()
        .ok_or_else(|| ParseError::new("profile.fanout", "fanout must be an object"))?;
    match object.get("mode") {
        None | Some(Value::Null) => Ok(FanoutMode::Unity),
        Some(Value::String(text)) => FanoutMode::parse(text).ok_or_else(|| {
            ParseError::new(
                "profile.fanout.mode",
                format!("unknown mode {text:?} (expected unity or group)"),
            )
        }),
        Some(_) => Err(ParseError::new("profile.fanout.mode", "must be a string")),
    }
}
