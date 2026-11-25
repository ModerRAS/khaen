//! [NONRT] WS 控制面：WS 必需子集（W01–W22）、prepare/commit 实际 ack、
//! GetCaptureSignalRms、rate/volume/mute/fader 兼容、未知命令拒收 —— WBS T24 写域。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/compat-matrix.md 第 4 节 W01–W22（WS 兼容矩阵）、第 9 节 X10–X13（WS 未知/越界
//!   拒收）、第 2.2 节硬规则 2「不假 ack」、第 2.1 节 KEEP/ADAPT、第 12 节 Q08（未知命令 envelope）
//!   /Q11（WS 鉴权边界）；
//! - rt-stack/spec/core-v1.md 第 5.3 节 PlanLifecycle（「REST/WS 的 applied 只能在实际
//!   plan_version 已出现在至少 1 个完整 block 的 BlockDesc 且 4 worker all-ack 后返回」）、
//!   第 3.4 节 worker-ack 位图口径 ACK_MASK(expected)、第 9.2 节错误码；
//! - scratch/lead/planner/DSP-HOST-DECISION.md 第 5 节（非 RT prepare + block 边界 atomic swap +
//!   四 worker ack 后回收，不假 ack）、第 6 节（自有 REST/WS；WS 兼容状态/遥测/音量；不假 ack）、
//!   第 7 节 G-web（actual version 一致才 PASS）；
//! - 现有消费者（只读参考，字段名沿用）：web/src/camilla.rs（12 条 live 读命令 + GetConfig /
//!   SetConfigJson / SetVolume；回复按命令名取 result/value）、
//!   audio/overlay/usr/local/bin/tspi-upmix（GetCaptureSignalRms 按采集通道索引取 dB；
//!   SetFaderVolume 传 [fader_id, volume_db]；静音口径 -120 dB，缺失值按 -240 处理）。
//!
//! 本模块交付什么（可判）：
//! 1. 传输无关的协议引擎：本模块不打开 socket。T26 拥有 WebSocket 传输（监听/握手/鉴权/
//!    loopback 绑定/有界队列/超时），把每个帧喂给 WsSession::handle_frame，再把返回的 WsReply
//!    编码成 Text/Pong/Close 帧。因此 W01/W02/W03/W04/W21 的逻辑可在无网络、无板端条件下被
//!    真实 cargo test 覆盖。
//! 2. W01 帧规则：只接受文本 JSON；Binary 帧显式报错（不静默当文本）；Ping 用 Pong 原 payload
//!    回显、Pong 忽略；Close 视为连接结束；非法 UTF-8 关闭（1007）；超单帧上限关闭（1009）。
//! 3. W02/W03 请求与回复形态：请求是命令字符串（"GetState"）或单键对象（{"SetVolume":-12.0}）；
//!    多键对象/空对象/标量/非法命令名一律拒绝，不猜。回复必须是
//!    {"<命令名>":{"result":"Ok","value":...}}；错误为
//!    {"<命令名>":{"result":{"Error":"...","code":"..."}}}（Q08 冻结：同键、可继续连接、不挂起）。
//!    无法确定命令名的帧错误用 {"error":{"code":...,"message":...}}（与 REST envelope 同形，X21）。
//! 4. W05–W15 读命令：GetVersion/GetState/GetStopReason/GetConfigFilePath/GetConfigTitle/
//!    GetVolume/GetMute/GetCaptureRate/GetClippedSamples/GetProcessingLoad/GetBufferLevel/
//!    GetRateAdjust/GetConfig 全部实现，值类型与既有客户端解析一致。W14 GetRateAdjust 在 host
//!    无唯一 clock servo 读数时返回稳定错误 unsupported（`rate_adjust_available=false`），
//!    不得用 0.0 冒充测量值（F-28）。
//! 5. W16 prepare/commit 实际 ack（本任务核心）：SetConfigJson 必须 prepare（schema/字段/长度/
//!    taps/rate/role/精度/hash 校验 + 容量准入）→ commit（block 边界生效）→ 核对实际 ack：
//!    新 plan_version 出现在至少 1 个完整 block、ack_mask 等于 ACK_MASK(expected_workers)、
//!    graph_id/config_sha256/version 与 prepare 完全一致、host 未声明 rolled_back。任何一项不满足
//!    都返回错误（commit_not_acked），并触发 abort_prepared 让 host 回到自研 last-good/mute；
//!    绝不假 Ok。另提供显式 PrepareConfigJson/CommitConfig/AbortConfig 三命令（P1 ADD）把
//!    prepare 与 commit 分离暴露给 T70/校准 UI。ack 规则实现为公共函数 validate_commit_ack，
//!    T42 REST 的 apply/prepare/commit 必须复用同一函数（同一套实际 ack 判据，不出现两套口径）。
//! 6. W17/W09 音量/静音：范围 -150..=+50 dB（两端含）、NaN/Inf 拒绝、越界服务端显式错误；
//!    返回 actual dB；volume 与 mute 互不覆盖（写后 GetVolume/GetMute 读回一致）。
//! 7. W18 GetCaptureSignalRms：value 是按采集通道索引的 dB 数组，长度必须等于
//!    capture_channel_count()；每个值必须有限；静音口径固定 RMS_FLOOR_DB = -240.0 dBFS
//!    （低于 floor 的读数按 floor 上报；禁止 -inf/NaN/Inf 上线）。长度不符/非有限返回错误，
//!    不发半截数组。
//! 8. W19/W13 fader：SetFaderVolume 载荷 [fader_id:int, volume_db:float]；id 必须为非负整数，
//!    未知 id 由 host 返回 unknown_fader 明确错误（不静默忽略、不写越界）；P1 无内容上混，
//!    fader 不得用于切换上混路径（C07），也不得绕过保护/限幅（host 合同）。GetFaderVolume
//!    （P1 ADD）用于写后读回验证。
//! 9. W20/W21/W22/X10–X13 未知与非法拒收：未知命令回同键 unknown_command 且连接可继续；
//!    非 JSON/空命令/多键/类型错误/越界/未知 fader 均给稳定错误码、不 panic、不挂起；单帧上限
//!    2 MiB 配置 + 8 MiB 帧（WsLimits 可注入更小值做测试）；不实现/不伪造 GetXruns（W22：
//!    Camilla 4.x 无该命令，xrun/under/over 由 REST/计数提供），GetXruns 会作为未知命令被拒。
//!
//! 非目标 / 边界：
//! - 不打开网络端口、不做 TLS/鉴权/CSRF/会话：W23「仅 loopback 或鉴权」由 T26 传输层与 T42
//!   鉴权模型保证；本模块只处理已认证连接上的帧。
//! - 不做 DSP/图编译/内存准入本身：prepare_config_json/commit_prepared 由 T26 适配
//!   rt-host::swap::PlanRegistry（admit 对应 prepare；stage/arm/complete_swap 对应 commit；
//!   TspiSwapAckV1 对应 ack 证据）与 T13/T12/T66 的 plan/hash 校验实现；本模块只冻结对外语义
//!   与 ack 判据。
//! - 不定义校准 job 进度命令：R48/DEC 第 6 节的校准进度属 T42 REST + T70 Web 面；本任务不扩
//!   WS 命令集，避免与 T42 口径分叉（记录在 evidence/T24 的 NOT_RUN/边界）。
//! - 不碰板端/ALSA/CueMix/服务/模式；无第三方依赖（纯 std + khaen-core，不加 Cargo 依赖）。
//!
//! 与 T42 REST 的一致性（同一判据，不两套口径）：
//! - commit_not_acked / stale_plan / switch_busy / admission_rejected 等错误码由
//!   WsOpError::from_rt_status 从 core-v1 第 9.2 节 RtStatus 统一映射；T42 复用同一函数与
//!   validate_commit_ack。
//! - WS 协议版本走 GetVersion（W05：新栈 WS 协议版本，不是 Camilla 版本）；REST 版本走可选
//!   请求头 X-TSPI-API-Version（X14/Q18），两者不混用。

use core::fmt::Write as _;

use khaen_core::rt::{RtStatus, SAMPLE_RATES_HZ};

// ---------------------------------------------------------------------------
// 常量（W01–W22 / X10–X13 / core-v1 第 3.4 节、第 9.2 节）
// ---------------------------------------------------------------------------

/// 新栈 WS 协议版本（W05：客户端能力判断用；不是 CamillaDSP 版本）。
pub const WS_PROTOCOL_VERSION: &str = "tspi-ws/1";

/// 默认单帧上限（字节；配置 2 MiB 转义后可能接近 4 MiB，留余量）。
pub const DEFAULT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 配置文档上限（字节；compat-matrix X22：配置 >2MiB 必须明确拒绝，不截断后假装成功）。
pub const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;

/// 音量/静音 dB 下限（W09/W17：-150..+50，两端含）。
pub const VOLUME_MIN_DB: f64 = -150.0;

/// 音量/静音 dB 上限。
pub const VOLUME_MAX_DB: f64 = 50.0;

/// RMS 静音 floor（W18 冻结口径：dBFS 有限负值，禁止 -inf/NaN/Inf；S1 缺失值按 -240 处理）。
pub const RMS_FLOOR_DB: f64 = -240.0;

/// GetProcessingLoad 文档化口径（W12）：busy_compute_ns / t_block_ns 的固定窗口平均。
/// 引擎只拒绝非有限/负值；上界（过载）由 host 按该口径如实报告，不在此伪造。
pub const PROCESSING_LOAD_HINT: &str = "busy_compute_ns/t_block_ns (fixed-window mean)";

/// worker all-ack 期望位掩码（core-v1 第 3.4 节：ACK_MASK(expected)=(1u32<<expected)-1）。
pub const fn ack_mask_for_workers(expected_workers: u8) -> u32 {
    ((1u64 << (expected_workers as u64)) - 1) as u32
}

/// 模式回滚（A03 exit 9）的 control 层扩展 RtStatus 码。
///
/// core-v1 第 9.2 节的 101–123 已冻结：119 = plan stale（role/fs/P/G/hash 陈旧），
/// 不能拿来表示「切换失败后已回滚」。124 是 control 层（T44）明确保留的扩展码，
/// 只在模式面使用，不进入 RT 主路径；T42 REST 映射 409 rolled_back，WS 映射
/// `rolled_back` 稳定码（见 [`WsErrorCode::RolledBack`]）。
pub const RT_STATUS_MODE_ROLLED_BACK: u32 = 124;

