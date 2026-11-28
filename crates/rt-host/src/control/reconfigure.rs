//! 热重配（M3b）：rt-host 数据面在**不退出进程**的前提下换通道数/采样率/AES67 输入映射。
//!
//! 协议（沿用同一条 UNIX socket 一行 JSON）：
//!
//!     channels_status   -> 当前 format/channels/AES67/plan_rate/device + generation + last_result
//!     channels_prepare  -> 校验（map 长度、PT、支持速率、只读设备探测、准入）-> token + planned
//!                          generation + diff；不同 token 的 prepare 覆盖旧草稿（幂等重放同 token）
//!     channels_commit   -> 块边界执行（块线程）：静音 -> 关旧 PCM -> 换配置/重开设备/重建收包与
//!                          输入抖动环 -> hw 回读校验 -> 恢复块线程；失败按旧配置回滚
//!     channels_abort    -> 丢弃草稿（无副作用）
//!
//! # 状态机归属
//!
//! 控制线程（[`ChannelsState`]）只做协议/校验/草稿；真正换配置在块线程（唯一 Runtime owner），
//! 经 `ControlCommand::Reconfigure` 命令通道 + 一次性 ack 执行，机械动作在
//! `khaen_host::Runtime::reconfigure`（host seam）。因此 `Runtime::run_block` 一行不改。
//!
//! # 失败语义（fail-closed）
//!
//! - 校验失败：什么都没碰，`bad_request` / `unsupported` / `device_unavailable`；
//! - 提交失败：host 尽力按旧配置重开旧设备与旧收包线程，响应里 `rolled_back` 如实说明回滚
//!   是否完整；`generation` 不前进（不伪造成已生效），`last_result` 记录失败与 gap；
//! - 块线程不响应（ack 超时）：本次提交结果未知，`channels_busy` 保持到块线程的追踪器里出现
//!   结果（恢复后自动对账，避免重复生效/状态漂移）；
//! - 与校准模式互斥：校准模式中 commit -> `cal_busy`；重配进行中 cal_enter -> `reconfiguring`。

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use khaen_core::rt::{MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ};
use khaen_core::sha256::{sha256, sha256_hex};
use khaen_host::admission::AdmissionLimits;
use khaen_host::aes67_input::{
    Aes67InputConfig, MAX_OUTPUT_CHANNELS as MAX_INPUT_BUS_CHANNELS, MAX_RTP_CHANNELS,
};
use khaen_host::backend::DeviceConfig;
use khaen_host::control::{engine_token, ControlError};
use khaen_host::engine::EngineConfig;
use khaen_host::owner::DeviceId;
use khaen_host::runtime::{HwReadback, ReconfigureFailure, ReconfigureOutcome, RuntimeConfig};
use serde_json::{json, Map, Value};

use super::snapshot::RuntimeSnapshot;

/// 本模块接通的 op（status.wired 用）。
pub const CHANNELS_OPS: &[&str] = &[
    "channels_status",
    "channels_prepare",
    "channels_commit",
    "channels_abort",
];

/// 控制面稳定错误码（host 的 ReconfigureFailure.code 直接原样上报，见 error 模块）。
pub mod error {
    use khaen_host::control::error as host_error;

    /// 上一笔提交结果未知/仍在执行（单活动；等结果或看 channels_status）。
    pub const CHANNELS_BUSY: &str = "channels_busy";
    /// 重配进行中（校准 enter 拒绝用）。
    pub const RECONFIGURING: &str = "reconfiguring";
    /// 只读设备探测失败：设备不在位/读不到 hw params。
    pub const DEVICE_UNAVAILABLE: &str = "device_unavailable";
    /// 没有待提交的 channels 草稿。
    pub const NO_CHANNELS_DRAFT: &str = "no_channels_draft";
    /// 通道/格式目标不被准入接受。
    pub const ADMISSION_REJECTED: &str = host_error::ADMISSION_REJECTED;
    /// engine_token 无效。
    pub const TOKEN_INVALID: &str = host_error::ENGINE_TOKEN_INVALID;
    /// 请求字段非法。
    pub const BAD_REQUEST: &str = host_error::BAD_REQUEST;
}

// ===========================================================================
// 当前配置视图（协议字段的形状）
// ===========================================================================

/// 数据面通道/格式/AES67 输入配置的协议视图（channels_status 的 `current`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelsView {
    /// 输出通道数（引擎/设备/DAC）。
    pub channels: usize,
    /// 引擎输入宽度（plan content.in_channels / AES67 输入总线宽度）。
    /// 与 `channels` 独立：2 输入 → 22 输出时这里是 2、channels 是 22。
    pub content_channels: usize,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 块长（帧；format op 管，channels 重配不改）。
    pub block_frames: usize,
    /// AES67 组播地址。
    pub aes67_group: Ipv4Addr,
    /// AES67 UDP 端口。
    pub aes67_port: u16,
    /// AES67 RTP 载荷类型（96..=127）。
    pub aes67_payload_type: u8,
    /// AES67 输入（RTP）通道数。
    pub aes67_channels: usize,
    /// RTP 通道 -> 输出位置的映射（长度 = aes67_channels，值 < channels）。
    pub aes67_map: Vec<u16>,
    /// 抖动目标帧数。
    pub jitter_frames: usize,
    /// 生效方案速率（设备监督器口径）。
    pub plan_rate: u32,
    /// ALSA 设备名。
    pub device: String,
}

impl ChannelsView {
    /// 从运行时配置构造视图。
    pub fn from_config(config: &RuntimeConfig) -> Self {
        Self {
            channels: config.engine.output_channels,
            content_channels: config.engine.input_channels,
            sample_rate: config.engine.sample_rate,
            block_frames: config.engine.block_frames,
            aes67_group: config.input.multicast_addr,
            aes67_port: config.input.port,
            aes67_payload_type: config.input.payload_type,
            aes67_channels: config.input.channels,
            aes67_map: config.input.channels_map.clone(),
            jitter_frames: config.input.jitter_target_frames,
            plan_rate: config.effective_plan_rate(),
            device: config.device.device.as_str().to_string(),
        }
    }

    /// 协议 JSON（字段稳定；通道映射为空时是 `[]`）。
    pub fn to_json(&self) -> Value {
        json!({
            "channels": self.channels,
            "content_channels": self.content_channels,
            "sample_rate": self.sample_rate,
            "block_frames": self.block_frames,
            "aes67_group": self.aes67_group.to_string(),
            "aes67_port": self.aes67_port,
            "aes67_payload_type": self.aes67_payload_type,
            "aes67_channels": self.aes67_channels,
            "aes67_map": self.aes67_map,
            "jitter_frames": self.jitter_frames,
            "plan_rate": self.plan_rate,
            "device": self.device,
        })
    }