/// 必需兼容子集（W05–W19：逐条对应兼容矩阵第 4 节的 P1 要求）。
pub const REQUIRED_SUBSET_COMMANDS: &[&str] = &[
    "GetVersion",
    "GetState",
    "GetStopReason",
    "GetConfigFilePath",
    "GetConfigTitle",
    "GetVolume",
    "GetMute",
    "GetCaptureRate",
    "GetClippedSamples",
    "GetProcessingLoad",
    "GetBufferLevel",
    "GetRateAdjust",
    "GetConfig",
    "SetConfigJson",
    "SetVolume",
    "GetCaptureSignalRms",
    "SetFaderVolume",
];

/// P1 新增（ADD，非 Camilla 原生）命令：mute 写、fader 读回、actual version 读回、
/// 显式 prepare/commit/abort 三命令（core-v1 第 5.3 节 / DEC 第 5 节的分离语义）。
pub const P1_ADDED_COMMANDS: &[&str] = &[
    "SetMute",
    "GetFaderVolume",
    "GetConfigVersion",
    "PrepareConfigJson",
    "CommitConfig",
    "AbortConfig",
];

/// 全部支持命令（必需子集 + P1 ADD）。未知命令一律 unknown_command。
pub const SUPPORTED_COMMANDS: &[&str] = &[
    "GetVersion",
    "GetState",
    "GetStopReason",
    "GetConfigFilePath",
    "GetConfigTitle",
    "GetVolume",
    "GetMute",
    "GetCaptureRate",
    "GetClippedSamples",
    "GetProcessingLoad",
    "GetBufferLevel",
    "GetRateAdjust",
    "GetConfig",
    "SetConfigJson",
    "SetVolume",
    "GetCaptureSignalRms",
    "SetFaderVolume",
    "SetMute",
    "GetFaderVolume",
    "GetConfigVersion",
    "PrepareConfigJson",
    "CommitConfig",
    "AbortConfig",
];

/// 是否支持该命令（大小写敏感、精确匹配；W02/X10）。
pub fn is_supported_command(command: &str) -> bool {
    SUPPORTED_COMMANDS.iter().any(|c| *c == command)
}

// ---------------------------------------------------------------------------
// 错误码与错误对象（W20/W21/X10–X13；compat 第 2.4 节/R50 envelope 稳定字符串码）
// ---------------------------------------------------------------------------

/// WS 稳定错误码（字符串值冻结；T70/T42 可统一映射；不泄漏内部路径/密钥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WsErrorCode {
    /// 未知命令（W20/X10）：同键回复、连接可继续。
    UnknownCommand,
    /// 非文本帧（W01）：Binary 显式报错，不静默当文本。
    InvalidFrameType,
    /// 非法 JSON / 非 JSON 帧（W21/X11）。
    InvalidJson,
    /// 请求形态非法：空对象、多键对象、空命令名、非字符串/单键对象（W02/X11）。
    InvalidRequest,
    /// 参数类型/取值非法：音量越界、fader 载荷形状错、plan_version token 错等（X12）。
    InvalidArgument,
    /// 未知 fader id（W19/X13）。
    UnknownFader,
    /// 帧/配置超上限（X22）：明确拒绝，不截断假装成功。
    PayloadTooLarge,
    /// 不支持（host 明确不支持的合法请求）。
    Unsupported,
    /// 已有一个 prepare 在途（W16 prepare/commit 串行；冲突）。
    PrepareBusy,
    /// 无 pending prepare 却 commit/abort（W16）。
    NotPrepared,
    /// prepare 阶段拒绝（schema/字段/长度/taps/rate/role/精度/hash 校验失败，W16）。
    PrepareRejected,
    /// plan 校验/结构/ABI 类拒绝（core-v1 第 9.2 节 101–112）。
    PlanRejected,
    /// 容量/内存/调度/时钟准入拒绝（core-v1 第 9.2 节 113–116）。
    AdmissionRejected,
    /// plan stale：role/fs/P/G/hash 变化未重设计（core-v1 第 9.2 节 119）。
    StalePlan,
    /// 切换忙：另一 plan 正在 commit/retire（core-v1 第 9.2 节 120）。
    SwitchBusy,
    /// quarantine 槽耗尽，拒绝新 commit（core-v1 第 9.2 节 121）。
    QuarantineExhausted,
    /// 音频设备被校准 job 独占（core-v1 第 9.2 节 117；compat 第 5.4 节 DeviceOwner）。
    DeviceBusy,
    /// 模式切换失败后已回滚（A03 exit 9；F-30：119 只表示 plan stale，回滚不得复用它）。
    RolledBack,
    /// commit 未达到实际 ack 判据（W16 核心：blocks<1、all-ack 缺失、版本/hash 不一致、
    /// host 声明 rolled_back）——绝不假 Ok。
    CommitNotAcked,
    /// 运行期 RT fault（core-v1 第 9.2 节 201–407）：control 侧如实上报，不隐藏。
    RuntimeFault,
    /// 服务端内部契约违例（host 返回值非法等；不泄漏内部路径）。
    Internal,
}

impl WsErrorCode {
    /// 稳定字符串码（线上 code 字段值；不得随版本静默改名）。
    pub const fn as_str(self) -> &'static str {
        match self {
            WsErrorCode::UnknownCommand => "unknown_command",
            WsErrorCode::InvalidFrameType => "invalid_frame_type",
            WsErrorCode::InvalidJson => "invalid_json",
            WsErrorCode::InvalidRequest => "invalid_request",
            WsErrorCode::InvalidArgument => "invalid_argument",
            WsErrorCode::UnknownFader => "unknown_fader",
            WsErrorCode::PayloadTooLarge => "payload_too_large",
            WsErrorCode::Unsupported => "unsupported",
            WsErrorCode::PrepareBusy => "prepare_busy",
            WsErrorCode::NotPrepared => "not_prepared",
            WsErrorCode::PrepareRejected => "prepare_rejected",
            WsErrorCode::PlanRejected => "plan_rejected",
            WsErrorCode::AdmissionRejected => "admission_rejected",
            WsErrorCode::StalePlan => "stale_plan",
            WsErrorCode::SwitchBusy => "switch_busy",
            WsErrorCode::QuarantineExhausted => "quarantine_exhausted",
            WsErrorCode::DeviceBusy => "device_busy",
            WsErrorCode::RolledBack => "rolled_back",
            WsErrorCode::CommitNotAcked => "commit_not_acked",
            WsErrorCode::RuntimeFault => "runtime_fault",
            WsErrorCode::Internal => "internal",
        }
    }

    /// 全部错误码（测试断言字符串唯一/非空/可反解用）。
    pub const ALL: &'static [WsErrorCode] = &[
        WsErrorCode::UnknownCommand,
        WsErrorCode::InvalidFrameType,
        WsErrorCode::InvalidJson,
        WsErrorCode::InvalidRequest,
        WsErrorCode::InvalidArgument,
        WsErrorCode::UnknownFader,
        WsErrorCode::PayloadTooLarge,
        WsErrorCode::Unsupported,
        WsErrorCode::PrepareBusy,
        WsErrorCode::NotPrepared,
        WsErrorCode::PrepareRejected,
        WsErrorCode::PlanRejected,
        WsErrorCode::AdmissionRejected,
        WsErrorCode::StalePlan,
        WsErrorCode::SwitchBusy,
        WsErrorCode::QuarantineExhausted,
        WsErrorCode::DeviceBusy,
        WsErrorCode::RolledBack,
        WsErrorCode::CommitNotAcked,
        WsErrorCode::RuntimeFault,
        WsErrorCode::Internal,
    ];

    /// 由稳定字符串码反解（T42/T70 映射用）。
    pub fn from_str(code: &str) -> Option<WsErrorCode> {
        WsErrorCode::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == code)
    }
}

impl core::fmt::Display for WsErrorCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// WS 错误码到 T42 REST 稳定码的映射（同一语义面不出现两套口径；T70 可据此统一提示）。
///
/// REST 侧码值取自 crates/control/src/rest.rs 的 CODE_* 常量（T42 实装）与
/// rt_status_to_error 的 core-v1 第 9.2 节映射；两边都从同一 RtStatus 派生，
/// 差别只在命名（见下表）。返回 None 表示该错误是 WS 传输/协议专属，REST 无等价码。
///
/// | WS 码 | REST 码（T42） | 说明 |
/// | --- | --- | --- |
/// | invalid_json | invalid_json | 请求体非 JSON |
/// | invalid_request / invalid_argument | invalid_field | 请求字段缺失/类型/取值非法 |
/// | invalid_frame_type | （无） | WS 帧类型专属（W01） |
/// | unknown_command | （无） | WS 命令专属（W20；REST 未知路径是 not_found） |
/// | unknown_fader | invalid_field | fader id 未知（WS 面概念，REST 以字段非法表达） |
/// | payload_too_large | payload_too_large | 配置/帧超上限（X22） |
/// | unsupported | unsupported | 显式拒绝（如 P1 无上混） |
/// | prepare_busy | busy | 已有 prepare 在途（R49 冲突 409 族） |
/// | not_prepared | not_prepared | 未 prepare 就 commit（R49 409） |
/// | prepare_rejected / plan_rejected | invalid_plan | plan/配置校验拒绝 |
/// | admission_rejected | admission_failed | 容量/内存/调度/时钟准入拒绝 |
/// | stale_plan | stale | plan stale（core 119） |
/// | switch_busy | switch_busy | 切图在途（core 120） |
/// | quarantine_exhausted | quarantine_exhausted | quarantine 槽耗尽（core 121） |
/// | device_busy | device_busy | 设备被校准 job 独占（core 117） |
/// | rolled_back | rolled_back | 模式切换失败已回滚（control 扩展码 124；不复用 119） |
/// | commit_not_acked / runtime_fault / internal | internal_error | 未达实际 ack / RT 故障 / 内部契约违例 |
pub fn rest_equivalent(ws: WsErrorCode) -> Option<&'static str> {
    match ws {
        WsErrorCode::InvalidJson => Some("invalid_json"),
        WsErrorCode::InvalidRequest => Some("invalid_field"),
        WsErrorCode::InvalidArgument => Some("invalid_field"),
        WsErrorCode::InvalidFrameType => None,
        WsErrorCode::UnknownCommand => None,
        WsErrorCode::UnknownFader => Some("invalid_field"),
        WsErrorCode::PayloadTooLarge => Some("payload_too_large"),
        WsErrorCode::Unsupported => Some("unsupported"),
        WsErrorCode::PrepareBusy => Some("busy"),
        WsErrorCode::NotPrepared => Some("not_prepared"),
        WsErrorCode::PrepareRejected => Some("invalid_plan"),
        WsErrorCode::PlanRejected => Some("invalid_plan"),
        WsErrorCode::AdmissionRejected => Some("admission_failed"),
        WsErrorCode::StalePlan => Some("stale"),
        WsErrorCode::SwitchBusy => Some("switch_busy"),
        WsErrorCode::QuarantineExhausted => Some("quarantine_exhausted"),
        WsErrorCode::DeviceBusy => Some("device_busy"),
        WsErrorCode::RolledBack => Some("rolled_back"),
        WsErrorCode::CommitNotAcked => Some("internal_error"),
        WsErrorCode::RuntimeFault => Some("internal_error"),
        WsErrorCode::Internal => Some("internal_error"),
    }
}

/// WS 命令错误（host trait 与本模块共用；message 由调用方保证已脱敏：不得含 token/密钥/
/// 内部绝对路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsOpError {
    code: WsErrorCode,
    message: String,
}

impl WsOpError {
    /// 构造错误（message 由调用方保证脱敏）。
    pub fn new(code: WsErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 错误码。
    pub const fn code(&self) -> WsErrorCode {
        self.code
    }

    /// 可读消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 线上错误 result 对象：{"Error":"<code>: <message>","code":"<code>"}（Q08/W03）。
    pub fn to_json(&self) -> JsonValue {
        let mut text = String::new();
        text.push_str(self.code.as_str());
        if !self.message.is_empty() {
            text.push_str(": ");
            text.push_str(&self.message);
        }
        JsonValue::Object(vec![
            ("Error".to_string(), JsonValue::String(text)),
            (
                "code".to_string(),
                JsonValue::String(self.code.as_str().to_string()),
            ),
        ])
    }

    /// 由 core-v1 第 9.2 节 RtStatus 映射到稳定 WS 错误码（T26 host 适配 / T42 复用同一映射）。
    ///
    /// 映射表（只覆盖 core-v1 第 9.2 节已冻结码段）：
    /// - 101–112（ABI/layout/format/hash/表校验）-> PlanRejected
    /// - 113–116（容量/内存/调度/时钟准入）-> AdmissionRejected
    /// - 117 设备忙 -> DeviceBusy；118 设备参数 -> InvalidArgument
    /// - 119 stale -> StalePlan；120 切换忙 -> SwitchBusy
    /// - 121 quarantine 耗尽 -> QuarantineExhausted；122 unsupported -> Unsupported
    /// - 124（control 层扩展码）-> RolledBack（模式切换失败已回滚；不复用 119）
    /// - 201–499 RT 运行期 fault -> RuntimeFault
    /// - 其余（含 0/未知）-> Internal
    pub fn from_rt_status(status: RtStatus, context: &str) -> Self {
        let code = status.code();
        let ws_code = if code == RT_STATUS_MODE_ROLLED_BACK {
            WsErrorCode::RolledBack
        } else if status == RtStatus::ERR_DEVICE_BUSY {
            WsErrorCode::DeviceBusy
        } else if status == RtStatus::ERR_DEVICE_PARAMS {
            WsErrorCode::InvalidArgument
        } else if status == RtStatus::ERR_PLAN_STALE {
            WsErrorCode::StalePlan
        } else if status == RtStatus::ERR_SWITCH_BUSY {
            WsErrorCode::SwitchBusy
        } else if status == RtStatus::ERR_QUARANTINE_EXHAUSTED {
            WsErrorCode::QuarantineExhausted
        } else if status == RtStatus::ERR_UNSUPPORTED {
            WsErrorCode::Unsupported
        } else if (101..=112).contains(&code) {
            WsErrorCode::PlanRejected
        } else if (113..=116).contains(&code) {
            WsErrorCode::AdmissionRejected
        } else if (201..=499).contains(&code) {
            WsErrorCode::RuntimeFault
        } else {
            WsErrorCode::Internal
        };
        let mut message = String::new();
        message.push_str(context);
        let _ = write!(message, " (rt_status={code})");
        Self::new(ws_code, message)
    }
}

impl core::fmt::Display for WsOpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for WsOpError {}

fn internal_error(message: impl Into<String>) -> WsOpError {
    WsOpError::new(WsErrorCode::Internal, message)
}

fn invalid_argument(message: impl Into<String>) -> WsOpError {
    WsOpError::new(WsErrorCode::InvalidArgument, message)
}

fn invalid_request(message: impl Into<String>) -> WsOpError {
    WsOpError::new(WsErrorCode::InvalidRequest, message)
}

fn commit_not_acked(message: impl Into<String>) -> WsOpError {
    WsOpError::new(WsErrorCode::CommitNotAcked, message)
}

// ---------------------------------------------------------------------------
// W06/W07 状态与停止原因（枚举冻结；Running 只在真实出流后由 host 返回）
// ---------------------------------------------------------------------------

/// GetState 状态枚举（W06：至少 Running/Paused/Inactive/Starting/Error；不得乐观置 Running）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WsRunState {
    /// 未启动/已停止。
    Inactive,
    /// 启动中（设备协商/epoch0 建立）。
    Starting,
    /// 真实出流中（host 必须以实际出流为准，禁止乐观）。
    Running,
    /// 暂停（保留 epoch/设备，不开新块）。
    Paused,
    /// 错误（fault/设备丢失等；原因见 GetStopReason）。
    Error,
}

impl WsRunState {
    /// 线上字符串（与既有客户端 "Running" 判定一致）。
    pub const fn as_str(self) -> &'static str {
        match self {
            WsRunState::Inactive => "Inactive",
            WsRunState::Starting => "Starting",
            WsRunState::Running => "Running",
            WsRunState::Paused => "Paused",
            WsRunState::Error => "Error",
        }
    }

    /// 全部状态。
    pub const ALL: &'static [WsRunState] = &[
        WsRunState::Inactive,
        WsRunState::Starting,
        WsRunState::Running,
        WsRunState::Paused,
        WsRunState::Error,
    ];

    /// 由线上字符串反解。
    pub fn from_str(state: &str) -> Option<WsRunState> {
        WsRunState::ALL
            .iter()
            .copied()
            .find(|s| s.as_str() == state)
    }
}

/// GetStopReason 稳定原因码（W07；对齐 CamillaDSP 4.x 语义，便于既有客户端兼容）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WsStopReason {
    /// 无停止原因（正常运行/未停）。
    None,
    /// 正常完成/用户正常停止（非错误）。
    Done,
    /// 采集错误。
    CaptureError,
    /// 播放错误。
    PlaybackError,
    /// 采集格式变化（需新 epoch 重准入）。
    CaptureFormatChange,
    /// 播放格式变化（需新 epoch 重准入）。
    PlaybackFormatChange,
}

impl WsStopReason {
    /// 线上字符串（None 走 JSON null，不输出字符串）。
    pub const fn as_str(self) -> &'static str {
        match self {
            WsStopReason::None => "None",
            WsStopReason::Done => "Done",
            WsStopReason::CaptureError => "CaptureError",
            WsStopReason::PlaybackError => "PlaybackError",
            WsStopReason::CaptureFormatChange => "CaptureFormatChange",
            WsStopReason::PlaybackFormatChange => "PlaybackFormatChange",
        }
    }

    /// 是否为错误原因（W07：正常停止不得报错误原因）。
    pub const fn is_error(self) -> bool {
        matches!(
            self,
            WsStopReason::CaptureError
                | WsStopReason::PlaybackError
                | WsStopReason::CaptureFormatChange
                | WsStopReason::PlaybackFormatChange
        )
    }

    /// 全部原因。
    pub const ALL: &'static [WsStopReason] = &[
        WsStopReason::None,
        WsStopReason::Done,
        WsStopReason::CaptureError,
        WsStopReason::PlaybackError,
        WsStopReason::CaptureFormatChange,
        WsStopReason::PlaybackFormatChange,
    ];

    /// 由线上字符串反解（"None" 也接受）。
    pub fn from_str(reason: &str) -> Option<WsStopReason> {
        WsStopReason::ALL
            .iter()
            .copied()
            .find(|r| r.as_str() == reason)
    }
}

// ---------------------------------------------------------------------------
// 最小 JSON 值/解析/编码（W02/W03/W21；纯 std，无 serde 依赖）
// ---------------------------------------------------------------------------

/// JSON 值。
///
/// 解析器把所有数字统一解析为 Number(f64)（JSON 无语义区分整/浮点）；编码侧另提供
/// Int/UInt 变体以无损输出 u64 计数（如 GetClippedSamples）。
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// null。
    Null,
    /// true/false。
    Bool(bool),
    /// JSON 数字（解析结果；编码时必须是有限值）。
    Number(f64),
    /// 有符号整数（编码用）。
    Int(i64),
    /// 无符号整数（编码用；u64 计数保精度）。
    UInt(u64),
    /// 字符串。
    String(String),
    /// 数组。
    Array(Vec<JsonValue>),
    /// 对象（保持插入顺序；可含重复键，供严格请求解码检测）。
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    /// 是否 null。
    pub fn is_null(&self) -> bool {
        matches!(self, JsonValue::Null)
    }

    /// 取 bool。
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// 取 f64（Number/Int/UInt）。
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            JsonValue::Number(v) => Some(*v),
            JsonValue::Int(v) => Some(*v as f64),
            JsonValue::UInt(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// 取 i64（要求整数值且落在 f64 精确整数范围/整型变体）。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            JsonValue::Int(v) => Some(*v),
            JsonValue::UInt(v) => i64::try_from(*v).ok(),
            JsonValue::Number(v) => {
                if v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_992.0 {
                    Some(*v as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 取 u64（要求非负整数）。
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            JsonValue::UInt(v) => Some(*v),
            JsonValue::Int(v) => u64::try_from(*v).ok(),
            JsonValue::Number(v) => {
                if v.is_finite() && v.fract() == 0.0 && *v >= 0.0 && *v <= 9_007_199_254_740_992.0 {
                    Some(*v as u64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 取字符串。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// 取数组。
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(items) => Some(items),
            _ => None,
        }
    }

    /// 取对象键值对。
    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            JsonValue::Object(pairs) => Some(pairs),
            _ => None,
        }
    }

    /// 对象按 key 取第一个匹配值。
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        self.as_object()
            .and_then(|pairs| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v))
    }
}