    /// 两个视图之间的差异（只列变化字段；`{"channels":{"from":2,"to":4}, ...}`）。
    pub fn diff(&self, target: &Self) -> Value {
        let mut diff = Map::new();
        macro_rules! field {
            ($name:literal, $old:expr, $new:expr) => {
                if $old != $new {
                    diff.insert(
                        $name.to_string(),
                        json!({ "from": $old, "to": $new }),
                    );
                }
            };
        }
        field!("channels", self.channels, target.channels);
        field!(
            "content_channels",
            self.content_channels,
            target.content_channels
        );
        field!("sample_rate", self.sample_rate, target.sample_rate);
        field!("block_frames", self.block_frames, target.block_frames);
        if self.aes67_group != target.aes67_group {
            diff.insert(
                "aes67_group".to_string(),
                json!({ "from": self.aes67_group.to_string(), "to": target.aes67_group.to_string() }),
            );
        }
        field!("aes67_port", self.aes67_port, target.aes67_port);
        field!(
            "aes67_payload_type",
            self.aes67_payload_type,
            target.aes67_payload_type
        );
        field!("aes67_channels", self.aes67_channels, target.aes67_channels);
        field!("aes67_map", self.aes67_map, target.aes67_map);
        field!("jitter_frames", self.jitter_frames, target.jitter_frames);
        field!("plan_rate", self.plan_rate, target.plan_rate);
        field!("device", self.device, target.device);
        Value::Object(diff)
    }

    /// 引擎格式是否变化（变化会重建引擎、丢弃旧图）。
    pub fn engine_changed(&self, target: &Self) -> bool {
        self.channels != target.channels
            || self.content_channels != target.content_channels
            || self.sample_rate != target.sample_rate
            || self.block_frames != target.block_frames
    }

    /// 由视图派生完整的新运行时配置（其余字段沿用 base：安全策略/收包/监督器/未加载策略）。
    pub fn to_config(&self, base: &RuntimeConfig) -> RuntimeConfig {
        let mut config = base.clone();
        config.input.multicast_addr = self.aes67_group;
        config.input.port = self.aes67_port;
        config.input.payload_type = self.aes67_payload_type;
        config.input.sample_rate = self.sample_rate;
        config.input.channels = self.aes67_channels;
        config.input.channels_map = self.aes67_map.clone();
        config.input.output_channels = self.content_channels;
        config.input.block_frames = self.block_frames;
        config.input.jitter_target_frames = self.jitter_frames;
        config.engine = EngineConfig {
            block_frames: self.block_frames,
            sample_rate: self.sample_rate,
            input_channels: self.content_channels,
            output_channels: self.channels,
        };
        config.device = DeviceConfig {
            device: DeviceId::new(self.device.clone()),
            sample_rate: self.sample_rate,
            block_frames: self.block_frames,
            input_channels: self.content_channels,
            output_channels: self.channels,
        };
        // 设备监督器的方案速率：原来跟随引擎速率（含 0 = 未显式指定）时跟随新速率。
        if config.supervisor.plan_rate == 0
            || config.supervisor.plan_rate == base.engine.sample_rate
        {
            config.supervisor.plan_rate = self.sample_rate;
        }
        config
    }
}

// ===========================================================================
// 请求补丁（channels_prepare 的字段）
// ===========================================================================

/// channels_prepare 的可选字段（未给的沿用当前值）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelsPatch {
    /// 输出通道数。
    /// 只给本字段时输入宽度跟随它（兼容旧客户端）；同时给 `content_channels` 时两者独立。
    pub channels: Option<usize>,
    /// 引擎输入宽度（plan content.in_channels / AES67 输入总线宽度）。
    pub content_channels: Option<usize>,
    /// 采样率（Hz）。
    pub sample_rate: Option<u32>,
    /// AES67 组播地址。
    pub aes67_group: Option<Ipv4Addr>,
    /// AES67 UDP 端口。
    pub aes67_port: Option<u16>,
    /// AES67 RTP 载荷类型。
    pub aes67_payload_type: Option<u8>,
    /// AES67 RTP 通道数。
    pub aes67_channels: Option<usize>,
    /// RTP 通道 -> 输出位置映射。
    pub aes67_map: Option<Vec<u16>>,
    /// 抖动目标帧数。
    pub jitter_frames: Option<usize>,
    /// 原始 SDP 文本（运行期替换 AES67 接收参数；与显式 aes67_* 字段互斥）。
    pub sdp: Option<String>,
    /// SDP 解析出的来源信息（prepare 成功后用于更新 channels_status.input_source）。
    sdp_source: Option<Aes67InputSource>,
}

fn unsigned_field(object: &Map<String, Value>, name: &str) -> Result<u64, ControlError> {
    match object.get(name) {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| bad(format!("field {name} must be a non-negative integer"))),
        Some(_) => Err(bad(format!("field {name} must be a non-negative integer"))),
        None => Err(bad(format!("missing field: {name}"))),
    }
}

fn bad(message: impl Into<String>) -> ControlError {
    ControlError::new(error::BAD_REQUEST, message)
}

impl ChannelsPatch {
    /// 允许的字段名（未知字段直接拒绝：拼错字段不能静默变成"什么都没改"）。
    const FIELDS: [&'static str; 10] = [
        "channels",
        "content_channels",
        "sample_rate",
        "aes67_group",
        "aes67_port",
        "aes67_payload_type",
        "aes67_channels",
        "aes67_map",
        "jitter_frames",
        "sdp",
    ];