/// 构造 JSON 对象（保持给定顺序）。
pub fn json_object(pairs: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// JSON 解析错误（W21：不 panic，给偏移与原因）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonError {
    /// 出错字节偏移。
    pub offset: usize,
    /// 稳定原因文本。
    pub message: &'static str,
}

impl JsonError {
    const fn new(offset: usize, message: &'static str) -> Self {
        Self { offset, message }
    }
}

impl core::fmt::Display for JsonError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for JsonError {}

/// JSON 编码错误（仅非有限数字会触发；引擎任何路径都不得把 NaN/Inf 上线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonEncodeError {
    /// 稳定原因文本。
    pub message: &'static str,
}

impl JsonEncodeError {
    const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl core::fmt::Display for JsonEncodeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for JsonEncodeError {}

/// JSON 最大嵌套深度（防恶意深嵌套导致栈溢出；超限按 invalid_json 拒绝，不 panic）。
pub const JSON_MAX_DEPTH: u32 = 64;

/// 解析 JSON 文本（严格 RFC 8259 语法：禁止注释/尾逗号/前导零/NaN/Inf；超范围数字拒绝）。
pub fn json_parse(text: &str) -> Result<JsonValue, JsonError> {
    let mut parser = JsonParser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(JsonError::new(parser.pos, "trailing characters"));
    }
    Ok(value)
}

struct JsonParser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    depth: u32,
}

impl<'a> JsonParser<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\n' || b == b'\r' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonValue, JsonError> {
        self.skip_ws();
        match self.peek() {
            None => Err(JsonError::new(self.pos, "unexpected end of input")),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(JsonValue::String(self.parse_string()?)),
            Some(b't') => {
                self.expect_literal(b"true")?;
                Ok(JsonValue::Bool(true))
            }
            Some(b'f') => {
                self.expect_literal(b"false")?;
                Ok(JsonValue::Bool(false))
            }
            Some(b'n') => {
                self.expect_literal(b"null")?;
                Ok(JsonValue::Null)
            }
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(JsonError::new(self.pos, "unexpected character")),
        }
    }

    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), JsonError> {
        if self.bytes.len() < self.pos + literal.len()
            || &self.bytes[self.pos..self.pos + literal.len()] != literal
        {
            return Err(JsonError::new(self.pos, "invalid literal"));
        }
        self.pos += literal.len();
        Ok(())
    }

    fn parse_object(&mut self) -> Result<JsonValue, JsonError> {
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH {
            return Err(JsonError::new(self.pos, "nesting too deep"));
        }
        self.pos += 1;
        self.skip_ws();
        let mut pairs: Vec<(String, JsonValue)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(JsonError::new(self.pos, "expected object key string"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(JsonError::new(self.pos, "expected colon"));
            }
            self.pos += 1;
            let value = self.parse_value()?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(JsonError::new(self.pos, "expected comma or closing brace")),
            }
        }
        self.depth -= 1;
        Ok(JsonValue::Object(pairs))
    }

    fn parse_array(&mut self) -> Result<JsonValue, JsonError> {
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH {
            return Err(JsonError::new(self.pos, "nesting too deep"));
        }
        self.pos += 1;
        self.skip_ws();
        let mut items: Vec<JsonValue> = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            let value = self.parse_value()?;
            items.push(value);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => {
                    return Err(JsonError::new(
                        self.pos,
                        "expected comma or closing bracket",
                    ))
                }
            }
        }
        self.depth -= 1;
        Ok(JsonValue::Array(items))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        if self.peek() != Some(b'"') {
            return Err(JsonError::new(self.pos, "expected string"));
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let b = match self.peek() {
                Some(b) => b,
                None => return Err(JsonError::new(self.pos, "unterminated string")),
            };
            if b == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if b == b'\\' {
                self.pos += 1;
                let esc = match self.peek() {
                    Some(e) => e,
                    None => return Err(JsonError::new(self.pos, "unterminated escape")),
                };
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hi = self.parse_hex4()?;
                        let code = if (0xD800..=0xDBFF).contains(&hi) {
                            if self.peek() != Some(b'\\') {
                                return Err(JsonError::new(self.pos, "lone high surrogate"));
                            }
                            self.pos += 1;
                            if self.peek() != Some(b'u') {
                                return Err(JsonError::new(self.pos, "expected low surrogate"));
                            }
                            self.pos += 1;
                            let lo = self.parse_hex4()?;
                            if !(0xDC00..=0xDFFF).contains(&lo) {
                                return Err(JsonError::new(self.pos, "invalid low surrogate"));
                            }
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else if (0xDC00..=0xDFFF).contains(&hi) {
                            return Err(JsonError::new(self.pos, "lone low surrogate"));
                        } else {
                            hi
                        };
                        match char::from_u32(code) {
                            Some(ch) => out.push(ch),
                            None => return Err(JsonError::new(self.pos, "invalid unicode escape")),
                        }
                    }
                    _ => return Err(JsonError::new(self.pos, "invalid escape")),
                }
                continue;
            }
            if b < 0x20 {
                return Err(JsonError::new(self.pos, "control character in string"));
            }
            if b < 0x80 {
                out.push(b as char);
                self.pos += 1;
                continue;
            }
            let rest = &self.text[self.pos..];
            let ch = match rest.chars().next() {
                Some(ch) => ch,
                None => return Err(JsonError::new(self.pos, "invalid utf-8")),
            };
            out.push(ch);
            self.pos += ch.len_utf8();
        }
        // 不可达（循环里 return），保留 start 供偏移诊断。
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(JsonError::new(self.pos, "truncated unicode escape"));
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self.bytes[self.pos];
            self.pos += 1;
            let digit = match b {
                b'0'..=b'9' => (b - b'0') as u32,
                b'a'..=b'f' => (b - b'a' + 10) as u32,
                b'A'..=b'F' => (b - b'A' + 10) as u32,
                _ => return Err(JsonError::new(self.pos - 1, "invalid hex digit")),
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<JsonValue, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(JsonError::new(self.pos, "invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(JsonError::new(self.pos, "invalid fraction"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(JsonError::new(self.pos, "invalid exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let literal = &self.text[start..self.pos];
        let value: f64 = match literal.parse() {
            Ok(v) => v,
            Err(_) => return Err(JsonError::new(start, "invalid number")),
        };
        if !value.is_finite() {
            return Err(JsonError::new(start, "number out of range"));
        }
        Ok(JsonValue::Number(value))
    }
}

/// 编码 JSON（有限数字；字符串完整转义）。
pub fn json_encode(value: &JsonValue) -> Result<String, JsonEncodeError> {
    let mut out = String::new();
    push_json_value(&mut out, value)?;
    Ok(out)
}

fn push_json_value(out: &mut String, value: &JsonValue) -> Result<(), JsonEncodeError> {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(true) => out.push_str("true"),
        JsonValue::Bool(false) => out.push_str("false"),
        JsonValue::Number(v) => push_json_number(out, *v)?,
        JsonValue::Int(v) => {
            let _ = write!(out, "{v}");
        }
        JsonValue::UInt(v) => {
            let _ = write!(out, "{v}");
        }
        JsonValue::String(s) => push_json_string(out, s),
        JsonValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_value(out, item)?;
            }
            out.push(']');
        }
        JsonValue::Object(pairs) => {
            out.push('{');
            for (i, (key, item)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_string(out, key);
                out.push(':');
                push_json_value(out, item)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn push_json_number(out: &mut String, value: f64) -> Result<(), JsonEncodeError> {
    if !value.is_finite() {
        return Err(JsonEncodeError::new("non-finite number"));
    }
    if value == 0.0 {
        out.push('0');
        return Ok(());
    }
    let magnitude = value.abs();
    if value.fract() == 0.0 && magnitude <= 9_007_199_254_740_992.0 {
        let _ = write!(out, "{}", value as i64);
    } else if (1e-6..1e16).contains(&magnitude) {
        let _ = write!(out, "{value}");
    } else {
        let _ = write!(out, "{value:e}");
    }
    Ok(())
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// 请求解码（W02/W21/X11）：字符串或单键对象；多键/空对象/标量一律拒绝，不猜
// ---------------------------------------------------------------------------

/// 已解码的 WS 请求。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WsRequest<'a> {
    /// 命令名（大小写敏感、精确匹配）。
    pub command: &'a str,
    /// 载荷（字符串形态命令为 None；对象形态为该 key 的 value）。
    pub payload: Option<&'a JsonValue>,
}

static JSON_NULL: JsonValue = JsonValue::Null;

impl<'a> WsRequest<'a> {
    /// 载荷；无载荷返回 JSON null 引用。
    pub fn payload_or_null(&self) -> &'a JsonValue {
        match self.payload {
            Some(value) => value,
            None => &JSON_NULL,
        }
    }
}

/// 命令名字符集：1..=64 个 ASCII 字母/数字（Camilla 风格 CamelCase；防止奇怪 key）。
pub fn is_valid_command_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    name.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// 解码请求（W02：字符串命令或单键对象；对象多键视为非法命令，拒绝不猜）。
pub fn decode_request(value: &JsonValue) -> Result<WsRequest<'_>, WsOpError> {
    match value {
        JsonValue::String(command) => {
            if !is_valid_command_name(command) {
                return Err(invalid_request("invalid command name"));
            }
            Ok(WsRequest {
                command: command.as_str(),
                payload: None,
            })
        }
        JsonValue::Object(pairs) => {
            if pairs.is_empty() {
                return Err(invalid_request(
                    "empty object: expected exactly one command key",
                ));
            }
            if pairs.len() > 1 {
                return Err(invalid_request(
                    "multi-key object is not a command (refusing to guess)",
                ));
            }
            let (command, payload) = &pairs[0];
            if !is_valid_command_name(command) {
                return Err(invalid_request("invalid command name"));
            }
            Ok(WsRequest {
                command: command.as_str(),
                payload: Some(payload),
            })
        }
        _ => Err(invalid_request(
            "request must be a command string or a single-key object",
        )),
    }
}