    /// 解析请求对象里的可选字段；空补丁返回错误（至少改一项）。
    pub fn parse(object: &Map<String, Value>) -> Result<Self, ControlError> {
        for key in object.keys() {
            if !Self::FIELDS.contains(&key.as_str()) {
                return Err(bad(format!(
                    "unknown channels_prepare field {key:?}; supported fields are {:?}",
                    Self::FIELDS
                )));
            }
        }
        let mut patch = Self::default();
        if object.contains_key("channels") {
            let value = unsigned_field(object, "channels")?;
            patch.channels =
                Some(usize::try_from(value).map_err(|_| bad("channels does not fit in usize"))?);
        }
        if object.contains_key("content_channels") {
            let value = unsigned_field(object, "content_channels")?;
            patch.content_channels = Some(
                usize::try_from(value)
                    .map_err(|_| bad("content_channels does not fit in usize"))?,
            );
        }
        if object.contains_key("sample_rate") {
            let value = unsigned_field(object, "sample_rate")?;
            patch.sample_rate =
                Some(u32::try_from(value).map_err(|_| bad("sample_rate does not fit in u32"))?);
        }
        if object.contains_key("aes67_group") {
            let text = object
                .get("aes67_group")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("field aes67_group must be an IPv4 address string"))?;
            let group: Ipv4Addr = text.parse().map_err(|_| {
                bad(format!(
                    "field aes67_group is not an IPv4 address: {text:?}"
                ))
            })?;
            patch.aes67_group = Some(group);
        }
        if object.contains_key("aes67_port") {
            let value = unsigned_field(object, "aes67_port")?;
            patch.aes67_port =
                Some(u16::try_from(value).map_err(|_| bad("aes67_port does not fit in u16"))?);
        }
        if object.contains_key("aes67_payload_type") {
            let value = unsigned_field(object, "aes67_payload_type")?;
            patch.aes67_payload_type = Some(
                u8::try_from(value).map_err(|_| bad("aes67_payload_type does not fit in u8"))?,
            );
        }
        if object.contains_key("aes67_channels") {
            let value = unsigned_field(object, "aes67_channels")?;
            patch.aes67_channels = Some(
                usize::try_from(value).map_err(|_| bad("aes67_channels does not fit in usize"))?,
            );
        }
        if object.contains_key("aes67_map") {
            let entries = object
                .get("aes67_map")
                .and_then(Value::as_array)
                .ok_or_else(|| bad("field aes67_map must be an array of output slot indexes"))?;
            let mut map = Vec::with_capacity(entries.len());
            for entry in entries {
                let slot = entry
                    .as_u64()
                    .ok_or_else(|| bad("field aes67_map entries must be non-negative integers"))?;
                map.push(
                    u16::try_from(slot)
                        .map_err(|_| bad("field aes67_map entry does not fit in u16"))?,
                );
            }
            patch.aes67_map = Some(map);
        }
        if object.contains_key("jitter_frames") {
            let value = unsigned_field(object, "jitter_frames")?;
            patch.jitter_frames = Some(
                usize::try_from(value).map_err(|_| bad("jitter_frames does not fit in usize"))?,
            );
        }
        if object.contains_key("sdp") {
            let text = object
                .get("sdp")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("field sdp must be the raw SDP text"))?;
            patch.sdp = Some(text.to_string());
        }
        if patch.is_empty() {
            return Err(bad(format!(
                "channels_prepare needs at least one of {:?}",
                Self::FIELDS
            )));
        }
        Ok(patch)
    }

    /// 解析原始 SDP 并展开成 AES67 参数（channels_prepare 的 sdp 字段）。
    ///
    /// `sample_rate` 是目标采样率（同一补丁里显式改了速率就按目标速率校验），
    /// `input_channels` 是运行时输入宽度（SDP RTP 通道数必须 ≤ 它）。
    /// SDP 与显式 `aes67_*` 字段互斥（不静默让其中一边覆盖另一边）。
    pub fn resolve_sdp(
        &mut self,
        sample_rate: u32,
        input_channels: usize,
    ) -> Result<(), ControlError> {
        let Some(text) = self.sdp.take() else {
            return Ok(());
        };
        if self.aes67_group.is_some()
            || self.aes67_port.is_some()
            || self.aes67_payload_type.is_some()
            || self.aes67_channels.is_some()
            || self.aes67_map.is_some()
        {
            return Err(bad(
                "field sdp cannot be combined with explicit aes67_group/aes67_port/aes67_payload_type/aes67_channels/aes67_map; give only one source (the SDP is authoritative for all of them)",
            ));
        }
        let parsed = crate::sdp_config::parse_sdp_input(&text, sample_rate, input_channels)
            .map_err(|message| bad(format!("field sdp is not usable: {message}")))?;
        self.aes67_group = Some(parsed.multicast_addr);
        self.aes67_port = Some(parsed.port);
        self.aes67_payload_type = Some(parsed.payload_type);
        self.aes67_channels = Some(parsed.channels);
        self.aes67_map = Some(parsed.channels_map);
        self.sdp_source = Some(Aes67InputSource::Sdp {
            sha256: sha256_hex(text.as_bytes()),
        });
        Ok(())
    }

    /// SDP 来源（prepare 成功后控制面用它更新 channels_status.input_source）。
    pub fn sdp_input_source(&self) -> Option<&Aes67InputSource> {
        self.sdp_source.as_ref()
    }

    /// 补丁是否改动了 AES67 接收参数（用于把启动来源标成运行期手工覆盖）。
    pub fn touches_aes67_input(&self) -> bool {
        self.aes67_group.is_some()
            || self.aes67_port.is_some()
            || self.aes67_payload_type.is_some()
            || self.aes67_channels.is_some()
            || self.aes67_map.is_some()
    }

    /// 是否为空补丁。
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// 把补丁应用到当前视图，得到目标视图。
    pub fn apply(&self, current: &ChannelsView) -> ChannelsView {
        let mut target = current.clone();
        if let Some(channels) = self.channels {
            target.channels = channels;
            // 旧语义：只改输出通道时输入宽度跟随；显式 content_channels 才独立。
            if self.content_channels.is_none() {
                target.content_channels = channels;
            }
        }
        if let Some(content_channels) = self.content_channels {
            target.content_channels = content_channels;
        }
        if let Some(sample_rate) = self.sample_rate {
            target.sample_rate = sample_rate;
        }
        if let Some(group) = self.aes67_group {
            target.aes67_group = group;
        }
        if let Some(port) = self.aes67_port {
            target.aes67_port = port;
        }
        if let Some(payload_type) = self.aes67_payload_type {
            target.aes67_payload_type = payload_type;
        }
        if let Some(channels) = self.aes67_channels {
            target.aes67_channels = channels;
        }
        if let Some(map) = &self.aes67_map {
            target.aes67_map = map.clone();
        }
        if let Some(jitter) = self.jitter_frames {
            target.jitter_frames = jitter;
        }
        target
    }
}

// ===========================================================================
// 只读设备探测（prepare 用；提交后的 hw 回读在同一 trait 上做）
// ===========================================================================

/// 只读探测输出设备的当前 hw params（prepare 校验 + commit 回读校验共用）。
///
/// 生产实现读 `/proc/asound`（[`AlsaDeviceProbe`]）；测试注入假探测。探测必须是**只读**的
/// （不打开、不改设备），因此可以在控制线程做。
pub trait DeviceProbe: Send + Sync {
    /// 当前 hw params（rate/channels 都要读到）；设备不在位/读不到返回人读原因。
    fn probe(&self) -> Result<HwReadback, String>;
}