// ---------------------------------------------------------------------------
// W16 prepare/commit 数据结构与实际 ack 判据
// （core-v1 第 5.3 节：applied 只能在实际 plan_version 出现在至少 1 个完整 block
//   且 all-ack 后返回；DEC 第 5 节：prepare/commit 分离、不假 ack）
// ---------------------------------------------------------------------------

/// prepare 成功后的凭据（host 在 prepare 阶段已完成 schema/字段/长度/taps/rate/role/精度/hash
/// 校验与容量/双图/ring/栈预算准入，对应 core-v1 PlanValidated/PlanAdmitted）。
///
/// 字段是**对外可审计的实际值**：commit 之后 WS 返回给客户端的 plan_version/graph_id/
/// config_sha256 必须与本结构完全一致（不允许 prepare 一个版本、commit 另一个版本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedConfig {
    /// 已准入的新 plan 版本（进程内单调 u32，core-v1 第 3.4 节；非 0）。
    pub plan_version: u32,
    /// 规范化 graph_id 的 SHA-256 十六进制（64 位小写 hex；用于 stale/hash 校验与 actual ack）。
    pub graph_id_hex: String,
    /// 输入配置文档的 SHA-256 十六进制（64 位小写 hex；证明 commit 的正是 prepare 的那份）。
    pub config_sha256_hex: String,
    /// 期望 worker 数（生产 4、2 核降级 2；ACK_MASK(expected) 口径见 core-v1 第 3.4 节）。
    pub expected_workers: u8,
    /// 准入容量报告（预分配/双图/ring/stack 字节；供审计/证据，不参与 ack 判定）。
    pub admitted_bytes: u64,
    /// 准入备注（短文本；不得含内部绝对路径/密钥）。
    pub admission_note: String,
}

impl PreparedConfig {
    /// 校验 host 返回的 prepare 凭据形状（host 合同违例返回 internal，不假 ack）。
    pub fn validate(&self) -> Result<(), WsOpError> {
        if self.plan_version == 0 {
            return Err(internal_error("prepared plan_version must be non-zero"));
        }
        if !matches!(self.expected_workers, 2 | 4) {
            return Err(internal_error("prepared expected_workers must be 2 or 4"));
        }
        if !is_sha256_hex(&self.graph_id_hex) {
            return Err(internal_error(
                "prepared graph_id must be 64 lowercase hex chars",
            ));
        }
        if !is_sha256_hex(&self.config_sha256_hex) {
            return Err(internal_error(
                "prepared config_sha256 must be 64 lowercase hex chars",
            ));
        }
        if self.admission_note.len() > 4096 {
            return Err(internal_error("prepared admission_note too long"));
        }
        Ok(())
    }
}

/// commit 后 host 报告的实际生效证据（T26 由 rt-host::swap::PlanRegistry + TspiSwapAckV1 +
/// BlockDesc 统计构造；WS 层用 validate_commit_ack 核对后才允许回 Ok）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAck {
    /// 实际生效的新 plan 版本（必须等于 prepare 的 plan_version）。
    pub plan_version: u32,
    /// commit 前的 last-good 版本（首个 plan 为 0；必须与 plan_version 不同）。
    pub previous_plan_version: u32,
    /// 实际生效图的 graph_id（必须等于 prepare 的 graph_id）。
    pub graph_id_hex: String,
    /// 实际生效配置的 SHA-256（必须等于 prepare 的 config_sha256）。
    pub config_sha256_hex: String,
    /// 实际 expected worker 数（必须等于 prepare 的 expected_workers）。
    pub expected_workers: u8,
    /// 已完整发出且 BlockDesc.plan_version 等于新版本的 block 数（必须 >= 1）。
    pub blocks_committed: u64,
    /// 那些 block 上 4 worker（或 2 核降级的 2 worker）all-ack 位图（必须等于 ACK_MASK(expected)）。
    pub ack_mask: u32,
    /// 新版本首次出现的 block 序号（审计用）。
    pub switched_at_block: u64,
    /// host 是否实际走了 rolled_back 路径（成功 ack 必须为 false）。
    pub rolled_back: bool,
}

/// 当前实际生效 plan 的只读摘要（GetConfigVersion / actual version 读回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualPlan {
    /// 当前 plan_version（0 表示尚未 commit 任何 plan）。
    pub plan_version: u32,
    /// 当前 graph_id（plan_version 为 0 时必须为空串）。
    pub graph_id_hex: String,
    /// 当前配置 SHA-256（plan_version 为 0 时必须为空串）。
    pub config_sha256_hex: String,
}

impl ActualPlan {
    /// 无 plan（未启动/未 commit）。
    pub fn none() -> Self {
        Self {
            plan_version: 0,
            graph_id_hex: String::new(),
            config_sha256_hex: String::new(),
        }
    }

    /// 校验 host 返回值形状（host 合同违例返回 internal）。
    pub fn validate(&self) -> Result<(), WsOpError> {
        if self.plan_version == 0 {
            if !self.graph_id_hex.is_empty() || !self.config_sha256_hex.is_empty() {
                return Err(internal_error(
                    "actual plan_version 0 must have empty graph/config hashes",
                ));
            }
            return Ok(());
        }
        if !is_sha256_hex(&self.graph_id_hex) {
            return Err(internal_error(
                "actual graph_id must be 64 lowercase hex chars",
            ));
        }
        if !is_sha256_hex(&self.config_sha256_hex) {
            return Err(internal_error(
                "actual config_sha256 must be 64 lowercase hex chars",
            ));
        }
        Ok(())
    }
}

/// 是否 64 位小写十六进制（SHA-256 十六进制口径）。
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 期望 all-ack 位图（只接受生产 4 worker / 2 核降级 2 worker；其余为 host 合同违例）。
pub fn required_ack_mask(expected_workers: u8) -> Result<u32, WsOpError> {
    match expected_workers {
        2 | 4 => Ok(ack_mask_for_workers(expected_workers)),
        other => Err(internal_error(format!(
            "expected_workers must be 2 or 4, got {other}"
        ))),
    }
}

/// **W16 实际 ack 判据（唯一实现；T42 REST 必须复用本函数，不出现两套口径）**。
///
/// 全部条件必须同时满足：
/// 1. prepare 凭据本身合法（plan_version/hex/expected_workers）；
/// 2. host 未声明 rolled_back；
/// 3. ack.plan_version 等于 prepared.plan_version（不允许偷换版本）；
/// 4. previous_plan_version 与 plan_version 不同（版本必须推进）；
/// 5. ack.graph_id 等于 prepared.graph_id（不接受异图）；
/// 6. ack.config_sha256 等于 prepared.config_sha256（不接受异配置）；
/// 7. ack.expected_workers 等于 prepared.expected_workers；
/// 8. blocks_committed >= 1（core-v1 第 5.3 节：至少 1 个完整 block 的 BlockDesc 带新版本）；
/// 9. ack_mask 等于 ACK_MASK(expected_workers)（worker all-ack 到齐，不多不少）。
///
/// 任一失败都返回 commit_not_acked；prepare 凭据形状/hex 问题由 validate 返回 internal。
pub fn validate_commit_ack(prepared: &PreparedConfig, ack: &CommitAck) -> Result<(), WsOpError> {
    prepared.validate()?;
    if ack.rolled_back {
        return Err(commit_not_acked(
            "host reported rollback instead of applied plan",
        ));
    }
    if ack.plan_version != prepared.plan_version {
        return Err(commit_not_acked(format!(
            "plan_version mismatch: ack {} vs prepared {}",
            ack.plan_version, prepared.plan_version
        )));
    }
    if ack.previous_plan_version == ack.plan_version {
        return Err(commit_not_acked("plan_version did not advance"));
    }
    if ack.graph_id_hex != prepared.graph_id_hex {
        return Err(commit_not_acked(
            "graph_id mismatch between prepare and commit",
        ));
    }
    if ack.config_sha256_hex != prepared.config_sha256_hex {
        return Err(commit_not_acked(
            "config_sha256 mismatch between prepare and commit",
        ));
    }
    if ack.expected_workers != prepared.expected_workers {
        return Err(commit_not_acked(
            "expected_workers mismatch between prepare and commit",
        ));
    }
    let required = required_ack_mask(ack.expected_workers)?;
    if ack.blocks_committed == 0 {
        return Err(commit_not_acked(
            "no complete block carried the new plan_version",
        ));
    }
    if ack.ack_mask != required {
        return Err(commit_not_acked(format!(
            "worker all-ack incomplete: ack_mask={:#x}, required={:#x}",
            ack.ack_mask, required
        )));
    }
    Ok(())
}

/// prepare 回复 value（PrepareConfigJson 与审计用）。
pub fn prepare_ack_json(prepared: &PreparedConfig) -> JsonValue {
    json_object(vec![
        ("prepared", JsonValue::Bool(true)),
        (
            "plan_version",
            JsonValue::UInt(prepared.plan_version as u64),
        ),
        ("graph_id", JsonValue::String(prepared.graph_id_hex.clone())),
        (
            "config_sha256",
            JsonValue::String(prepared.config_sha256_hex.clone()),
        ),
        (
            "expected_workers",
            JsonValue::UInt(prepared.expected_workers as u64),
        ),
        ("admitted_bytes", JsonValue::UInt(prepared.admitted_bytes)),
        (
            "admission_note",
            JsonValue::String(prepared.admission_note.clone()),
        ),
    ])
}

/// commit 实际 ack 回复 value（SetConfigJson/CommitConfig；全部为实际值）。
pub fn commit_ack_json(ack: &CommitAck) -> JsonValue {
    json_object(vec![
        ("applied", JsonValue::Bool(true)),
        ("plan_version", JsonValue::UInt(ack.plan_version as u64)),
        (
            "previous_plan_version",
            JsonValue::UInt(ack.previous_plan_version as u64),
        ),
        ("graph_id", JsonValue::String(ack.graph_id_hex.clone())),
        (
            "config_sha256",
            JsonValue::String(ack.config_sha256_hex.clone()),
        ),
        (
            "expected_workers",
            JsonValue::UInt(ack.expected_workers as u64),
        ),
        ("blocks_committed", JsonValue::UInt(ack.blocks_committed)),
        ("ack_mask", JsonValue::UInt(ack.ack_mask as u64)),
        ("switched_at_block", JsonValue::UInt(ack.switched_at_block)),
    ])
}

/// actual version 回复 value（GetConfigVersion）。
pub fn actual_plan_json(actual: &ActualPlan) -> JsonValue {
    json_object(vec![
        ("plan_version", JsonValue::UInt(actual.plan_version as u64)),
        ("graph_id", JsonValue::String(actual.graph_id_hex.clone())),
        (
            "config_sha256",
            JsonValue::String(actual.config_sha256_hex.clone()),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Host 侧控制面接口（T26 适配实现；本模块不直接依赖 rt-host，保持 compat 到 core 的依赖方向）
// ---------------------------------------------------------------------------

/// WS 控制面 host 接口（T26 用新 host/engine/swap/clock 实现；tests/T24 用 FakePlane 实现）。
///
/// 实现约束（不满足时 WS 层以 internal/commit_not_acked 拒绝，不假 Ok）：
/// - 读方法返回**实际值**（actual hwparams/servo/计数），不是配置意图值；
/// - Running 只在真实出流后返回（W06）；正常停止不得报错误原因（W07）；
/// - volume/mute 互不覆盖，写后读回一致（W09/W17）；范围 -150 到 +50；
/// - RMS 单位 dBFS、长度等于 capture_channel_count()、值有限（W18；静音用 RMS_FLOOR_DB）；
/// - fader 只接受已分配 id，未知 id 返回 unknown_fader（W19/X13）；不得用 fader 绕保护/切上混；
/// - prepare_config_json 完成校验与准入但**不得改音频输出**；commit_prepared 在 block 边界生效，
///   返回前必须至少发出 1 个带新 plan_version 的完整 block 且 worker all-ack（core-v1 第 5.3 节）；
///   失败或校验不通过必须回自研 last-good 或 mute（DEC 第 5 节），不得启动 Camilla 兜底；
/// - abort_prepared 必须把「已 prepare 未生效」或「commit 未通过实际 ack」的 plan 安全丢弃
///   （PlanAborted/PlanRolledBack），保留 last-good。
pub trait WsControlPlane {
    /// W05：WS 协议版本（默认新栈常量；host 不可借此冒充 Camilla 版本）。
    fn protocol_version(&self) -> String {
        WS_PROTOCOL_VERSION.to_string()
    }

    /// W06：运行状态（不得乐观置 Running）。
    fn run_state(&self) -> WsRunState;

    /// W07：停止原因（正常停止用 None/Done，不得报错误原因）。
    fn stop_reason(&self) -> WsStopReason;

    /// W08：当前 graph/manifest 路径（逻辑标识即可；不得暴露无关敏感文件）。
    fn config_path(&self) -> String;

    /// W08：当前配置标题。
    fn config_title(&self) -> String;

    /// W09：实际音量 dB。
    fn volume_db(&self) -> f64;

    /// W17：写音量并返回**实际** dB（越界/不可用必须返回 Err，不得假成功）。
    fn set_volume_db(&mut self, db: f64) -> Result<f64, WsOpError>;

    /// W09：实际静音状态。
    fn mute(&self) -> bool;

    /// 写 mute（P1 ADD 写路径；幂等；不得改变 volume）。
    fn set_mute(&mut self, mute: bool) -> Result<bool, WsOpError>;

    /// W10：实际输入采样率（P1 actual 48k/96k；0 表示未运行）。
    fn capture_rate_hz(&self) -> u32;

    /// 采集通道数（W18 RMS 数组长度口径）。
    fn capture_channel_count(&self) -> u32;

    /// W11：累计削波样本（单调；重启归零由 REST 标 epoch）。
    fn clipped_samples(&self) -> u64;

    /// W12：有界处理负载（口径见 PROCESSING_LOAD_HINT）。
    fn processing_load(&self) -> f64;

    /// W13：播放队列水位（帧）。
    fn buffer_level_frames(&self) -> i64;

    /// W14：唯一 clock servo 的调整量（ppm）。
    ///
    /// `rate_adjust_available()` 为 false 时本值不得被读取、更不得用 0 冒充；
    /// handle_frame 会返回稳定错误（unsupported），而不是伪造 0 ppm。
    fn rate_adjust_ppm(&self) -> f64;

    /// W14：是否真有唯一 clock servo 的读数（默认 true = 旧 host 兼容）。
    /// 未接线/未测量时返回 false -> GetRateAdjust 显式不可用（F-28：不恒 0）。
    fn rate_adjust_available(&self) -> bool {
        true
    }

    /// W15：当前 graph/manifest 文档（新栈自有 schema，带版本/hash；不含密钥）。
    fn config_document(&self) -> String;

    /// W18：按采集通道索引的 RMS（dBFS；缺失/静音用 RMS_FLOOR_DB，禁止 NaN/Inf）。
    fn capture_signal_rms_db(&self) -> Vec<f64>;

    /// W19：已分配 fader 的当前 dB（未分配返回 None）。
    fn fader_volume_db(&self, fader_id: i32) -> Option<f64>;

    /// W19：写 fader dB 并返回实际值（未知 id 返回 unknown_fader；不得绕保护/限幅）。
    fn set_fader_volume_db(&mut self, fader_id: i32, db: f64) -> Result<f64, WsOpError>;

    /// 当前实际生效 plan（GetConfigVersion / actual version 读回）。
    fn actual_plan(&self) -> ActualPlan;

    /// W16 prepare：校验与资源准入；成功返回凭据，**不得改变音频输出**。
    fn prepare_config_json(&mut self, config_json: &str) -> Result<PreparedConfig, WsOpError>;

    /// W16 commit：block 边界实际生效并返回实际 ack 证据；失败必须回 last-good/mute。
    fn commit_prepared(&mut self, prepared: &PreparedConfig) -> Result<CommitAck, WsOpError>;

    /// W16 abort：安全丢弃已 prepare（可能 commit 未验证）的 plan，保留 last-good。
    fn abort_prepared(&mut self, prepared: &PreparedConfig) -> Result<(), WsOpError>;
}

// ---------------------------------------------------------------------------
// 帧 / 回复 / 限制 / 会话统计与协议引擎（W01–W04、W20–W22）
// ---------------------------------------------------------------------------

/// WS 关闭码：正常关闭（RFC 6455）。
pub const WS_CLOSE_NORMAL: u16 = 1000;
/// WS 关闭码：协议错误。
pub const WS_CLOSE_PROTOCOL_ERROR: u16 = 1002;
/// WS 关闭码：非 UTF-8 载荷。
pub const WS_CLOSE_INVALID_PAYLOAD: u16 = 1007;
/// WS 关闭码：帧过大。
pub const WS_CLOSE_TOO_BIG: u16 = 1009;

/// 输入帧（T26 传输层解码 WebSocket 后喂给本引擎；本模块不碰 socket）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsFrame<'a> {
    /// 文本帧（UTF-8 JSON）。
    Text(&'a str),
    /// 二进制帧（W01：显式报错，不静默当文本）。
    Binary(&'a [u8]),
    /// Ping（必须以 Pong 原 payload 回显）。
    Ping(&'a [u8]),
    /// Pong（忽略）。
    Pong(&'a [u8]),
    /// Close（连接结束；本模块会 abort pending prepare）。
    Close,
}

/// 输出帧（T26 编码成 WebSocket 帧）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsReply {
    /// 无回复（Pong 帧/已关闭连接）。
    None,
    /// 文本帧（JSON；可能是一条命令回复或帧级错误 envelope）。
    Text(String),
    /// Pong（回显 Ping payload）。
    Pong(Vec<u8>),
    /// 关闭连接（带关闭码与稳定原因）。
    Close {
        /// RFC 6455 关闭码。
        code: u16,
        /// 稳定原因文本（如 payload_too_large/invalid_utf8/normal_closure）。
        reason: String,
    },
}

impl WsReply {
    /// 文本内容。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            WsReply::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// 关闭信息。
    pub fn as_close(&self) -> Option<(u16, &str)> {
        match self {
            WsReply::Close { code, reason } => Some((*code, reason.as_str())),
            _ => None,
        }
    }
}

/// 会话限制（X22/W21：单帧/配置上限明确拒绝，不截断；测试用更小值注入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsLimits {
    /// 单帧上限（字节）。
    pub max_frame_bytes: usize,
    /// 配置文档上限（字节）。
    pub max_config_bytes: usize,
}

impl WsLimits {
    /// 构造限制。
    pub const fn new(max_frame_bytes: usize, max_config_bytes: usize) -> Self {
        Self {
            max_frame_bytes,
            max_config_bytes,
        }
    }

    /// 默认限制（8 MiB 帧 / 2 MiB 配置）。
    pub const DEFAULT: WsLimits = WsLimits::new(DEFAULT_MAX_FRAME_BYTES, MAX_CONFIG_BYTES);
}

impl Default for WsLimits {
    fn default() -> Self {
        WsLimits::DEFAULT
    }
}

/// 会话统计（非 RT 导出；T70/证据用；只计本会话）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WsSessionStats {
    /// 总帧数（Text/Binary/Ping/Pong/Close）。
    pub frames: u64,
    /// 文本帧。
    pub text_frames: u64,
    /// 二进制帧（显式报错）。
    pub binary_frames: u64,
    /// Ping 帧。
    pub ping_frames: u64,
    /// Pong 帧。
    pub pong_frames: u64,
    /// Close 帧。
    pub close_frames: u64,
    /// 帧级/请求级非法（含非 JSON、多键、非法命令名、Binary 报错）。
    pub invalid_frames: u64,
    /// 未知命令数（W20）。
    pub unknown_commands: u64,
    /// 命令成功回复数（result=Ok）。
    pub commands_ok: u64,
    /// 命令错误回复数（含 unknown）。
    pub commands_err: u64,
}

/// 单个 WS 连接会话（按连接创建；顺序处理帧、每帧最多一个回复；W04）。
///
/// 传输层契约（T26）：同一连接内**顺序**调用 handle_frame，并把返回值按 WsReply 编码发送；
/// 每次调用最多产生一个输出帧；不得并发调用同一 session（self 以可变借用传递已保证）。
#[derive(Debug)]
pub struct WsSession {
    limits: WsLimits,
    pending: Option<PreparedConfig>,
    closed: bool,
    stats: WsSessionStats,
}

impl WsSession {
    /// 新建会话。
    pub fn new(limits: WsLimits) -> Self {
        Self {
            limits,
            pending: None,
            closed: false,
            stats: WsSessionStats::default(),
        }
    }

    /// 当前限制。
    pub const fn limits(&self) -> WsLimits {
        self.limits
    }

    /// 会话统计快照。
    pub const fn stats(&self) -> WsSessionStats {
        self.stats
    }

    /// 在途 prepare（PrepareConfigJson 之后、CommitConfig/AbortConfig 之前）。
    pub fn pending_prepare(&self) -> Option<&PreparedConfig> {
        self.pending.as_ref()
    }

    /// 是否已关闭（Close/非法 UTF-8/超帧后）。
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// 处理一个输入帧，返回要发回的帧（W01/W02/W03/W04/W20/W21）。
    pub fn handle_frame(&mut self, plane: &mut dyn WsControlPlane, frame: WsFrame<'_>) -> WsReply {
        self.stats.frames += 1;
        if self.closed {
            return WsReply::None;
        }
        match frame {
            WsFrame::Text(text) => {
                self.stats.text_frames += 1;
                if text.len() > self.limits.max_frame_bytes {
                    return self.close_with_abort(
                        plane,
                        WS_CLOSE_TOO_BIG,
                        WsErrorCode::PayloadTooLarge.as_str(),
                    );
                }
                self.dispatch_text(plane, text)
            }
            WsFrame::Binary(bytes) => {
                self.stats.binary_frames += 1;
                if bytes.len() > self.limits.max_frame_bytes {
                    return self.close_with_abort(
                        plane,
                        WS_CLOSE_TOO_BIG,
                        WsErrorCode::PayloadTooLarge.as_str(),
                    );
                }
                self.stats.invalid_frames += 1;
                WsReply::Text(encode_generic_error(
                    WsErrorCode::InvalidFrameType,
                    "binary frames are not supported; send UTF-8 JSON text frames",
                ))
            }
            WsFrame::Ping(payload) => {
                self.stats.ping_frames += 1;
                WsReply::Pong(payload.to_vec())
            }
            WsFrame::Pong(_) => {
                self.stats.pong_frames += 1;
                WsReply::None
            }
            WsFrame::Close => {
                self.stats.close_frames += 1;
                self.closed = true;
                let _ = self.abort_pending(plane);
                WsReply::Close {
                    code: WS_CLOSE_NORMAL,
                    reason: "normal_closure".to_string(),
                }
            }
        }
    }

    /// 处理文本帧（便捷入口）。
    pub fn handle_text(&mut self, plane: &mut dyn WsControlPlane, text: &str) -> WsReply {
        self.handle_frame(plane, WsFrame::Text(text))
    }

    /// 处理原始字节帧（先校验 UTF-8：非法则 1007 关闭；W01/W21）。
    pub fn handle_bytes(&mut self, plane: &mut dyn WsControlPlane, bytes: &[u8]) -> WsReply {
        self.stats.frames += 1;
        if self.closed {
            return WsReply::None;
        }
        if bytes.len() > self.limits.max_frame_bytes {
            return self.close_with_abort(
                plane,
                WS_CLOSE_TOO_BIG,
                WsErrorCode::PayloadTooLarge.as_str(),
            );
        }
        match core::str::from_utf8(bytes) {
            Ok(text) => {
                self.stats.text_frames += 1;
                self.dispatch_text(plane, text)
            }
            Err(_) => {
                self.stats.invalid_frames += 1;
                self.close_with_abort(plane, WS_CLOSE_INVALID_PAYLOAD, "invalid_utf8")
            }
        }
    }

    /// 中止在途 prepare（连接关闭/传输层超时时由 T26 调用；幂等）。
    pub fn abort_pending(&mut self, plane: &mut dyn WsControlPlane) -> Result<(), WsOpError> {
        match self.pending.take() {
            None => Ok(()),
            Some(prepared) => plane.abort_prepared(&prepared),
        }
    }

    fn close_with_abort(
        &mut self,
        plane: &mut dyn WsControlPlane,
        code: u16,
        reason: &str,
    ) -> WsReply {
        self.closed = true;
        let _ = self.abort_pending(plane);
        WsReply::Close {
            code,
            reason: reason.to_string(),
        }
    }

    fn dispatch_text(&mut self, plane: &mut dyn WsControlPlane, text: &str) -> WsReply {
        match json_parse(text) {
            Err(err) => {
                self.stats.invalid_frames += 1;
                WsReply::Text(encode_generic_error(
                    WsErrorCode::InvalidJson,
                    &err.to_string(),
                ))
            }
            Ok(value) => match decode_request(&value) {
                Err(err) => {
                    self.stats.invalid_frames += 1;
                    WsReply::Text(encode_generic_error(err.code(), err.message()))
                }
                Ok(request) => self.dispatch_request(plane, request),
            },
        }
    }

    fn dispatch_request(
        &mut self,
        plane: &mut dyn WsControlPlane,
        request: WsRequest<'_>,
    ) -> WsReply {
        let command = request.command;
        if !is_supported_command(command) {
            self.stats.unknown_commands += 1;
            self.stats.commands_err += 1;
            let mut message = String::from("unsupported command: ");
            message.push_str(command);
            return WsReply::Text(encode_error_reply(
                command,
                &WsOpError::new(WsErrorCode::UnknownCommand, message),
            ));
        }
        let payload = request.payload_or_null();
        match self.handle_command(plane, command, payload) {
            Ok(value) => {
                self.stats.commands_ok += 1;
                WsReply::Text(encode_ok_reply(command, &value))
            }
            Err(err) => {
                self.stats.commands_err += 1;
                WsReply::Text(encode_error_reply(command, &err))
            }
        }
    }
}

impl WsSession {
    fn handle_command(
        &mut self,
        plane: &mut dyn WsControlPlane,
        command: &str,
        payload: &JsonValue,
    ) -> Result<JsonValue, WsOpError> {
        match command {
            // ---- W05–W15 读命令 ----
            "GetVersion" => Ok(JsonValue::String(plane.protocol_version())),
            "GetState" => Ok(JsonValue::String(plane.run_state().as_str().to_string())),
            "GetStopReason" => {
                let reason = plane.stop_reason();
                if reason == WsStopReason::None {
                    Ok(JsonValue::Null)
                } else {
                    Ok(JsonValue::String(reason.as_str().to_string()))
                }
            }
            "GetConfigFilePath" => Ok(JsonValue::String(plane.config_path())),
            "GetConfigTitle" => Ok(JsonValue::String(plane.config_title())),
            "GetVolume" => {
                let db = require_db_read(plane.volume_db(), "volume")?;
                Ok(JsonValue::Number(db))
            }
            "GetMute" => Ok(JsonValue::Bool(plane.mute())),
            "GetCaptureRate" => {
                let rate = plane.capture_rate_hz();
                // 0 表示未运行（无 actual rate）；P1 实际采样率集合 = core SAMPLE_RATES_HZ。
                if rate != 0 && !SAMPLE_RATES_HZ.contains(&rate) {
                    return Err(internal_error(format!(
                        "capture_rate {rate} Hz not in supported actual rates"
                    )));
                }
                Ok(JsonValue::UInt(rate as u64))
            }
            "GetClippedSamples" => Ok(JsonValue::UInt(plane.clipped_samples())),
            "GetProcessingLoad" => {
                let load = require_finite_read(plane.processing_load(), "processing_load")?;
                if load < 0.0 {
                    return Err(internal_error("processing_load must be >= 0"));
                }
                Ok(JsonValue::Number(load))
            }
            "GetBufferLevel" => Ok(JsonValue::Int(plane.buffer_level_frames())),
            "GetRateAdjust" => {
                if !plane.rate_adjust_available() {
                    // F-28：没有唯一 clock servo 读数时不得返回 0.0 冒充测量值。
                    return Err(WsOpError::new(
                        WsErrorCode::Unsupported,
                        "rate adjustment unavailable: no clock servo measurement (host not wired)",
                    ));
                }
                let ppm = require_finite_read(plane.rate_adjust_ppm(), "rate_adjust_ppm")?;
                Ok(JsonValue::Number(ppm))
            }
            "GetConfig" => Ok(JsonValue::String(plane.config_document())),
            // ---- W17/W09 音量/静音 ----
            "SetVolume" => {
                let db = require_number(payload, "volume db")?;
                require_db_argument(db)?;
                let actual = plane.set_volume_db(db)?;
                let actual = require_db_read(actual, "volume")?;
                Ok(JsonValue::Number(actual))
            }
            "SetMute" => {
                let mute = payload
                    .as_bool()
                    .ok_or_else(|| invalid_argument("mute must be a boolean"))?;
                let actual = plane.set_mute(mute)?;
                Ok(JsonValue::Bool(actual))
            }
            // ---- W18 RMS ----
            "GetCaptureSignalRms" => self.rms_value(plane),
            // ---- W19/W13 fader ----
            "SetFaderVolume" => self.set_fader_value(plane, payload),
            "GetFaderVolume" => {
                let fader_id = require_fader_id(payload)?;
                match plane.fader_volume_db(fader_id) {
                    Some(db) => {
                        let db = require_db_read(db, "fader volume")?;
                        Ok(JsonValue::Number(db))
                    }
                    None => Err(WsOpError::new(
                        WsErrorCode::UnknownFader,
                        format!("fader {fader_id} is not allocated"),
                    )),
                }
            }
            // ---- actual version 读回 ----
            "GetConfigVersion" => {
                let actual = plane.actual_plan();
                actual.validate()?;
                Ok(actual_plan_json(&actual))
            }
            // ---- W16 prepare/commit/abort ----
            "SetConfigJson" => self.set_config_json(plane, payload),
            "PrepareConfigJson" => self.prepare_config(plane, payload),
            "CommitConfig" => self.commit_config(plane, payload),
            "AbortConfig" => self.abort_config(plane),
            // is_supported_command 已过滤；到这里说明 dispatch 表与命令表分叉。
            other => Err(internal_error(format!(
                "command {other} is listed as supported but has no dispatch"
            ))),
        }
    }
}

impl WsSession {
    fn rms_value(&self, plane: &dyn WsControlPlane) -> Result<JsonValue, WsOpError> {
        let channels = plane.capture_channel_count();
        let rms = plane.capture_signal_rms_db();
        if rms.len() != channels as usize {
            return Err(internal_error(format!(
                "rms length {} != capture channels {}",
                rms.len(),
                channels
            )));
        }
        let mut values: Vec<JsonValue> = Vec::with_capacity(rms.len());
        for (index, value) in rms.iter().enumerate() {
            if !value.is_finite() {
                return Err(internal_error(format!("rms[{index}] is not finite")));
            }
            // W18 冻结口径：静音 floor = RMS_FLOOR_DB（低于 floor 的读数按 floor 上报）。
            let clamped = if *value < RMS_FLOOR_DB {
                RMS_FLOOR_DB
            } else {
                *value
            };
            values.push(JsonValue::Number(clamped));
        }
        Ok(JsonValue::Array(values))
    }

    fn set_fader_value(
        &mut self,
        plane: &mut dyn WsControlPlane,
        payload: &JsonValue,
    ) -> Result<JsonValue, WsOpError> {
        let items = payload
            .as_array()
            .ok_or_else(|| invalid_argument("SetFaderVolume expects [fader_id, volume_db]"))?;
        if items.len() != 2 {
            return Err(invalid_argument(
                "SetFaderVolume expects exactly 2 elements [fader_id, volume_db]",
            ));
        }
        let fader_id = require_fader_id(&items[0])?;
        let db = require_number(&items[1], "fader volume db")?;
        require_db_argument(db)?;
        let actual = plane.set_fader_volume_db(fader_id, db)?;
        let actual = require_db_read(actual, "fader volume")?;
        Ok(JsonValue::Number(actual))
    }

    fn config_payload<'a>(&self, payload: &'a JsonValue) -> Result<&'a str, WsOpError> {
        let config = payload
            .as_str()
            .ok_or_else(|| invalid_argument("config must be a JSON string"))?;
        if config.trim().is_empty() {
            return Err(invalid_argument("config string is empty"));
        }
        if config.len() > self.limits.max_config_bytes {
            let mut message = String::from("config exceeds max_config_bytes (");
            let _ = write!(
                message,
                "{} > {}",
                config.len(),
                self.limits.max_config_bytes
            );
            message.push(')');
            return Err(WsOpError::new(WsErrorCode::PayloadTooLarge, message));
        }
        Ok(config)
    }

    fn prepare_config(
        &mut self,
        plane: &mut dyn WsControlPlane,
        payload: &JsonValue,
    ) -> Result<JsonValue, WsOpError> {
        if self.pending.is_some() {
            return Err(WsOpError::new(
                WsErrorCode::PrepareBusy,
                "a prepared config is already pending commit/abort",
            ));
        }
        let config = self.config_payload(payload)?;
        let prepared = plane.prepare_config_json(config)?;
        prepared.validate()?;
        let value = prepare_ack_json(&prepared);
        self.pending = Some(prepared);
        Ok(value)
    }

    fn set_config_json(
        &mut self,
        plane: &mut dyn WsControlPlane,
        payload: &JsonValue,
    ) -> Result<JsonValue, WsOpError> {
        if self.pending.is_some() {
            return Err(WsOpError::new(
                WsErrorCode::PrepareBusy,
                "a prepared config is already pending commit/abort",
            ));
        }
        let config = self.config_payload(payload)?;
        let prepared = plane.prepare_config_json(config)?;
        prepared.validate()?;
        self.finish_commit(plane, &prepared)
    }

    fn commit_config(
        &mut self,
        plane: &mut dyn WsControlPlane,
        payload: &JsonValue,
    ) -> Result<JsonValue, WsOpError> {
        let token = optional_plan_version(payload)?;
        let pending_version = match self.pending.as_ref() {
            Some(pending) => pending.plan_version,
            None => {
                return Err(WsOpError::new(
                    WsErrorCode::NotPrepared,
                    "no prepared config to commit",
                ))
            }
        };
        if let Some(version) = token {
            if version != pending_version {
                return Err(WsOpError::new(
                    WsErrorCode::StalePlan,
                    format!("plan_version {version} is not the pending {pending_version}"),
                ));
            }
        }
        let prepared = match self.pending.take() {
            Some(prepared) => prepared,
            None => {
                return Err(WsOpError::new(
                    WsErrorCode::NotPrepared,
                    "no prepared config to commit",
                ))
            }
        };
        self.finish_commit(plane, &prepared)
    }

    fn abort_config(&mut self, plane: &mut dyn WsControlPlane) -> Result<JsonValue, WsOpError> {
        let prepared = match self.pending.take() {
            Some(prepared) => prepared,
            None => {
                return Err(WsOpError::new(
                    WsErrorCode::NotPrepared,
                    "no prepared config to abort",
                ))
            }
        };
        plane.abort_prepared(&prepared)?;
        Ok(json_object(vec![
            ("aborted", JsonValue::Bool(true)),
            (
                "plan_version",
                JsonValue::UInt(prepared.plan_version as u64),
            ),
        ]))
    }

    /// prepare 之后 commit，并核对实际 ack（W16 核心；任何失败都不回 Ok）。
    fn finish_commit(
        &mut self,
        plane: &mut dyn WsControlPlane,
        prepared: &PreparedConfig,
    ) -> Result<JsonValue, WsOpError> {
        let ack = match plane.commit_prepared(prepared) {
            Ok(ack) => ack,
            // host 承诺 commit 失败时已回 last-good/mute，不再重复 abort（避免双重回滚）。
            Err(err) => return Err(err),
        };
        match validate_commit_ack(prepared, &ack) {
            Ok(()) => Ok(commit_ack_json(&ack)),
            Err(err) => {
                // 未达到实际 ack 判据：立刻让 host 丢弃该 plan，并在回复中如实报错。
                let _ = plane.abort_prepared(prepared);
                let mut message = String::from("commit not acknowledged: ");
                message.push_str(err.message());
                Err(WsOpError::new(WsErrorCode::CommitNotAcked, message))
            }
        }
    }
}

impl Default for WsSession {
    fn default() -> Self {
        Self::new(WsLimits::default())
    }
}

// ---------------------------------------------------------------------------
// 载荷/返回值校验与回复编码辅助
// ---------------------------------------------------------------------------

fn require_finite_read(value: f64, what: &str) -> Result<f64, WsOpError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(internal_error(format!(
            "{what} must be finite (host contract)"
        )))
    }
}