/// 生产探测：从 `/proc/asound` 读 `hw:` 设备的 hw_params 里的 rate/channels。
#[derive(Debug, Clone)]
pub struct AlsaDeviceProbe {
    /// ALSA 设备名（`hw:` 直连写法；其它写法读不到 hw_params，如实失败）。
    pub device: String,
    /// `/proc/asound` 根（测试可注入临时目录）。
    pub proc_asound: PathBuf,
}

impl AlsaDeviceProbe {
    /// 新建。
    pub fn new(device: impl Into<String>, proc_asound: PathBuf) -> Self {
        Self {
            device: device.into(),
            proc_asound,
        }
    }
}

impl DeviceProbe for AlsaDeviceProbe {
    fn probe(&self) -> Result<HwReadback, String> {
        let cards = crate::app::read_cards_text(&self.proc_asound);
        let Some(card) = crate::app::resolve_card_index(&self.device, &cards) else {
            return Err(format!(
                "ALSA device {} is not present in {}/cards (read-only probe; device absent)",
                self.device,
                self.proc_asound.display()
            ));
        };
        let pcm = crate::app::device_pcm_index(&self.device);
        let path = self
            .proc_asound
            .join(format!("card{card}"))
            .join(format!("pcm{pcm}p"))
            .join("sub0")
            .join("hw_params");
        let text = std::fs::read_to_string(&path).map_err(|err| {
            format!(
                "cannot read {} hw params ({}): {err}",
                self.device,
                path.display()
            )
        })?;
        let sample_rate = crate::app::parse_hw_params_rate(&text);
        let channels = crate::app::parse_hw_params_channels(&text);
        if sample_rate.is_none() || channels.is_none() {
            return Err(format!(
                "{} does not report hw rate/channels (hw_params is {:?}); hot reconfiguration is fail-closed without a real readback",
                path.display(),
                text.trim()
            ));
        }
        Ok(HwReadback {
            sample_rate,
            channels,
        })
    }
}

// ===========================================================================
// 块线程侧的共享追踪器（generation / last gap / last result / 在飞提交）
// ===========================================================================

/// 一次成功生效的重配记录（块线程写、控制线程/状态文件读）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedReconfigure {
    /// 幂等 token。
    pub token: String,
    /// 从静音到恢复的实测空档（毫秒）。
    pub gap_ms: u64,
    /// 生效后的目标视图。
    pub target: ChannelsView,
    /// 生效后的引擎格式。
    pub format: EngineConfig,
    /// 引擎格式是否变化（旧图被丢弃/重新 stage）。
    pub engine_changed: bool,
    /// 重配前设备是否 absent。
    pub device_was_absent: bool,
    /// 旧图是否被丢弃。
    pub graph_dropped: bool,
}

/// 热重配的跨线程追踪器：块线程在块边界执行并记录，控制线程只读（锁内拷贝）。
#[derive(Debug, Clone, Default)]
pub struct ReconfigureTracker {
    /// 已成功生效的重配代际（从 0 起；只有真正生效才 +1）。
    pub generation: u64,
    /// 最近一次重配的实测空档（毫秒；失败也记录）。
    pub last_gap_ms: Option<u64>,
    /// 最近一次重配结果（成功/失败 JSON；没有为 None）。
    pub last_result: Option<Value>,
    /// 正在块线程执行/排队的 token（None = 没有在飞提交）。
    pub in_flight_token: Option<String>,
    /// 最近一次成功生效的记录（ack 丢失时控制面据此对账）。
    pub last_applied: Option<AppliedReconfigure>,
    /// 最近一次失败的 token（ack 丢失时控制面据此对账）。
    pub last_failed_token: Option<String>,
}

/// 追踪器共享句柄（控制核心、块线程桥、状态文件共用同一份）。
pub type SharedReconfigure = Arc<Mutex<ReconfigureTracker>>;

/// 新建共享追踪器。
pub fn shared_tracker() -> SharedReconfigure {
    Arc::new(Mutex::new(ReconfigureTracker::default()))
}

/// 门占用时 cal_enter 的错误文案标签（错误码见 [`error::RECONFIGURING`]）。
pub const RECONFIGURING_LABEL: &str = "reconfiguring";

/// 重配/校准互斥门：热重配提交在块边界执行期间由控制线程持有，校准 enter 只读检查。
#[derive(Debug, Default)]
pub struct ReconfigGate {
    busy: std::sync::atomic::AtomicBool,
}

impl ReconfigGate {
    /// 新建（未占用）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 尝试占用（false = 已被占用）。
    pub fn enter(&self) -> bool {
        self.busy
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// 释放。
    pub fn leave(&self) {
        self.busy.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// 是否被占用。
    pub fn is_busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// AES67 输入配置的来源（启动时确定；channels_status 的 `input_source`）。
///
/// 只描述“启动配置是怎么来的”：SDP 文件解析（附原文 sha256）或手工 env。
/// 运行期 channels_commit 改的是 receiver 参数，不改变这个来源描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aes67InputSource {
    /// 手工 env（KHAEN_AES67_*）。
    Manual,
    /// SDP 文件（KHAEN_AES67_SDP）；sha256 是 SDP 原文的十六进制摘要。
    Sdp {
        /// SDP 原文 sha256（hex）。
        sha256: String,
    },
}

impl Aes67InputSource {
    /// 协议 JSON：`{source:"manual"|"sdp", sdp_sha256}`。
    pub fn to_json(&self) -> Value {
        match self {
            Self::Manual => json!({ "source": "manual", "sdp_sha256": Value::Null }),
            Self::Sdp { sha256 } => json!({ "source": "sdp", "sdp_sha256": sha256 }),
        }
    }
}

/// 热重配协议上下文（ControlCore 构造参数）：初始运行时配置 + 只读设备探测。
pub struct ChannelsConfig {
    /// 启动时的完整运行时配置（prepare 从它派生目标配置）。
    pub config: RuntimeConfig,
    /// 只读设备探测（prepare 校验；提交后的 hw 回读校验也用它）。
    pub probe: Arc<dyn DeviceProbe>,
    /// 启动时 AES67 输入配置的来源（SDP/手工）；旧调用方可给 None（channels_status 不编）。
    pub input_source: Option<Aes67InputSource>,
}

impl std::fmt::Debug for ChannelsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelsConfig")
            .field("device", &self.config.device.device.as_str())
            .field("engine", &self.config.engine)
            .finish_non_exhaustive()
    }
}

/// 取追踪器锁（中毒不影响：里面只有可重建的状态）。
pub fn lock_tracker(tracker: &SharedReconfigure) -> MutexGuard<'_, ReconfigureTracker> {
    tracker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ReconfigureTracker {
    /// status.reconfigure / 状态文件的增量 JSON。
    pub fn status_json(&self) -> Value {
        json!({
            "generation": self.generation,
            "last_gap_ms": self.last_gap_ms,
            "last_result": self.last_result,
            "in_flight": self.in_flight_token.is_some(),
            "in_flight_token": self.in_flight_token,
        })
    }

    /// 块线程：开始执行一笔重配（登记在飞 token）。
    pub fn begin(&mut self, token: &str) {
        self.in_flight_token = Some(token.to_string());
    }

    /// 块线程：重配成功（generation +1、记录 gap/结果/生效记录）。
    pub fn record_success(
        &mut self,
        applied: AppliedReconfigure,
        gap_ms: u64,
        result: Value,
    ) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.last_gap_ms = Some(gap_ms);
        self.last_result = Some(result);
        self.last_failed_token = None;
        self.in_flight_token = None;
        self.last_applied = Some(applied);
        self.generation
    }

    /// 块线程：重配失败（generation 不前进；记录 gap/失败结果）。
    pub fn record_failure(&mut self, token: &str, gap_ms: u64, result: Value) {
        self.last_gap_ms = Some(gap_ms);
        self.last_result = Some(result);
        self.last_failed_token = Some(token.to_string());
        self.in_flight_token = None;
    }
}

// ===========================================================================
// 失败/提示 JSON
// ===========================================================================

/// 提交失败响应（ok=false + 稳定错误码；rollback 与 gap 如实上报）。
pub fn failure_json(failure: &ReconfigureFailure, gap_ms: Option<u64>, generation: u64) -> Value {
    json!({
        "ok": false,
        "op": "channels_commit",
        "error": failure.code,
        "stage": failure.stage,
        "message": failure.message,
        "rolled_back": failure.rolled_back,
        "device_started": failure.device_started,
        "receiver_running": failure.receiver_running,
        "gap_ms": gap_ms,
        "generation": generation,
        "applied": false,
    })
}

// ===========================================================================
// 控制面状态机（草稿 / 单活动提交 / 对账）
// ===========================================================================

/// 一份已校验、已探测的提交草稿。
#[derive(Debug, Clone)]
pub(crate) struct ChannelsDraft {
    /// 幂等 token。
    pub token: String,
    /// 预计生效的代际（当前 generation + 1）。
    pub planned_generation: u64,
    /// 目标视图。
    pub target: ChannelsView,
    /// 目标完整运行时配置（提交时交给块线程）。
    pub config: RuntimeConfig,
    /// 目标 AES67 输入来源：SDP 补丁给 SDP（附 sha256）；显式改 AES67 参数给 Manual；
    /// None = 本补丁不涉及输入来源，commit 后保持原值。
    pub input_source: Option<Aes67InputSource>,
    /// 是否有变化（false = 幂等 no-op 草稿）。
    pub changed: bool,
    /// prepare 响应（同 token 重复 prepare 原样返回）。
    pub response: Value,
}

/// 提交成功后控制核心需要同步的副作用。
#[derive(Debug, Clone)]
pub struct ChannelsCommitEffect {
    /// 生效后的数据面格式。
    pub format: EngineConfig,
    /// 引擎格式是否变化（控制面要同步编译格式/丢旧图基线）。
    pub engine_changed: bool,
}

/// 对账结果（ack 丢失后从块线程追踪器恢复出的一笔结果）。
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    /// 对应 token。
    pub token: String,
    /// 可原样重放的响应。
    pub response: Value,
    /// 成功生效时的控制面副作用；失败为 None。
    pub effect: Option<ChannelsCommitEffect>,
}

/// 控制线程的 channels 状态机（草稿 + 单活动提交 + 追踪器镜像）。
pub struct ChannelsState {
    base_config: RuntimeConfig,
    current: ChannelsView,
    input_source: Option<Aes67InputSource>,
    probe: Arc<dyn DeviceProbe>,
    tracker: SharedReconfigure,
    limits: AdmissionLimits,
    pending: Option<ChannelsDraft>,
    commit_in_flight: Option<String>,
    last_committed: Option<(String, Value)>,
}

impl std::fmt::Debug for ChannelsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelsState")
            .field("current", &self.current)
            .field("has_pending", &self.pending.is_some())
            .field("commit_in_flight", &self.commit_in_flight)
            .field("generation", &self.generation())
            .field(
                "last_committed",
                &self.last_committed.as_ref().map(|(token, _)| token),
            )
            .finish_non_exhaustive()
    }
}

impl ChannelsState {
    /// 新建（初始配置来自启动时的 RuntimeConfig）。
    pub fn new(
        config: RuntimeConfig,
        input_source: Option<Aes67InputSource>,
        probe: Arc<dyn DeviceProbe>,
        tracker: SharedReconfigure,
        limits: AdmissionLimits,
    ) -> Self {
        Self {
            current: ChannelsView::from_config(&config),
            base_config: config,
            input_source,
            probe,
            tracker,
            limits,
            pending: None,
            commit_in_flight: None,
            last_committed: None,
        }
    }

    /// 当前代际（追踪器唯一来源）。
    pub fn generation(&self) -> u64 {
        lock_tracker(&self.tracker).generation
    }

    /// 探测句柄（提交命令要带给块线程做回读校验）。
    pub fn probe(&self) -> Arc<dyn DeviceProbe> {
        Arc::clone(&self.probe)
    }