fn require_db_read(value: f64, what: &str) -> Result<f64, WsOpError> {
    let value = require_finite_read(value, what)?;
    if !(VOLUME_MIN_DB..=VOLUME_MAX_DB).contains(&value) {
        return Err(internal_error(format!(
            "{what} {value} dB outside {VOLUME_MIN_DB}..{VOLUME_MAX_DB} (host contract)"
        )));
    }
    Ok(value)
}

fn require_db_argument(value: f64) -> Result<(), WsOpError> {
    if !value.is_finite() {
        return Err(invalid_argument("volume db must be finite"));
    }
    if !(VOLUME_MIN_DB..=VOLUME_MAX_DB).contains(&value) {
        return Err(invalid_argument(format!(
            "volume db {value} outside {VOLUME_MIN_DB}..{VOLUME_MAX_DB}"
        )));
    }
    Ok(())
}

fn require_number(payload: &JsonValue, what: &str) -> Result<f64, WsOpError> {
    let value = payload
        .as_f64()
        .ok_or_else(|| invalid_argument(format!("{what} must be a number")))?;
    if !value.is_finite() {
        return Err(invalid_argument(format!("{what} must be finite")));
    }
    Ok(value)
}

fn require_fader_id(payload: &JsonValue) -> Result<i32, WsOpError> {
    let id = payload
        .as_i64()
        .ok_or_else(|| invalid_argument("fader id must be an integer"))?;
    if !(0..=i32::MAX as i64).contains(&id) {
        return Err(invalid_argument("fader id out of range"));
    }
    Ok(id as i32)
}