    /// 是否有一笔提交在飞/结果未知。
    pub fn in_flight(&self) -> Option<&'static str> {
        if self.commit_in_flight.is_some() {
            Some(error::CHANNELS_BUSY)
        } else {
            None
        }
    }

    /// 启动时的 AES67 流表（channels_status.input_streams；只读配置，不含实时统计）。
    ///
    /// 第一项是主流（用当前视图，channels_commit 改过也如实反映）；其余是启动时
    /// `additional_inputs`（多流热重配未实现，这些流不会随 commit 改变）。
    fn input_streams_json(&self) -> Value {
        fn stream(config: &Aes67InputConfig, primary: bool) -> Value {
            json!({
                "primary": primary,
                "group": config.multicast_addr.to_string(),
                "port": config.port,
                "payload_type": config.payload_type,
                "channels": config.channels,
                "map": config.channels_map,
                "output_channels": config.output_channels,
                "jitter_frames": config.jitter_target_frames,
            })
        }
        let primary_config = Aes67InputConfig {
            multicast_addr: self.current.aes67_group,
            port: self.current.aes67_port,
            payload_type: self.current.aes67_payload_type,
            sample_rate: self.current.sample_rate,
            channels: self.current.aes67_channels,
            channels_map: self.current.aes67_map.clone(),
            output_channels: self.current.content_channels,
            block_frames: self.current.block_frames,
            jitter_target_frames: self.current.jitter_frames,
        };
        let mut streams = Vec::with_capacity(1 + self.base_config.additional_inputs.len());
        streams.push(stream(&primary_config, true));
        for input in &self.base_config.additional_inputs {
            streams.push(stream(input, false));
        }
        Value::Array(streams)
    }

    /// channels_status 响应。
    pub fn status_json(&self, snapshot: Option<&RuntimeSnapshot>) -> Value {
        let tracker = lock_tracker(&self.tracker);
        let generation = tracker.generation;
        let last_gap_ms = tracker.last_gap_ms;
        let last_result = tracker.last_result.clone();
        drop(tracker);
        let device_state = snapshot.map(|snapshot| {
            if snapshot.lifecycle.is_absent() {
                "absent"
            } else {
                "present"
            }
        });
        let mut reasons = Map::new();
        if self.commit_in_flight.is_some() {
            reasons.insert(
                "busy".to_string(),
                json!("a channels commit is in flight (or its acknowledgement was lost): a second commit is rejected with channels_busy until the block thread records the result"),
            );
        }
        if device_state == Some("absent") {
            reasons.insert(
                "device".to_string(),
                json!("output device is absent: hot reconfiguration commits are rejected with device_absent (the block loop keeps retrying the device with backoff, the process stays up)"),
            );
        }
        json!({
            "ok": true,
            "op": "channels_status",
            "generation": generation,
            "input_source": self.input_source.as_ref().map(Aes67InputSource::to_json),
            "input_streams": self.input_streams_json(),
            "current": self.current.to_json(),
            "format": {
                "block_frames": self.current.block_frames,
                "sample_rate": self.current.sample_rate,
                "channels": self.current.channels,
            },
            "device": {
                "name": self.current.device,
                "state": device_state,
                "probe": self.probe.probe().map(|hw| json!({
                    "available": true,
                    "sample_rate": hw.sample_rate,
                    "channels": hw.channels,
                })).unwrap_or_else(|message| json!({
                    "available": false,
                    "reason": message,
                })),
            },
            "pending": self.pending.as_ref().map(|draft| json!({
                "engine_token": draft.token,
                "planned_generation": draft.planned_generation,
                "changed": draft.changed,
                "target": draft.target.to_json(),
            })),
            "busy": self.commit_in_flight.is_some(),
            "last_gap_ms": last_gap_ms,
            "last_result": last_result,
            "reasons": reasons,
        })
    }

    /// channels_prepare 第二阶段：校验（准入 + 配置自洽 + 只读设备探测）并生成草稿。
    pub fn prepare(&mut self, request: &Map<String, Value>) -> Result<Value, ControlError> {
        if self.commit_in_flight.is_some() {
            return Err(ControlError::new(
                error::CHANNELS_BUSY,
                "a previous channels commit is still in flight (or its acknowledgement was lost); wait for channels_status to show the result before preparing another configuration",
            ));
        }
        let mut patch = ChannelsPatch::parse(request)?;
        let target_sample_rate = patch.sample_rate.unwrap_or(self.current.sample_rate);
        // SDP 的 RTP 通道数必须装进目标输入总线：显式 content_channels > 只改 channels 的跟随值 > 当前值。
        let target_content_channels = patch
            .content_channels
            .or(patch.channels)
            .unwrap_or(self.current.content_channels);
        patch.resolve_sdp(target_sample_rate, target_content_channels)?;
        let input_source = patch.sdp_input_source().cloned().or_else(|| {
            if patch.touches_aes67_input() {
                Some(Aes67InputSource::Manual)
            } else {
                None
            }
        });
        let target = patch.apply(&self.current);
        let planned_generation = self.generation().saturating_add(1);
        let token = channels_token(planned_generation, &target);
        // 幂等：同 token 重复 prepare 原样返回（不重复探测/分配草稿）。
        if let Some(draft) = &self.pending {
            if draft.token == token {
                return Ok(draft.response.clone());
            }
        }
        self.validate_target(&target)?;
        // 只读设备探测：设备不在位/读不到 hw params 时拒绝（fail-closed，不先改配置）。
        let hw = self
            .probe
            .probe()
            .map_err(|message| ControlError::new(error::DEVICE_UNAVAILABLE, message))?;
        let config = target.to_config(&self.base_config);
        let diff = self.current.diff(&target);
        let changed = diff
            .as_object()
            .map(|object| !object.is_empty())
            .unwrap_or(true);
        let engine_changed = self.current.engine_changed(&target);
        let graph = if engine_changed {
            json!({
                "action": "dropped",
                "reason": "the engine format (output channels/content_channels/sample rate/block) changes, so the running graph is compiled for the old format; it is dropped at commit and a matching plan must be prepared+committed again (or pass a precompiled draft through the internal command)",
            })
        } else {
            json!({
                "action": "kept",
                "reason": "engine format unchanged: the running graph stays on the same SlotEngine; only the AES67 input mapping/jitter ring is rebuilt",
            })
        };
        let response = json!({
            "ok": true,
            "op": "channels_prepare",
            "token": token.clone(),
            "engine_token": token,
            "planned_generation": planned_generation,
            "changed": changed,
            "diff": diff,
            "current": self.current.to_json(),
            "target": target.to_json(),
            "format": {
                "block_frames": target.block_frames,
                "sample_rate": target.sample_rate,
                "channels": target.channels,
            },
            "device": {
                "name": target.device,
                "state": "present",
                "probe": {
                    "available": true,
                    "sample_rate": hw.sample_rate,
                    "channels": hw.channels,
                },
            },
            "graph": graph,
            "admission": {
                "accepted": true,
                "limits": {
                    "max_channels": MAX_OUTPUT_CHANNELS,
                    "max_aes67_channels": MAX_RTP_CHANNELS,
                    "supported_sample_rates": SAMPLE_RATES_HZ,
                    "payload_type_range": [96, 127],
                    "max_lanes": self.limits.max_lanes,
                    "max_dac_channels": self.limits.max_dac_channels,
                },
            },
            "applies_to": "rt-host data plane (applied at a block boundary by channels_commit; the rt-host process/PID stays up)",
            "notes": {
                "gap": "the commit response reports the measured gap_ms from mute to unmute; there is no hard limit (the target reference is <= 2 s)",
                "rollback": "if the new configuration fails (validation is done here; device open / hw readback are checked at commit), the block loop rolls back to the previous configuration and reports rolled_back",
                "mutual_exclusion": "channels_commit is rejected with cal_busy while in-process calibration mode is active; cal_enter is rejected with reconfiguring while a commit is in flight",
            },
        });
        self.pending = Some(ChannelsDraft {
            token,
            planned_generation,
            target,
            config,
            input_source,
            changed,
            response: response.clone(),
        });
        Ok(response)
    }

    /// 目标校验（准入 + 配置自洽；全部在控制线程做，失败时什么都没碰）。
    fn validate_target(&self, target: &ChannelsView) -> Result<(), ControlError> {
        if target.channels == 0 || target.channels > MAX_OUTPUT_CHANNELS {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "channels {} is outside the admitted range 1..={MAX_OUTPUT_CHANNELS} (plan ABI output channel limit)",
                    target.channels
                ),
            ));
        }
        if usize::from(self.limits.max_dac_channels) < target.channels {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "channels {} exceeds the admission limit max_dac_channels={}",
                    target.channels, self.limits.max_dac_channels
                ),
            ));
        }
        if target.content_channels == 0 || target.content_channels > MAX_INPUT_BUS_CHANNELS {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "content_channels {} is outside 1..={MAX_INPUT_BUS_CHANNELS} (host AES67 input bus limit; plan ABI input budget is larger but the receiver bus is capped here)",
                    target.content_channels
                ),
            ));
        }
        if usize::from(self.limits.max_input_channels) < target.content_channels {
            return Err(ControlError::new(
                error::ADMISSION_REJECTED,
                format!(
                    "content_channels {} exceeds the admission limit max_input_channels={}",
                    target.content_channels, self.limits.max_input_channels
                ),
            ));
        }
        if !SAMPLE_RATES_HZ.contains(&target.sample_rate) {
            return Err(bad(format!(
                "sample_rate {} Hz is not supported; supported values are {SAMPLE_RATES_HZ:?}",
                target.sample_rate
            )));
        }
        if target.aes67_channels == 0 || target.aes67_channels > MAX_RTP_CHANNELS {
            return Err(bad(format!(
                "aes67_channels {} is outside 1..={MAX_RTP_CHANNELS} (AES67 RTP channel limit)",
                target.aes67_channels
            )));
        }
        if !(96..=127).contains(&target.aes67_payload_type) {
            return Err(bad(format!(
                "aes67_payload_type {} is outside the dynamic RTP payload range 96..=127",
                target.aes67_payload_type
            )));
        }
        if target.aes67_port == 0 {
            return Err(bad(
                "aes67_port must be 1..=65535 (0 would bind an ephemeral port)",
            ));
        }
        // IO I2b：目的地址可以是组播组，也可以是单播本端地址（SDP c= 为单播时）；
        // 只拒绝 unspecified/broadcast 这类没有具体接收端的地址。
        if target.aes67_group.is_unspecified()
            || target.aes67_group == std::net::Ipv4Addr::BROADCAST
        {
            return Err(bad(format!(
                "aes67_group {} must be a concrete IPv4 destination (multicast group or unicast local address, not 0.0.0.0/broadcast)",
                target.aes67_group
            )));
        }
        // 映射规则（与 host 的 Aes67InputConfig::validate 同一口径）：
        // 映射长度 = AES67 RTP 通道数；每个输出位置 < channels 且不重复。
        if target.aes67_map.len() != target.aes67_channels {
            return Err(bad(format!(
                "aes67_map has {} entries but aes67_channels is {}: the map length must equal the AES67 (RTP) channel count",
                target.aes67_map.len(),
                target.aes67_channels
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for &slot in &target.aes67_map {
            if usize::from(slot) >= target.content_channels {
                return Err(bad(format!(
                    "aes67_map slot {slot} is not a valid input bus slot (content_channels = {})",
                    target.content_channels
                )));
            }
            if !seen.insert(slot) {
                return Err(bad(format!(
                    "aes67_map must be injective: output slot {slot} is mapped twice",
                )));
            }
        }
        // 配置自洽（三段速率/通道/块长一致、收包槽装得下 L24 帧、监督器装配等）由 host 校验。
        let config = target.to_config(&self.base_config);
        config.validate().map_err(|err| {
            bad(format!(
                "target configuration is not self-consistent: {err} (channels={}, sample_rate={}, aes67_channels={}, map={:?})",
                target.channels, target.sample_rate, target.aes67_channels, target.aes67_map
            ))
        })
    }

    /// 取出待提交草稿（token 不符时草稿放回，可重试）。
    pub(crate) fn take_draft(&mut self, token: &str) -> Result<ChannelsDraft, ControlError> {
        let Some(draft) = self.pending.take() else {
            return Err(ControlError::new(
                error::NO_CHANNELS_DRAFT,
                "no prepared channels draft: call channels_prepare before channels_commit",
            ));
        };
        if draft.token != token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {token}",
                draft.token
            );
            self.pending = Some(draft);
            return Err(ControlError::new(error::TOKEN_INVALID, message));
        }
        Ok(draft)
    }

    /// 丢弃草稿（abort）：token 不符时草稿放回。
    pub fn abort(&mut self, token: &str) -> Result<Value, ControlError> {
        let Some(draft) = self.pending.take() else {
            return Err(ControlError::new(
                error::NO_CHANNELS_DRAFT,
                "no prepared channels draft to abort",
            ));
        };
        if draft.token != token {
            let message = format!(
                "engine_token mismatch: prepared token {}, request {token}",
                draft.token
            );
            self.pending = Some(draft);
            return Err(ControlError::new(error::TOKEN_INVALID, message));
        }
        Ok(json!({
            "ok": true,
            "op": "channels_abort",
            "aborted": true,
            "aborted_token": token,
            "note": "draft discarded; the running data-plane configuration is untouched",
        }))
    }

    /// 标记一笔提交在飞（单活动；重复提交返回 channels_busy）。
    pub fn begin_commit(&mut self, token: &str) -> Result<(), ControlError> {
        if self.commit_in_flight.is_some() {
            return Err(ControlError::new(
                error::CHANNELS_BUSY,
                "another channels commit is in flight (or its acknowledgement was lost): wait for it to finish before committing again",
            ));
        }
        self.commit_in_flight = Some(token.to_string());
        Ok(())
    }

    /// 释放"在飞"标记（提交命令通道直接报错时用；超时则保持，由追踪器对账）。
    pub fn abandon_in_flight(&mut self) {
        self.commit_in_flight = None;
    }

    /// no-op 提交（目标与当前一致）：不碰设备、不推进 generation。
    pub fn finish_noop(&mut self, draft: ChannelsDraft) -> Value {
        self.commit_in_flight = None;
        let generation = self.generation();
        let response = json!({
            "ok": true,
            "op": "channels_commit",
            "applied": false,
            "changed": false,
            "idempotent": true,
            "generation": generation,
            "gap_ms": 0,
            "format": {
                "block_frames": self.current.block_frames,
                "sample_rate": self.current.sample_rate,
                "channels": self.current.channels,
            },
            "note": "target configuration already matches the running data plane; nothing was applied and the device was not touched",
        });
        self.last_committed = Some((draft.token, response.clone()));
        if let Ok(mut tracker) = self.tracker.lock() {
            tracker.last_result = Some(json!({
                "ok": true,
                "applied": false,
                "changed": false,
                "note": "no-op channels commit (target already matched)",
            }));
        }
        response
    }

    /// 提交成功：把目标视图/配置落到控制面镜像，返回响应与控制面副作用。
    pub fn finish_success(
        &mut self,
        draft: &ChannelsDraft,
        done: &super::command::ReconfigureDone,
        gap_ms: u64,
    ) -> (Value, ChannelsCommitEffect) {
        self.commit_in_flight = None;
        self.base_config = draft.config.clone();
        self.current = draft.target.clone();
        if let Some(source) = draft.input_source.clone() {
            self.input_source = Some(source);
        }
        let generation = self.generation();
        let outcome: &ReconfigureOutcome = &done.outcome;
        let response = json!({
            "ok": true,
            "op": "channels_commit",
            "applied": true,
            "changed": draft.changed,
            "idempotent": false,
            "generation": generation,
            "gap_ms": gap_ms,
            "pid": std::process::id(),
            "format": {
                "block_frames": self.current.block_frames,
                "sample_rate": self.current.sample_rate,
                "channels": self.current.channels,
            },
            "channels": self.current.to_json(),
            "device_restarted": true,
            "pcm_reconfigured": true,
            "receiver_rebound": outcome.receiver_rebound,
            "engine_rebuilt": outcome.engine_rebuilt,
            "graph_dropped": outcome.graph_dropped,
            "device_was_absent": outcome.device_was_absent,
            "hw": {
                "sample_rate": outcome.hw.sample_rate,
                "channels": outcome.hw.channels,
            },
            "alsa_stats_reset": outcome.alsa_stats_reset,
            "input_stats_reset": outcome.input_stats_reset,
            "rolled_back": false,
            "note": "configuration applied and verified by hw readback at a block boundary; the process/PID and the block counter are unchanged; blocks resume immediately after unmute",
        });
        self.last_committed = Some((draft.token.clone(), response.clone()));
        let effect = ChannelsCommitEffect {
            format: outcome.format.clone(),
            engine_changed: outcome.engine_rebuilt,
        };
        (response, effect)
    }

    /// 提交失败：如实返回稳定码 + rollback 状态（generation 不前进）。
    pub fn finish_failure(&mut self, failure: &ReconfigureFailure, gap_ms: Option<u64>) -> Value {
        self.commit_in_flight = None;
        let generation = self.generation();
        failure_json(failure, gap_ms, generation)
    }

    /// 一笔提交成功后的控制面副作用：数据面格式镜像 + （格式变化时）清理旧图基线。
    pub fn last_committed_replay(&self, token: &str) -> Option<Value> {
        match &self.last_committed {
            Some((committed, response)) if committed == token => Some(response.clone()),
            _ => None,
        }
    }

    /// ack 丢失后的对账：块线程追踪器里出现本笔提交的结果时采用它（不重复生效）。
    ///
    /// 返回 Some = 结果已恢复（成功会带控制面副作用）；None = 仍在飞（保持 channels_busy）。
    pub fn recover_in_flight(&mut self) -> Option<ReconcileOutcome> {
        let token = self.commit_in_flight.clone()?;
        let (applied, failed_token, failed_result) = {
            let tracker = lock_tracker(&self.tracker);
            (
                tracker.last_applied.clone(),
                tracker.last_failed_token.clone(),
                tracker.last_result.clone(),
            )
        };
        if let Some(applied) = applied {
            if applied.token == token {
                let (response, effect) = self.adopt(&applied);
                return Some(ReconcileOutcome {
                    token,
                    response,
                    effect: Some(effect),
                });
            }
        }
        if failed_token.as_deref() == Some(token.as_str()) {
            self.commit_in_flight = None;
            self.pending = None;
            let generation = self.generation();
            let response = failed_result.unwrap_or_else(|| {
                json!({
                    "ok": false,
                    "op": "channels_commit",
                    "error": "not_applied",
                    "message": "the block loop reported a failed channels commit but no detailed result was recorded",
                    "applied": false,
                    "generation": generation,
                })
            });
            return Some(ReconcileOutcome {
                token,
                response,
                effect: None,
            });
        }
        None
    }

    /// 采用块线程记录的已生效结果（ack 丢失对账用）。
    fn adopt(&mut self, applied: &AppliedReconfigure) -> (Value, ChannelsCommitEffect) {
        self.commit_in_flight = None;
        self.pending = None;
        self.current = applied.target.clone();
        self.base_config = applied.target.to_config(&self.base_config);
        let generation = self.generation();
        let response = json!({
            "ok": true,
            "op": "channels_commit",
            "applied": true,
            "changed": true,
            "idempotent": true,
            "reconciled": true,
            "generation": generation,
            "gap_ms": applied.gap_ms,
            "pid": std::process::id(),
            "format": {
                "block_frames": applied.target.block_frames,
                "sample_rate": applied.target.sample_rate,
                "channels": applied.target.channels,
            },
            "channels": applied.target.to_json(),
            "device_restarted": true,
            "pcm_reconfigured": true,
            "engine_rebuilt": applied.engine_changed,
            "graph_dropped": applied.graph_dropped,
            "device_was_absent": applied.device_was_absent,
            "rolled_back": false,
            "note": "the commit acknowledgement was lost but the block loop recorded the applied configuration; the control plane reconciled to it instead of applying it again",
        });
        self.last_committed = Some((applied.token.clone(), response.clone()));
        let effect = ChannelsCommitEffect {
            format: applied.format.clone(),
            engine_changed: applied.engine_changed,
        };
        (response, effect)
    }
}

/// 幂等 token：planned generation + 目标视图的规范化 JSON 的 sha256 前 8 字节。
pub fn channels_token(planned_generation: u64, target: &ChannelsView) -> String {
    let canonical = format!("channels:{planned_generation}:{}", target.to_json());
    engine_token(planned_generation, &sha256(canonical.as_bytes()))
}