fn optional_plan_version(payload: &JsonValue) -> Result<Option<u32>, WsOpError> {
    match payload {
        JsonValue::Null => Ok(None),
        JsonValue::Object(pairs) => {
            if pairs.is_empty() {
                return Ok(None);
            }
            if pairs.len() > 1 {
                return Err(invalid_argument(
                    "CommitConfig accepts only the plan_version field",
                ));
            }
            let (key, value) = &pairs[0];
            if key != "plan_version" {
                return Err(invalid_argument(
                    "CommitConfig accepts only the plan_version field",
                ));
            }
            plan_version_value(value)
        }
        other => plan_version_value(other),
    }
}

fn plan_version_value(value: &JsonValue) -> Result<Option<u32>, WsOpError> {
    let version = value
        .as_u64()
        .ok_or_else(|| invalid_argument("plan_version must be a non-negative integer"))?;
    let version =
        u32::try_from(version).map_err(|_| invalid_argument("plan_version out of range (u32)"))?;
    Ok(Some(version))
}

fn encode_ok_reply(command: &str, value: &JsonValue) -> String {
    let mut out = String::new();
    out.push('{');
    push_json_string(&mut out, command);
    out.push_str(":{\"result\":\"Ok\",\"value\":");
    if push_json_value(&mut out, value).is_err() {
        out.push_str("null");
    }
    out.push_str("}}");
    out
}

fn encode_error_reply(command: &str, error: &WsOpError) -> String {
    let mut out = String::new();
    out.push('{');
    push_json_string(&mut out, command);
    out.push_str(":{\"result\":");
    if push_json_value(&mut out, &error.to_json()).is_err() {
        out.push_str("null");
    }
    out.push_str("}}");
    out
}

fn encode_generic_error(code: WsErrorCode, message: &str) -> String {
    let mut out = String::from("{\"error\":{\"code\":");
    push_json_string(&mut out, code.as_str());
    out.push_str(",\"message\":");
    push_json_string(&mut out, message);
    out.push_str("}}");
    out
}
