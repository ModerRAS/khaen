//! [NONRT] 输入内容状态（S1 兼容资料，P1 不消费） ——WBS T25 写域。
//!
//! ## 状态：DEPRECATED / COMPAT-SHIM（F-31：退役对照资料，不是 compat 公开运行时）
//!
//! - 本模块只服务**退役核对与回归对拍**（tests/T25）；P1 不消费：`p1_apply_s1_content` 一律
//!   `p1_not_supported`，REST `/api/channelmap/input-mode` 恒 `not_supported`（R47 唯一例外）。
//! - **不得**把本模块的内容判定/上混状态机链进控制面生产路径；模式切换/T42/T70 均不调用它
//!   （五个播放模式的 `upmix` 都是 false）。
//! - 职责边界：可变 N/O 映射归 `crate::roles`（T57）+ `crate::channel_map`（T25 扩展点占位），
//!   WS 传输契约归 `crate::ws`（T24）。
//!
//! ## 本模块两层职责（历史设计，保留原文供退役核对）
//!
//! 本模块是 rt-stack compat leaf content（WBS T25；deps T24/T23/T44 已 PASS）。职责分两层：
//!
//! 1. **S1 契约兼容资料（只记录/只模拟，不驱动 P1）**：把既有板端 S1 内容检测
//!    daemon（`tspi-upmix autodetect`）的可判语义冻结为纯函数 + 状态机，供退役核对、
//!    回归对拍与未来恢复时评审：
//!    - 读 `GetCaptureSignalRms`（W18）按采集通道索引取 dB，按输入布局 role→index 映射为
//!      各角色 RMS（缺失/非有限按 -240 dBFS floor 处理，S1 口径）；
//!    - 判定只用 **C / SL / SR / height** 的活动判多声道，**LFE 不作判据**
//!      （H16 旧固件 2.0 内容会填 L/R + LFE，LFE 有能量仍必须判 stereo）；
//!    - 500 ms 采样窗口、连续 hold 2500 ms 才切换、进入/退出阈值差 6 dB 迟滞、
//!      启动冻结 2500 ms（期间保持直通）；
//!    - auto/stereo/multichannel 三值 + override 优先级 override > detector > auto，
//!      auto 默认直通、detected=stereo 才上混；`force-*` 别名归一化；
//!    - 双路径切换通过 CamillaDSP Volume Aux1（fader 1 = 上混路径）与 Aux2
//!      （fader 2 = 多声道直通路径）0 dB / -120 dB 静音实现；本模块给出**原子绑定**
//!      的 fader 状态与切换计划（先关旧路径再开新路径，任何中间态都不得双路径同时有声，
//!      即不得双上混叠加）。
//! 2. **P1 显式拒绝（P1 不消费）**：按 compat-matrix §6 C01–C08 / R47、DEC §1/§6，P1 不实现
//!    节目上混或内容自动上混 daemon，本模块**不**向上层提供任何"用 S1 状态切上混/直通"的
//!    入口：`p1_apply_s1_content` 一律 `p1_not_supported`；REST GET 只能回
//!    `{"status":"not_supported","reason":"p1-no-content-upmix"}`，PUT 只能 400
//!    `not_supported`；S1 的 `input-content.json` / `input-mode.json` 只作兼容资料，
//!    新栈不读、不写、不据其切换。T44 mode 的 legacy status `input_content` 字段恒为 null。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/compat-matrix.md §6 C01–C08（input-content v1 / input-mode v1 字段、
//!   优先级、LFE 排除、时间参数、H16 内部上混推断、P1 RETIRE 处置）与 §4 W18/W19
//!   （GetCaptureSignalRms/SetFaderVolume 兼容口径）、§5 Y18、R47；
//! - scratch/lead/planner/DSP-HOST-DECISION.md §1（P1 不实现上混/下混/自动内容上混或
//!   旧 Aux 双路径上混）、§6（不实现内容自动上混 daemon；只保输入 N/目标 O 显式 reject）、
//!   §7 G-no-C（禁用/移除 `tspi-upmix-autodetect.service` 及其 Camilla 依赖）；
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T25 行与 §1/§7（写域、executed>0、不碰板端）；
//! - 既有 S1 契约与实现（只读参考）：
//!   - `upmix/docs/INPUT-MODE.md` §1（运行态/覆盖文件字段、优先级、判定规则、启动冻结）、
//!     §2（双路径 + Aux1/Aux2 fader、默认安全态 `--gain1=-120 --gain2=0`）、§7（已知限制）；
//!   - `audio/overlay/usr/local/bin/tspi-upmix`（`detect_content` 阈值/迟滞/置信度、
//!     `cmd_autodetect` 窗口/hold/冻结/override、`_set_faders` 1/2 号 fader -120/0 dB、
//!     `build_content_state` 字段、`FADER_STEREO=1`/`FADER_PASS=2`）；
//!   - `audio/overlay/etc/systemd/system/tspi-upmix-autodetect.service`（S1 unit 的
//!     Wants/After=tspi-camilladsp.service 与 ExecStart hold/freeze 参数）；
//!   - `web/src/channelmap.rs`（`normalize_input_mode` 别名、`build_input_mode_view`
//!     override>detector>auto 合并、daemon_running=10 s 窗口）。
//! - 新栈接口（只读参考，兼容/复用口径、不反向依赖）：T24 `crate::ws` 的 JSON 编解码、
//!   `RMS_FLOOR_DB`/`VOLUME_MIN_DB`/`VOLUME_MAX_DB`、W18/W19 命令名；T23 `core::nodes`
//!   的 `stereo_strict`/`allowed_role_mask`（P1 不做上混的图侧约束）；T44 `control::modes`
//!   的 toslink/target_multichannel 模式（toslink 必须 stereo strict、无上混）。
//!
//! # 非目标 / 边界
//! - 不执行 S1 daemon、不打开文件/socket/websocket/ALSA、不起线程/不 sleep、不写 `/run` 或
//!   `/etc`：所有时间由调用方以毫秒传入，所有 RMS/fader 都是内存值（纯 NONRT 模拟）；
//! - 不实现 P1 的上混/下混/内容路由：P1 侧入口一律显式拒绝（见 `p1_*`）；S1 状态只记录；
//! - 不实现 CamillaDSP 客户端/Volume ramp：fader 计划只表达稳态 0/-120 dB 的**切换顺序**，
//!   交叉渐变由宿主 ramp 完成（S1 `ramp_time=200 ms`）；
//! - 不解析 layout-presets YAML（T57 roles 负责）；本模块内置的 `h16-7.1.4` role→index 表
//!   是 S1 daemon 既有默认输入布局的冻结副本，只用于 S1 兼容模拟，不作为 P1 的映射真相；
//! - 不新增依赖（纯 std + `tspi-dsp-core`；JSON 复用同 crate `crate::ws` 的编解码）。
//!
//! # 与 S1 实现的显式偏差（记录于 evidence/T25/MANIFEST.md）
//! - D-1：manual override / startup freeze 期间清空 pending 候选计时。S1 `cmd_autodetect`
//!   仅在 frozen 分支清空 pending；manual 分支保留旧 pending_since，导致 override 解除后
//!   可能立即切换。本模块统一在两种分支清空，避免迟滞计时跨越模式边界；state 字段/理由
//!   不变，仅消除该实现瑕疵，不改变 Lead 冻结的优先级/窗口语义。
//! - D-2：判据副声道只用**显式 allowlist**（C/SL/SR/BL/BR/TFL/TFR/TRL/TRR/HFL/HFR/HRL/HRR）；
//!   S1 的 `detect_content` 在 `secondary_roles=None` 时把"除 FL/FR/LFE 外所有出现角色"
//!   都算作副声道。契约文字只承诺 C/SL/SR/height，本模块按契约收紧：未知角色名不参与
//!   多声道判定（但仍可出现在 `active_channels` 能量列表），避免未知 role 名造成误上混。
//! - D-3：非有限/缺失 RMS 一律按 -240 dBFS floor 处理（W18 floor 口径），不把 NaN/Inf 带进
//!   比较或状态文件；S1 客户端对缺失值同样按 -240 处理，对 -inf 未定义。
//!
//! # 数据流（供 T42/T70/T74 只读参考与退役证据）
//!
//! ~~~text
//! GetCaptureSignalRms value[]（W18：长度=采集通道、有限、-240 floor）
//!   -> parse_get_capture_signal_rms(value, capture_channels)
//!   -> rms_by_role(&capture, H16_7_1_4_ROLE_INDEX)              // role->dB
//!   -> detect_content(&role_db, &ContentConfig)                  // 判定/LFE 排除/迟滞带
//!   -> ContentDaemon::tick(now_ms, &role_db, updated_unix)       // 冻结/hold/override/state
//!        -> ContentState::to_json_string()                       // S1 state（只记录）
//!        -> Option<FaderSwitchPlan>                              // 原子、无双上混的 fader 计划
//!             -> plan.commands_json() -> {"SetFaderVolume":[1|-120]} ...
//!   P1：p1_apply_s1_content(...) => Err(p1_not_supported)；REST 只回 not_supported 状态壳
//! ~~~

use std::fmt;

use crate::ws::{json_encode, json_object, JsonValue, RMS_FLOOR_DB, VOLUME_MAX_DB, VOLUME_MIN_DB};

/// 本模块的兼容状态标记（F-31）：S1 退役对照资料，**不是** compat 公开运行时。
/// 生产路径不得据本模块切上混；tests/T25 用该标记断言「已标注退役」。
pub const COMPAT_STATUS: &str = "retired_s1_reference_not_runtime";

// ---------------------------------------------------------------------------
// 冻结常量（S1 契约 / W18·W19 兼容口径；不是 P1 音频调参默认值）
// ---------------------------------------------------------------------------

/// S1 运行态文件 schema（C01：`/run/tspi/input-content.json`）。
pub const INPUT_CONTENT_SCHEMA: &str = "tspi-input-content/v1";
/// S1 覆盖文件 schema（C02：`/etc/tspi-audio/input-mode.json`）。
pub const INPUT_MODE_SCHEMA: &str = "tspi-input-mode/v1";
/// 规范内容模式取值（C02；顺序即线上顺序）。
pub const INPUT_MODE_CHOICES: [&str; 3] = ["auto", "stereo", "multichannel"];
/// S1 默认判定阈值（dBFS；`tspi-upmix --threshold-db` 默认 -50.0）。
pub const DEFAULT_THRESHOLD_DB: f64 = -50.0;
/// S1 默认迟滞（dB；进入阈值与退出阈值之差，默认 6.0）。
pub const DEFAULT_HYSTERESIS_DB: f64 = 6.0;
/// S1 daemon 默认连续保持时间（ms；`--hold-ms 2500`）。
pub const DEFAULT_HOLD_MS: u32 = 2500;
/// S1 daemon 默认启动冻结（ms；`--startup-freeze-ms 2500`）。
pub const DEFAULT_STARTUP_FREEZE_MS: u32 = 2500;
/// S1 daemon 默认轮询间隔（ms；`--interval-ms` 默认 500）。
pub const DEFAULT_SAMPLE_INTERVAL_MS: u32 = 500;
/// S1 RMS 静音 floor（dBFS；缺失/非有限/低于 floor 一律按此上报；与 W18 `RMS_FLOOR_DB` 同值）。
pub const CONTENT_RMS_FLOOR_DB: f64 = RMS_FLOOR_DB;
/// S1 Aux 路径打开电平（dB）。
pub const AUX_OPEN_DB: f64 = 0.0;
/// S1 Aux 路径静音电平（dB；`tspi-upmix MUTE_DB=-120.0`）。
pub const AUX_MUTE_DB: f64 = -120.0;
/// S1 上混路径 fader id（Camilla Volume Aux1）。
pub const FADER_AUX_STEREO: i32 = 1;
/// S1 直通路径 fader id（Camilla Volume Aux2）。
pub const FADER_AUX_PASS: i32 = 2;
/// S1 双路径 fader id 表（顺序：上混路径、直通路径）。
pub const AUX_FADER_IDS: [i32; 2] = [FADER_AUX_STEREO, FADER_AUX_PASS];
/// S1 daemon 默认输入容器布局（`tspi-upmix autodetect --input-layout h16-7.1.4`）。
pub const S1_DEFAULT_CONTAINER_LAYOUT: &str = "h16-7.1.4";
/// `h16-7.1.4` 容器通道数（layout-presets v2：streams [8,4]）。
pub const H16_7_1_4_CHANNELS: usize = 12;
/// `h16-7.1.4` role→采集通道索引（S1 默认输入布局的冻结副本；顺序来自 overlay 预设：
/// FL0 FR1 C2 LFE3 SL4 SR5 BL6 BR7 + flow1 TFL8 TFR9 TRL10 TRR11；仅供 S1 兼容模拟）。
pub const H16_7_1_4_ROLE_INDEX: [(&str, usize); 12] = [
    ("FL", 0),
    ("FR", 1),
    ("C", 2),
    ("LFE", 3),
    ("SL", 4),
    ("SR", 5),
    ("BL", 6),
    ("BR", 7),
    ("TFL", 8),
    ("TFR", 9),
    ("TRL", 10),
    ("TRR", 11),
];
/// 立体声判据角色（S1 `stereo_roles=("FL","FR")`）。
pub const STEREO_ROLES: [&str; 2] = ["FL", "FR"];
/// LFE 角色名：**永远不是**多声道判据（C04；H16 旧固件 2.0 内容会填 L/R+LFE）。
pub const LFE_ROLE: &str = "LFE";
/// 多声道判据副声道 allowlist：C / SL / SR / height（含 BL/BR 环绕）。
/// 未知角色不在本表内 → 不参与多声道判定（D-2）；`LFE` 显式排除。
pub const CONTENT_SECONDARY_ROLES: [&str; 13] = [
    "C", "SL", "SR", "BL", "BR", "TFL", "TFR", "TRL", "TRR", "HFL", "HFR", "HRL", "HRR",
];
/// daemon 活跃判定窗口（web `build_input_mode_view`：state 10 s 内有更新视为 daemon 在跑）。
pub const DAEMON_RUNNING_WINDOW_SECS: u64 = 10;

/// S1 state `reason` 稳定字符串（daemon/CLI 既有输出口径）。
pub const REASON_NO_DATA: &str = "no-data";
/// 无信号（判据角色都不高于进入阈值）。
pub const REASON_NO_SIGNAL: &str = "no-signal";
/// FL/FR 有信号且副声道不高于退出阈值 → 2.0 内容（走上混路径）。
pub const REASON_FL_FR_ONLY: &str = "fl-fr-only";
/// 副声道高于进入阈值 → 多声道内容（走直通路径）。
pub const REASON_SECONDARY_ACTIVE: &str = "secondary-active";
/// 副声道落在迟滞带（> exit 且 <= enter）→ 保持当前状态不变。
pub const REASON_HYSTERESIS_BAND: &str = "hysteresis-band";
/// 启动冻结期（保持直通）。
pub const REASON_STARTUP_FREEZE: &str = "startup-freeze";
/// daemon 未运行（state 缺失时的合成状态）。
pub const REASON_DAEMON_NOT_RUNNING: &str = "daemon-not-running";
/// 手动覆盖理由前缀（实际值 `override-stereo` / `override-multichannel`）。
pub const REASON_OVERRIDE_PREFIX: &str = "override-";
/// Camilla WS 错误理由前缀（实际值 `ws-error: <detail>`）。
pub const REASON_WS_ERROR_PREFIX: &str = "ws-error";
/// fader 写入错误理由前缀（实际值 `fader-error: <detail>`）。
pub const REASON_FADER_ERROR_PREFIX: &str = "fader-error";

/// P1 不支持内容自动上混（compat-matrix C01–C08 / R47；DEC §1/§6）。
pub const P1_CONTENT_UPMMIX_SUPPORTED: bool = false;
/// P1 input-mode GET 状态壳的 reason（R47 冻结字符串）。
pub const P1_NO_CONTENT_UPMMIX_REASON: &str = "p1-no-content-upmix";
/// P1 input-mode GET 状态壳的 status（R47 冻结字符串）。
pub const P1_INPUT_MODE_STATUS: &str = "not_supported";
/// P1 必须退役的 S1 内容检测 unit（C07；必须 disabled/removed 且 inactive）。
pub const P1_RETIRED_CONTENT_UNIT: &str = "tspi-upmix-autodetect.service";
/// S1 unit 必须清除的 Camilla 依赖项（C07：否则 G-no-C 依赖/socket 检查失败）。
pub const P1_RETIRED_CONTENT_UNIT_CAMILLA_DEPS: [&str; 2] = [
    "Wants=tspi-camilladsp.service",
    "After=tspi-camilladsp.service",
];
/// P1 禁止在运行期出现的内容面能力（只作审计字符串，不参与逻辑）。
pub const P1_FORBIDDEN_CONTENT_FEATURES: [&str; 3] = [
    "content-autodetect-daemon",
    "aux-dual-path-upmix",
    "h16-internal-upmix-inference-consumed",
];
/// S1 `GetCaptureSignalRms` 命令名（W18）。
pub const CMD_GET_CAPTURE_SIGNAL_RMS: &str = "GetCaptureSignalRms";
/// S1 `SetFaderVolume` 命令名（W19）。
pub const CMD_SET_FADER_VOLUME: &str = "SetFaderVolume";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// content 模块稳定错误码（字符串值冻结；不静默降级、不猜未知值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentErrorCode {
    /// 配置非法（阈值/迟滞/hold/冻结/采样间隔）。
    InvalidConfig,
    /// 未知内容模式（只能是 auto/stereo/multichannel 或登记别名）。
    UnknownContentMode,
    /// JSON 文本语法错误。
    InvalidJson,
    /// schema 缺失或不是冻结版本。
    InvalidSchema,
    /// 必填字段缺失。
    MissingField,
    /// 字段类型错误。
    FieldType,
    /// 字段取值越界/非法枚举。
    FieldRange,
    /// 双上混叠加（两条 Aux 路径同时有声）——任何入口都不得产生。
    DoubleUpmix,
    /// 绑定不自洽（content_mode 与 fader 状态不匹配）。
    IncoherentBinding,
    /// fader 稳态非法（不是 0 / -120 dB 的路径开关值，或未知 fader id）。
    InvalidFader,
    /// fader 写入失败（宿主回调返回错误）。
    FaderWriteFailed,
    /// P1 显式不支持（内容上混/内容驱动切换）。
    P1NotSupported,
    /// 未知 P1 播放模式名。
    UnknownPlaybackMode,
    /// JSON 编码失败（非有限数字等；任何路径都不得把 NaN/Inf 上线）。
    Encode,
}

impl ContentErrorCode {
    /// 全部错误码（测试/报告用）。
    pub const ALL: &'static [ContentErrorCode] = &[
        ContentErrorCode::InvalidConfig,
        ContentErrorCode::UnknownContentMode,
        ContentErrorCode::InvalidJson,
        ContentErrorCode::InvalidSchema,
        ContentErrorCode::MissingField,
        ContentErrorCode::FieldType,
        ContentErrorCode::FieldRange,
        ContentErrorCode::DoubleUpmix,
        ContentErrorCode::IncoherentBinding,
        ContentErrorCode::InvalidFader,
        ContentErrorCode::FaderWriteFailed,
        ContentErrorCode::P1NotSupported,
        ContentErrorCode::UnknownPlaybackMode,
        ContentErrorCode::Encode,
    ];

    /// 稳定字符串码（线上/报告 code 字段）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ContentErrorCode::InvalidConfig => "invalid_config",
            ContentErrorCode::UnknownContentMode => "unknown_content_mode",
            ContentErrorCode::InvalidJson => "invalid_json",
            ContentErrorCode::InvalidSchema => "invalid_schema",
            ContentErrorCode::MissingField => "missing_field",
            ContentErrorCode::FieldType => "field_type",
            ContentErrorCode::FieldRange => "field_range",
            ContentErrorCode::DoubleUpmix => "double_upmix",
            ContentErrorCode::IncoherentBinding => "incoherent_binding",
            ContentErrorCode::InvalidFader => "invalid_fader",
            ContentErrorCode::FaderWriteFailed => "fader_write_failed",
            ContentErrorCode::P1NotSupported => "p1_not_supported",
            ContentErrorCode::UnknownPlaybackMode => "unknown_playback_mode",
            ContentErrorCode::Encode => "encode",
        }
    }

    /// 由稳定字符串码反解。
    pub fn from_str(code: &str) -> Option<ContentErrorCode> {
        ContentErrorCode::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == code)
    }
}

impl fmt::Display for ContentErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// content 模块错误（稳定 code + 脱敏消息；不 panic）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentError {
    code: ContentErrorCode,
    message: String,
}

impl ContentError {
    /// 构造错误（message 由调用方保证脱敏）。
    pub fn new(code: ContentErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 错误码。
    pub const fn code(&self) -> ContentErrorCode {
        self.code
    }

    /// 可读消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 错误 JSON（code/message；T42 REST/T70 报告可直接复用，不是完整 error envelope）。
    pub fn to_json(&self) -> JsonValue {
        json_object(vec![
            ("code", JsonValue::String(self.code.as_str().to_string())),
            ("message", JsonValue::String(self.message.clone())),
        ])
    }
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ContentError {}

fn error(code: ContentErrorCode, message: impl Into<String>) -> ContentError {
    ContentError::new(code, message)
}

fn p1_not_supported(message: impl Into<String>) -> ContentError {
    error(ContentErrorCode::P1NotSupported, message)
}

// ---------------------------------------------------------------------------
// 枚举：内容模式 / 检测结果 / 检测器来源 / 理由
// ---------------------------------------------------------------------------

/// 内容模式（C02/C03；auto = 由检测器决定，默认直通）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentMode {
    /// 自动：detected=stereo 才上混；未知/多声道一律直通（S1 auto 默认直通）。
    Auto,
    /// 强制 2.0：走上混路径。
    Stereo,
    /// 强制多声道：走直通路径。
    Multichannel,
}

impl ContentMode {
    /// 规范线上字符串（auto/stereo/multichannel）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ContentMode::Auto => "auto",
            ContentMode::Stereo => "stereo",
            ContentMode::Multichannel => "multichannel",
        }
    }

    /// 全部模式（顺序 = 冻结线上顺序）。
    pub const ALL: &'static [ContentMode] = &[
        ContentMode::Auto,
        ContentMode::Stereo,
        ContentMode::Multichannel,
    ];

    /// 严格解析规范值（大小写敏感、不接受别名；用于 state/schema 回读与配置校验）。
    pub fn from_str_strict(text: &str) -> Option<ContentMode> {
        match text {
            "auto" => Some(ContentMode::Auto),
            "stereo" => Some(ContentMode::Stereo),
            "multichannel" => Some(ContentMode::Multichannel),
            _ => None,
        }
    }

    /// 归一化解析（CLI/覆盖文件口径：去空白、大小写不敏感、接受登记别名）。
    ///
    /// 别名并集（S1 CLI normalize_input_mode + web normalize_input_mode）：
    /// - auto：auto、自动
    /// - stereo：stereo、force-stereo、2.0、2ch、force-2.0
    /// - multichannel：multichannel、force-multichannel、multi、mc、mch、force-multi
    pub fn normalize(text: &str) -> Option<ContentMode> {
        let trimmed = text.trim();
        let lowered = trimmed.to_ascii_lowercase();
        match lowered.as_str() {
            "auto" => Some(ContentMode::Auto),
            "stereo" | "force-stereo" | "2.0" | "2ch" | "force-2.0" => Some(ContentMode::Stereo),
            "multichannel" | "force-multichannel" | "multi" | "mc" | "mch" | "force-multi" => {
                Some(ContentMode::Multichannel)
            }
            _ => {
                if trimmed == "自动" {
                    Some(ContentMode::Auto)
                } else {
                    None
                }
            }
        }
    }

    /// 是否强制（override 生效值；auto 不是强制）。
    pub const fn is_forced(self) -> bool {
        !matches!(self, ContentMode::Auto)
    }

    /// 该模式对应的 S1 路径：stereo=上混，multichannel=直通，auto 未定（按直通安全默认）。
    pub const fn s1_path(self) -> AuxPath {
        match self {
            ContentMode::Stereo => AuxPath::StereoUpmix,
            ContentMode::Auto | ContentMode::Multichannel => AuxPath::PassThrough,
        }
    }
}

impl fmt::Display for ContentMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 检测结果（C01 detected）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DetectedKind {
    /// 未知：无信号 / 迟滞带 / 未运行。
    Unknown,
    /// 2.0 内容（FL/FR 有活动、C/SL/SR/height 无活动）。
    Stereo,
    /// 多声道内容（C/SL/SR/height 有活动；LFE 不作判据）。
    Multichannel,
}

impl DetectedKind {
    /// 线上字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            DetectedKind::Unknown => "unknown",
            DetectedKind::Stereo => "stereo",
            DetectedKind::Multichannel => "multichannel",
        }
    }

    /// 全部取值。
    pub const ALL: &'static [DetectedKind] = &[
        DetectedKind::Unknown,
        DetectedKind::Stereo,
        DetectedKind::Multichannel,
    ];

    /// 严格解析。
    pub fn from_str_strict(text: &str) -> Option<DetectedKind> {
        DetectedKind::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == text)
    }

    /// H16 内部上混推断（C06：multichannel→true，stereo→false，unknown→null）。推断值，
    /// 不是 H16 实测设置（H16 的 Pure/Dolby Upmix/DTS Neural:X 无法直接读取）。
    pub const fn h16_internal_upmix(self) -> Option<bool> {
        match self {
            DetectedKind::Multichannel => Some(true),
            DetectedKind::Stereo => Some(false),
            DetectedKind::Unknown => None,
        }
    }
}

impl fmt::Display for DetectedKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 检测器来源（C01 detector：energy=自动检测 / manual=被覆盖强制 / fallback=WS 或 fader 出错）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentDetectorKind {
    /// 能量自动检测。
    Energy,
    /// 覆盖文件强制。
    Manual,
    /// 回退（WS/fader 错误或 daemon 未运行）。
    Fallback,
}

impl ContentDetectorKind {
    /// 线上字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            ContentDetectorKind::Energy => "energy",
            ContentDetectorKind::Manual => "manual",
            ContentDetectorKind::Fallback => "fallback",
        }
    }

    /// 全部取值。
    pub const ALL: &'static [ContentDetectorKind] = &[
        ContentDetectorKind::Energy,
        ContentDetectorKind::Manual,
        ContentDetectorKind::Fallback,
    ];

    /// 严格解析。
    pub fn from_str_strict(text: &str) -> Option<ContentDetectorKind> {
        ContentDetectorKind::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == text)
    }
}

impl fmt::Display for ContentDetectorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 判定理由（C01 reason；override-* 带模式后缀）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContentReason {
    /// 没有任何判据角色的 RMS（无数据）。
    NoData,
    /// 判据角色都不高于进入阈值。
    NoSignal,
    /// FL/FR 有活动、副声道不高于退出阈值 → 2.0。
    FlFrOnly,
    /// 副声道高于进入阈值 → 多声道。
    SecondaryActive,
    /// 副声道落在迟滞带 → 保持当前状态。
    HysteresisBand,
    /// 启动冻结期（保持直通）。
    StartupFreeze,
    /// 覆盖强制（携带生效模式）。
    Override(ContentMode),
    /// daemon 未运行（合成状态）。
    DaemonNotRunning,
    /// Camilla WS 错误（detail 另存）。
    WsError,
    /// fader 写入错误（detail 另存）。
    FaderError,
}

impl ContentReason {
    /// 稳定前缀/字符串（override 返回 override，完整值见 ContentReason::to_reason_string）。
    pub const fn code(self) -> &'static str {
        match self {
            ContentReason::NoData => REASON_NO_DATA,
            ContentReason::NoSignal => REASON_NO_SIGNAL,
            ContentReason::FlFrOnly => REASON_FL_FR_ONLY,
            ContentReason::SecondaryActive => REASON_SECONDARY_ACTIVE,
            ContentReason::HysteresisBand => REASON_HYSTERESIS_BAND,
            ContentReason::StartupFreeze => REASON_STARTUP_FREEZE,
            ContentReason::Override(_) => REASON_OVERRIDE_PREFIX,
            ContentReason::DaemonNotRunning => REASON_DAEMON_NOT_RUNNING,
            ContentReason::WsError => REASON_WS_ERROR_PREFIX,
            ContentReason::FaderError => REASON_FADER_ERROR_PREFIX,
        }
    }

    /// state 文件 reason 字段值（override 展开为 override-stereo 等；错误类不带 detail）。
    pub fn to_reason_string(self) -> String {
        match self {
            ContentReason::Override(mode) => format!("{}{}", REASON_OVERRIDE_PREFIX, mode.as_str()),
            other => other.code().to_string(),
        }
    }
}

impl fmt::Display for ContentReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_reason_string())
    }
}

// ---------------------------------------------------------------------------
// 数值辅助 / 配置
// ---------------------------------------------------------------------------

/// 四舍五入到指定小数位（half away from zero；S1 state 用 round(...,1/2) 口径）。
fn round_to(value: f64, decimals: u32) -> f64 {
    if !value.is_finite() {
        return value;
    }
    let factor = 10f64.powi(decimals as i32);
    (value * factor).round() / factor
}

fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if value < low {
        low
    } else if value > high {
        high
    } else {
        value
    }
}

/// S1 检测/daemon 配置（全部显式；默认值 = S1 daemon 默认参数）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentConfig {
    /// 进入多声道的判定阈值（dBFS；副声道 > 该值判多声道）。默认 -50.0。
    pub threshold_db: f64,
    /// 迟滞（dB；退出阈值 = threshold_db - hysteresis_db）。默认 6.0。
    pub hysteresis_db: f64,
    /// 连续满足 hold 才切换（ms）。默认 2500。
    pub hold_ms: u32,
    /// 启动冻结（ms；期间保持直通）。默认 2500。
    pub startup_freeze_ms: u32,
    /// 轮询间隔（ms；元数据/采样节拍口径，判定本身由调用方传入 now_ms）。默认 500。
    pub sample_interval_ms: u32,
}

impl Default for ContentConfig {
    fn default() -> Self {
        Self {
            threshold_db: DEFAULT_THRESHOLD_DB,
            hysteresis_db: DEFAULT_HYSTERESIS_DB,
            hold_ms: DEFAULT_HOLD_MS,
            startup_freeze_ms: DEFAULT_STARTUP_FREEZE_MS,
            sample_interval_ms: DEFAULT_SAMPLE_INTERVAL_MS,
        }
    }
}

impl ContentConfig {
    /// 退出阈值：副声道 > (threshold - hysteresis) 且 <= threshold 时位于迟滞带（判 unknown，
    /// 保持当前状态）；<= 退出阈值才算已离开多声道。
    pub fn exit_threshold_db(&self) -> f64 {
        self.threshold_db - self.hysteresis_db
    }

    /// 校验配置（有限值/范围；失败给稳定错误码，不 panic）。
    pub fn validate(&self) -> Result<(), ContentError> {
        if !self.threshold_db.is_finite() {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "threshold_db must be finite",
            ));
        }
        if !self.hysteresis_db.is_finite() || self.hysteresis_db < 0.0 {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "hysteresis_db must be finite and >= 0",
            ));
        }
        if self.hold_ms == 0 || self.hold_ms > 600_000 {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "hold_ms must be in 1..=600000",
            ));
        }
        if self.startup_freeze_ms > 600_000 {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "startup_freeze_ms must be <= 600000",
            ));
        }
        if self.sample_interval_ms == 0 || self.sample_interval_ms > 60_000 {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "sample_interval_ms must be in 1..=60000",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GetCaptureSignalRms（W18）读取与 role 映射
// ---------------------------------------------------------------------------

/// 解析 S1 侧 GetCaptureSignalRms 的 value 数组（W18 兼容口径）。
///
/// 契约：必须是数组；长度必须等于采集通道数（不发半截数组）；每个值必须是有限 dB 数；
/// 低于静音 floor（-240 dBFS）按 floor 上报（禁止 NaN/Inf 上线）。
pub fn parse_get_capture_signal_rms(
    value: &JsonValue,
    capture_channels: usize,
) -> Result<Vec<f64>, ContentError> {
    let items = value.as_array().ok_or_else(|| {
        error(
            ContentErrorCode::FieldType,
            "GetCaptureSignalRms value must be an array",
        )
    })?;
    if items.len() != capture_channels {
        return Err(error(
            ContentErrorCode::FieldRange,
            format!(
                "rms length {} != capture channels {}",
                items.len(),
                capture_channels
            ),
        ));
    }
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let db = item.as_f64().ok_or_else(|| {
            error(
                ContentErrorCode::FieldType,
                format!("rms[{index}] must be a number"),
            )
        })?;
        if !db.is_finite() {
            return Err(error(
                ContentErrorCode::FieldRange,
                format!("rms[{index}] must be finite"),
            ));
        }
        out.push(if db < CONTENT_RMS_FLOOR_DB {
            CONTENT_RMS_FLOOR_DB
        } else {
            db
        });
    }
    Ok(out)
}

/// 把采集通道 dB 数组映射为各角色 dB（S1 _ws_capture_rms 口径）：
/// 索引越界/缺失 → -240 dBFS floor；非有限值 → floor；低于 floor → floor。
pub fn rms_by_role<'a>(capture: &[f64], role_index: &'a [(&'a str, usize)]) -> Vec<(&'a str, f64)> {
    role_index
        .iter()
        .map(|(role, index)| {
            let value = capture.get(*index).copied().unwrap_or(CONTENT_RMS_FLOOR_DB);
            let value = if value.is_finite() {
                value.max(CONTENT_RMS_FLOOR_DB)
            } else {
                CONTENT_RMS_FLOOR_DB
            };
            (*role, value)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// detect_content（C04/C05 判定；S1 detect_content 语义）
// ---------------------------------------------------------------------------

/// 单次内容判定结果（不携带时间窗口/覆盖，只表达"这一刻能量看起来是什么"）。
#[derive(Clone, Debug, PartialEq)]
pub struct DetectionResult {
    /// 判定结果（unknown = 无数据/无信号/迟滞带）。
    pub detected: DetectedKind,
    /// 置信度（0..=1；S1 公式：多声道 0.2+(sec-thr)/20，stereo 0.5+margin/40，夹在 0.05..=1）。
    pub confidence: f64,
    /// 阈值以上角色（按 dB 降序、同值按角色名升序；可含 LFE——能量列表，不作判据）。
    pub active_channels: Vec<String>,
    /// 判据副声道的最大 dB（S1 state 字段名 rms_min_db；无副声道数据为 None）。
    pub rms_min_db: Option<f64>,
    /// 理由。
    pub reason: ContentReason,
}

impl DetectionResult {
    /// H16 内部上混推断（同 DetectedKind::h16_internal_upmix）。
    pub const fn h16_internal_upmix(&self) -> Option<bool> {
        self.detected.h16_internal_upmix()
    }
}

/// S1 内容判定（tspi-upmix detect_content 等价语义）。
///
/// 规则（C04/C05，Lead 2026-11-15 裁决）：
/// 1. 副声道 = CONTENT_SECONDARY_ROLES 中出现且有限的角色（C/SL/SR/height）；**LFE 永不计入**；
/// 2. secondary_max > threshold_db → multichannel（secondary-active）；
/// 3. 否则 secondary_max > exit_threshold_db → unknown（hysteresis-band，保持当前状态）；
/// 4. 否则 fl_fr_max > threshold_db → stereo（fl-fr-only）；
/// 5. 否则 unknown（no-signal）；没有任何有限 RMS 值 → unknown（no-data）。
pub fn detect_content(
    rms_db: &[(&str, f64)],
    config: &ContentConfig,
) -> Result<DetectionResult, ContentError> {
    config.validate()?;
    let values: Vec<(&str, f64)> = rms_db
        .iter()
        .copied()
        .filter(|(_, db)| db.is_finite())
        .collect();
    if values.is_empty() {
        return Ok(DetectionResult {
            detected: DetectedKind::Unknown,
            confidence: 0.0,
            active_channels: Vec::new(),
            rms_min_db: None,
            reason: ContentReason::NoData,
        });
    }
    let threshold = config.threshold_db;
    let exit = config.exit_threshold_db();
    let mut active: Vec<(&str, f64)> = values
        .iter()
        .copied()
        .filter(|(_, db)| *db >= threshold)
        .collect();
    active.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(b.0))
    });
    let active_channels: Vec<String> = active.iter().map(|(role, _)| (*role).to_string()).collect();
    let fl_max = values
        .iter()
        .filter(|(role, _)| STEREO_ROLES.contains(role))
        .map(|(_, db)| *db)
        .fold(None, |acc: Option<f64>, db| {
            Some(acc.map_or(db, |current| current.max(db)))
        });
    let secondary_max = values
        .iter()
        .filter(|(role, _)| CONTENT_SECONDARY_ROLES.contains(role))
        .map(|(_, db)| *db)
        .fold(None, |acc: Option<f64>, db| {
            Some(acc.map_or(db, |current| current.max(db)))
        });
    let rms_min_db = secondary_max.map(|db| round_to(db, 1));
    if fl_max.is_none() && secondary_max.is_none() {
        return Ok(DetectionResult {
            detected: DetectedKind::Unknown,
            confidence: 0.0,
            active_channels,
            rms_min_db,
            reason: ContentReason::NoData,
        });
    }
    if let Some(sec) = secondary_max {
        if sec > threshold {
            let confidence = round_to(clamp(0.2 + (sec - threshold) / 20.0, 0.05, 1.0), 2);
            return Ok(DetectionResult {
                detected: DetectedKind::Multichannel,
                confidence,
                active_channels,
                rms_min_db,
                reason: ContentReason::SecondaryActive,
            });
        }
        if sec > exit {
            return Ok(DetectionResult {
                detected: DetectedKind::Unknown,
                confidence: 0.0,
                active_channels,
                rms_min_db,
                reason: ContentReason::HysteresisBand,
            });
        }
    }
    if let Some(fl) = fl_max {
        if fl > threshold {
            let sec_or_floor = secondary_max.unwrap_or(CONTENT_RMS_FLOOR_DB);
            let margin = clamp(threshold - sec_or_floor, 0.0, 20.0);
            let confidence = round_to(clamp(0.5 + margin / 40.0, 0.05, 1.0), 2);
            return Ok(DetectionResult {
                detected: DetectedKind::Stereo,
                confidence,
                active_channels,
                rms_min_db,
                reason: ContentReason::FlFrOnly,
            });
        }
    }
    Ok(DetectionResult {
        detected: DetectedKind::Unknown,
        confidence: 0.0,
        active_channels,
        rms_min_db,
        reason: ContentReason::NoSignal,
    })
}

/// H16 内部上混推断（C06）：multichannel→true，stereo→false，unknown→None。
pub const fn h16_internal_upmix(detected: DetectedKind) -> Option<bool> {
    detected.h16_internal_upmix()
}

// ---------------------------------------------------------------------------
// S1 运行态 / 覆盖文件（C01/C02）：结构、JSON 编解码
// ---------------------------------------------------------------------------

/// S1 运行态 tspi-input-content/v1（C01）。字段名/类型按契约冻结；本结构只作兼容资料，
/// **P1 不消费**。
#[derive(Clone, Debug, PartialEq)]
pub struct ContentState {
    /// 生效模式（stereo=走上混，multichannel=走直通，auto=尚未判定/未运行）。
    pub content_mode: ContentMode,
    /// 本轮检测结果。
    pub detected: DetectedKind,
    /// 覆盖文件里的值（null=没有覆盖文件；auto/stereo/multichannel 均可能出现）。
    pub override_mode: Option<ContentMode>,
    /// 置信度 0..=1。
    pub confidence: f64,
    /// 阈值以上的角色（能量列表；可含 LFE，但 LFE 不作判据）。
    pub active_channels: Vec<String>,
    /// 各角色 RMS（dB；已按 0.1 dB 取整；缺失角色可省略）。
    pub rms_db: Vec<(String, f64)>,
    /// 判定阈值（dBFS）。
    pub threshold_db: f64,
    /// hold（ms）。
    pub hold_ms: u32,
    /// 理由字符串（ContentReason::to_reason_string 或带 detail 的错误理由）。
    pub reason: String,
    /// H16 内部上混推断（按 detected；null=unknown）。
    pub h16_internal_upmix: Option<bool>,
    /// 输入容器布局（如 h16-7.1.4）。
    pub container_layout: Option<String>,
    /// 检测器来源。
    pub detector: ContentDetectorKind,
    /// 状态更新时间（unix 秒；由调用方传入）。
    pub updated_unix: u64,
}

impl ContentState {
    /// 合成"daemon 未运行"状态（C08：state 缺失时 detected=unknown、reason=daemon-not-running，
    /// 不抛错）。content_mode=auto（auto 默认直通）。
    pub fn daemon_not_running(
        override_mode: Option<ContentMode>,
        config: &ContentConfig,
        container_layout: Option<&str>,
        updated_unix: u64,
    ) -> Self {
        Self {
            content_mode: ContentMode::Auto,
            detected: DetectedKind::Unknown,
            override_mode,
            confidence: 0.0,
            active_channels: Vec::new(),
            rms_db: Vec::new(),
            threshold_db: config.threshold_db,
            hold_ms: config.hold_ms,
            reason: REASON_DAEMON_NOT_RUNNING.to_string(),
            h16_internal_upmix: None,
            container_layout: container_layout.map(str::to_string),
            detector: ContentDetectorKind::Fallback,
            updated_unix,
        }
    }

    /// 线上 JSON（字段顺序 = C01 契约顺序）。
    pub fn to_json(&self) -> JsonValue {
        json_object(vec![
            (
                "schema",
                JsonValue::String(INPUT_CONTENT_SCHEMA.to_string()),
            ),
            (
                "content_mode",
                JsonValue::String(self.content_mode.as_str().to_string()),
            ),
            (
                "detected",
                JsonValue::String(self.detected.as_str().to_string()),
            ),
            (
                "override",
                match self.override_mode {
                    Some(mode) => JsonValue::String(mode.as_str().to_string()),
                    None => JsonValue::Null,
                },
            ),
            ("confidence", JsonValue::Number(self.confidence)),
            (
                "active_channels",
                JsonValue::Array(
                    self.active_channels
                        .iter()
                        .map(|role| JsonValue::String(role.clone()))
                        .collect(),
                ),
            ),
            (
                "rms_db",
                JsonValue::Object(
                    self.rms_db
                        .iter()
                        .map(|(role, db)| (role.clone(), JsonValue::Number(*db)))
                        .collect(),
                ),
            ),
            ("threshold_db", JsonValue::Number(self.threshold_db)),
            ("hold_ms", JsonValue::UInt(u64::from(self.hold_ms))),
            ("reason", JsonValue::String(self.reason.clone())),
            (
                "h16_internal_upmix",
                match self.h16_internal_upmix {
                    Some(value) => JsonValue::Bool(value),
                    None => JsonValue::Null,
                },
            ),
            (
                "container_layout",
                match &self.container_layout {
                    Some(layout) => JsonValue::String(layout.clone()),
                    None => JsonValue::Null,
                },
            ),
            (
                "detector",
                JsonValue::String(self.detector.as_str().to_string()),
            ),
            ("updated_unix", JsonValue::UInt(self.updated_unix)),
        ])
    }

    /// 编码为 JSON 文本（禁止 NaN/Inf 上线；失败给 encode 错误）。
    pub fn to_json_string(&self) -> Result<String, ContentError> {
        json_encode(&self.to_json())
            .map_err(|e| error(ContentErrorCode::Encode, format!("state encode: {e}")))
    }

    /// 从 JSON 值严格解析（schema 精确匹配；必填字段/类型/范围校验；未知键忽略——
    /// S1 WebUI 读取方同样忽略未知键，纯前向扩展字段不参与应用）。
    pub fn from_json(value: &JsonValue) -> Result<ContentState, ContentError> {
        let schema = value
            .get("schema")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| error(ContentErrorCode::MissingField, "state.schema is required"))?;
        if schema != INPUT_CONTENT_SCHEMA {
            return Err(error(
                ContentErrorCode::InvalidSchema,
                format!("state.schema must be {INPUT_CONTENT_SCHEMA}"),
            ));
        }
        let content_mode_raw = required_str(value, "content_mode")?;
        let content_mode = ContentMode::normalize(content_mode_raw).ok_or_else(|| {
            error(
                ContentErrorCode::FieldRange,
                format!("state.content_mode unknown: {content_mode_raw}"),
            )
        })?;
        let detected_raw = required_str(value, "detected")?;
        let detected = DetectedKind::from_str_strict(detected_raw).ok_or_else(|| {
            error(
                ContentErrorCode::FieldRange,
                format!("state.detected unknown: {detected_raw}"),
            )
        })?;
        let override_mode = match value.get("override") {
            None | Some(JsonValue::Null) => None,
            Some(JsonValue::String(raw)) => Some(ContentMode::normalize(raw).ok_or_else(|| {
                error(
                    ContentErrorCode::FieldRange,
                    format!("state.override unknown: {raw}"),
                )
            })?),
            Some(_) => {
                return Err(error(
                    ContentErrorCode::FieldType,
                    "state.override must be string or null",
                ))
            }
        };
        let confidence = required_f64(value, "confidence")?;
        if !(0.0..=1.0).contains(&confidence) {
            return Err(error(
                ContentErrorCode::FieldRange,
                "state.confidence must be in 0..=1",
            ));
        }
        let active_channels = required_array(value, "active_channels")?
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    error(
                        ContentErrorCode::FieldType,
                        "state.active_channels entries must be strings",
                    )
                })
            })
            .collect::<Result<Vec<String>, ContentError>>()?;
        let rms_obj = value
            .get("rms_db")
            .ok_or_else(|| error(ContentErrorCode::MissingField, "state.rms_db is required"))?;
        let rms_pairs = rms_obj.as_object().ok_or_else(|| {
            error(
                ContentErrorCode::FieldType,
                "state.rms_db must be an object",
            )
        })?;
        let mut rms_db = Vec::with_capacity(rms_pairs.len());
        for (role, item) in rms_pairs {
            let db = item.as_f64().ok_or_else(|| {
                error(
                    ContentErrorCode::FieldType,
                    format!("state.rms_db.{role} must be a number"),
                )
            })?;
            if !db.is_finite() {
                return Err(error(
                    ContentErrorCode::FieldRange,
                    format!("state.rms_db.{role} must be finite"),
                ));
            }
            rms_db.push((role.clone(), db));
        }
        let threshold_db = required_f64(value, "threshold_db")?;
        if !threshold_db.is_finite() {
            return Err(error(
                ContentErrorCode::FieldRange,
                "state.threshold_db must be finite",
            ));
        }
        let hold_raw = value
            .get("hold_ms")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| error(ContentErrorCode::MissingField, "state.hold_ms is required"))?;
        let hold_ms = u32::try_from(hold_raw)
            .map_err(|_| error(ContentErrorCode::FieldRange, "state.hold_ms out of range"))?;
        let reason = required_str(value, "reason")?.to_string();
        let h16_internal_upmix = match value.get("h16_internal_upmix") {
            None | Some(JsonValue::Null) => None,
            Some(JsonValue::Bool(flag)) => Some(*flag),
            Some(_) => {
                return Err(error(
                    ContentErrorCode::FieldType,
                    "state.h16_internal_upmix must be bool or null",
                ))
            }
        };
        let container_layout = match value.get("container_layout") {
            None | Some(JsonValue::Null) => None,
            Some(JsonValue::String(layout)) => Some(layout.clone()),
            Some(_) => {
                return Err(error(
                    ContentErrorCode::FieldType,
                    "state.container_layout must be string or null",
                ))
            }
        };
        let detector_raw = required_str(value, "detector")?;
        let detector = ContentDetectorKind::from_str_strict(detector_raw).ok_or_else(|| {
            error(
                ContentErrorCode::FieldRange,
                format!("state.detector unknown: {detector_raw}"),
            )
        })?;
        let updated_unix = value
            .get("updated_unix")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| {
                error(
                    ContentErrorCode::MissingField,
                    "state.updated_unix is required",
                )
            })?;
        Ok(ContentState {
            content_mode,
            detected,
            override_mode,
            confidence,
            active_channels,
            rms_db,
            threshold_db,
            hold_ms,
            reason,
            h16_internal_upmix,
            container_layout,
            detector,
            updated_unix,
        })
    }

    /// 从 JSON 文本解析（语法错 → invalid_json）。
    pub fn from_json_str(text: &str) -> Result<ContentState, ContentError> {
        let value = crate::ws::json_parse(text)
            .map_err(|e| error(ContentErrorCode::InvalidJson, e.to_string()))?;
        ContentState::from_json(&value)
    }
}

fn required_str<'a>(value: &'a JsonValue, key: &str) -> Result<&'a str, ContentError> {
    value.get(key).and_then(JsonValue::as_str).ok_or_else(|| {
        error(
            ContentErrorCode::MissingField,
            format!("state.{key} is required (string)"),
        )
    })
}

fn required_f64(value: &JsonValue, key: &str) -> Result<f64, ContentError> {
    value.get(key).and_then(JsonValue::as_f64).ok_or_else(|| {
        error(
            ContentErrorCode::MissingField,
            format!("state.{key} is required (number)"),
        )
    })
}

fn required_array<'a>(value: &'a JsonValue, key: &str) -> Result<&'a [JsonValue], ContentError> {
    value.get(key).and_then(JsonValue::as_array).ok_or_else(|| {
        error(
            ContentErrorCode::MissingField,
            format!("state.{key} is required (array)"),
        )
    })
}

/// S1 覆盖文件 tspi-input-mode/v1（C02；WebUI/CLI 写，daemon 每轮读）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputModeOverride {
    /// 覆盖模式（auto/stereo/multichannel；写入前已归一化 force-* 别名）。
    pub mode: ContentMode,
    /// 更新时间（unix 秒；缺省 0）。
    pub updated_unix: u64,
    /// 更新者（缺省空串）。
    pub updated_by: String,
}

impl InputModeOverride {
    /// 新建覆盖。
    pub fn new(mode: ContentMode) -> Self {
        Self {
            mode,
            updated_unix: 0,
            updated_by: String::new(),
        }
    }

    /// 线上 JSON（字段顺序 = C02 契约顺序）。
    pub fn to_json(&self) -> JsonValue {
        json_object(vec![
            ("schema", JsonValue::String(INPUT_MODE_SCHEMA.to_string())),
            ("mode", JsonValue::String(self.mode.as_str().to_string())),
            ("updated_unix", JsonValue::UInt(self.updated_unix)),
            ("updated_by", JsonValue::String(self.updated_by.clone())),
        ])
    }

    /// 编码为 JSON 文本。
    pub fn to_json_string(&self) -> Result<String, ContentError> {
        json_encode(&self.to_json())
            .map_err(|e| error(ContentErrorCode::Encode, format!("override encode: {e}")))
    }

    /// 从 JSON 值解析（schema 必须匹配；mode 必填且接受 S1 别名并归一化；未知 mode 拒收，
    /// 不猜 auto；updated_unix/updated_by 可选——S1 WebUI 读覆盖文件同样只看 mode）。
    pub fn from_json(value: &JsonValue) -> Result<InputModeOverride, ContentError> {
        let schema = value
            .get("schema")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                error(
                    ContentErrorCode::MissingField,
                    "input-mode.schema is required",
                )
            })?;
        if schema != INPUT_MODE_SCHEMA {
            return Err(error(
                ContentErrorCode::InvalidSchema,
                format!("input-mode.schema must be {INPUT_MODE_SCHEMA}"),
            ));
        }
        let raw_mode = value
            .get("mode")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                error(
                    ContentErrorCode::MissingField,
                    "input-mode.mode is required",
                )
            })?;
        let mode = ContentMode::normalize(raw_mode).ok_or_else(|| {
            error(
                ContentErrorCode::UnknownContentMode,
                format!("input-mode.mode unknown: {raw_mode}"),
            )
        })?;
        let updated_unix = value
            .get("updated_unix")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let updated_by = value
            .get("updated_by")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        Ok(InputModeOverride {
            mode,
            updated_unix,
            updated_by,
        })
    }

    /// 从 JSON 文本解析。
    pub fn from_json_str(text: &str) -> Result<InputModeOverride, ContentError> {
        let value = crate::ws::json_parse(text)
            .map_err(|e| error(ContentErrorCode::InvalidJson, e.to_string()))?;
        InputModeOverride::from_json(&value)
    }
}

// ---------------------------------------------------------------------------
// 合并视图（override > detector > auto；web build_input_mode_view 等价语义）
// ---------------------------------------------------------------------------

/// WebUI 侧只读合并视图（C03；纯函数，不读文件）。
#[derive(Clone, Debug, PartialEq)]
pub struct ContentModeView {
    /// 生效模式。
    pub content_mode: ContentMode,
    /// 本轮检测值。
    pub detected: DetectedKind,
    /// 覆盖值（无覆盖文件为 None）。
    pub override_mode: Option<ContentMode>,
    /// 是否有运行态 state。
    pub state_available: bool,
    /// daemon 是否在跑（state.updated_unix 距 now 不超过 10 s）。
    pub daemon_running: bool,
    /// H16 内部上混推断。
    pub h16_internal_upmix: Option<bool>,
    /// 稳定提示码（UI 可映射文案；不使用中文常量，避免 i18n 耦合）：
    /// daemon-not-running / h16-internal-upmix-passthrough / stereo-upmix-required /
    /// manual-override。
    pub notes: Vec<&'static str>,
}

/// 合并运行态与覆盖：override > detector > auto（auto 默认直通）。
///
/// 与 web/src/channelmap.rs build_input_mode_view 等价：
/// - override 为 stereo/multichannel 时直接生效；
/// - 否则若 state.content_mode 存在：auto 时按 detected（stereo→stereo，其余→multichannel）
///   或 state 不存在→auto；非 auto 用 state 的值；
/// - 否则 detected=stereo → stereo；state 存在 → multichannel；否则 auto。
pub fn build_content_mode_view(
    state_mode: Option<ContentMode>,
    detected: DetectedKind,
    override_mode: Option<ContentMode>,
    state_available: bool,
    now_unix: u64,
    updated_unix: Option<u64>,
) -> ContentModeView {
    let daemon_running = updated_unix
        .map(|stamp| now_unix.saturating_sub(stamp) <= DAEMON_RUNNING_WINDOW_SECS)
        .unwrap_or(false);
    let content_mode = match override_mode {
        Some(ContentMode::Stereo) => ContentMode::Stereo,
        Some(ContentMode::Multichannel) => ContentMode::Multichannel,
        _ => match state_mode {
            Some(ContentMode::Auto) => {
                if detected == DetectedKind::Stereo {
                    ContentMode::Stereo
                } else if state_available {
                    ContentMode::Multichannel
                } else {
                    ContentMode::Auto
                }
            }
            Some(mode) => mode,
            None => {
                if detected == DetectedKind::Stereo {
                    ContentMode::Stereo
                } else if state_available {
                    ContentMode::Multichannel
                } else {
                    ContentMode::Auto
                }
            }
        },
    };
    let mut notes = Vec::new();
    if !state_available {
        notes.push("daemon-not-running");
    }
    let inferred = if state_available {
        detected.h16_internal_upmix()
    } else {
        None
    };
    if inferred == Some(true) || detected == DetectedKind::Multichannel {
        notes.push("h16-internal-upmix-passthrough");
    } else if detected == DetectedKind::Stereo {
        notes.push("stereo-upmix-required");
    }
    if override_mode.map(ContentMode::is_forced).unwrap_or(false) {
        notes.push("manual-override");
    }
    ContentModeView {
        content_mode,
        detected,
        override_mode,
        state_available,
        daemon_running,
        h16_internal_upmix: inferred,
        notes,
    }
}

// ---------------------------------------------------------------------------
// Aux1/Aux2 fader 双路径状态（S1 §2：Aux1=上混路径，Aux2=多声道直通路径）
// ---------------------------------------------------------------------------

/// Aux 路径状态（由两条 fader 的 0/-120 dB 组合决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AuxPath {
    /// 两条路径都静音（安全静默/切换中间态）。
    Muted,
    /// 只有上混路径（Aux1）有声。
    StereoUpmix,
    /// 只有直通路径（Aux2）有声。
    PassThrough,
    /// **双上混叠加**：两条路径同时有声（S1 Camilla 默认 0/0 会短暂出现，必须禁止）。
    DoubleUpmix,
}

impl AuxPath {
    /// 线上/报告字符串。
    pub const fn as_str(self) -> &'static str {
        match self {
            AuxPath::Muted => "muted",
            AuxPath::StereoUpmix => "stereo-upmix",
            AuxPath::PassThrough => "pass-through",
            AuxPath::DoubleUpmix => "double-upmix",
        }
    }

    /// 是否安全（非双上混；切换中间态 both-muted 也安全）。
    pub const fn is_safe(self) -> bool {
        !matches!(self, AuxPath::DoubleUpmix)
    }
}

impl fmt::Display for AuxPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 双路径 fader 稳态绑定（Aux1=fader 1 上混路径，Aux2=fader 2 直通路径）。
///
/// 稳态只允许 0 dB（开）或 -120 dB（静音）；交叉渐变（S1 ramp_time=200 ms）由宿主完成，
/// 本模型只表达稳态与切换顺序。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaderBinding {
    /// Aux1（上混路径）dB。
    pub aux_stereo_db: f64,
    /// Aux2（直通路径）dB。
    pub aux_pass_db: f64,
}

impl FaderBinding {
    /// 两条路径都静音（安全静默）。
    pub const fn both_muted() -> Self {
        Self {
            aux_stereo_db: AUX_MUTE_DB,
            aux_pass_db: AUX_MUTE_DB,
        }
    }

    /// 直通（Aux2 开、Aux1 静音）；S1 auto 默认直通 + 推荐启动参数 --gain1=-120 --gain2=0。
    pub const fn direct() -> Self {
        Self {
            aux_stereo_db: AUX_MUTE_DB,
            aux_pass_db: AUX_OPEN_DB,
        }
    }

    /// 上混（Aux1 开、Aux2 静音）。
    pub const fn upmix() -> Self {
        Self {
            aux_stereo_db: AUX_OPEN_DB,
            aux_pass_db: AUX_MUTE_DB,
        }
    }

    /// CamillaDSP 启动默认 Aux 音量（0/0 dB）——**不安全双路径叠加**，仅用于模拟 S1 已知
    /// 启动风险（INPUT-MODE §2 要求以 --gain1=-120 --gain2=0 或 daemon 尽快接管规避）。
    pub const fn camilla_startup_default() -> Self {
        Self {
            aux_stereo_db: AUX_OPEN_DB,
            aux_pass_db: AUX_OPEN_DB,
        }
    }

    /// 模式对应的安全稳态绑定（auto 按直通安全默认）。
    pub const fn for_mode(mode: ContentMode) -> Self {
        match mode {
            ContentMode::Stereo => FaderBinding::upmix(),
            ContentMode::Auto | ContentMode::Multichannel => FaderBinding::direct(),
        }
    }

    /// 路径状态。
    pub fn path(&self) -> AuxPath {
        let stereo_open = self.aux_stereo_db == AUX_OPEN_DB;
        let pass_open = self.aux_pass_db == AUX_OPEN_DB;
        match (stereo_open, pass_open) {
            (true, true) => AuxPath::DoubleUpmix,
            (true, false) => AuxPath::StereoUpmix,
            (false, true) => AuxPath::PassThrough,
            (false, false) => AuxPath::Muted,
        }
    }

    /// 是否双上混叠加（任何入口都不得产生）。
    pub fn is_double_upmix(&self) -> bool {
        self.path() == AuxPath::DoubleUpmix
    }

    /// 是否安全（非双上混）。
    pub fn is_safe(&self) -> bool {
        !self.is_double_upmix()
    }

    /// 是否安全稳态（恰好一条路径有声；切换中间态 both-muted 不算稳态）。
    pub fn is_steady_state(&self) -> bool {
        matches!(self.path(), AuxPath::StereoUpmix | AuxPath::PassThrough)
    }

    /// 是否全静音。
    pub fn is_silent(&self) -> bool {
        self.path() == AuxPath::Muted
    }

    /// 校验：每个 fader 必须是有限值且是 0/-120 dB 路径开关值。
    pub fn validate(&self) -> Result<(), ContentError> {
        for (fader, db) in [
            (FADER_AUX_STEREO, self.aux_stereo_db),
            (FADER_AUX_PASS, self.aux_pass_db),
        ] {
            if !db.is_finite() {
                return Err(error(
                    ContentErrorCode::InvalidFader,
                    format!("fader {fader} volume must be finite"),
                ));
            }
            if db != AUX_OPEN_DB && db != AUX_MUTE_DB {
                return Err(error(
                    ContentErrorCode::InvalidFader,
                    format!("fader {fader} steady volume must be 0 or -120 dB (got {db})"),
                ));
            }
        }
        Ok(())
    }

    /// [(fader_id, db); 2]（顺序：Aux1、Aux2）。
    pub fn writes(&self) -> [(i32, f64); 2] {
        [
            (FADER_AUX_STEREO, self.aux_stereo_db),
            (FADER_AUX_PASS, self.aux_pass_db),
        ]
    }
}

/// 内容模式 + 双路径 fader 的**原子绑定**（两者同属一个值：不存在"新模式 + 旧 fader"的
/// 中间可观测状态；切换先经 both-muted，再打开目标路径）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentBinding {
    /// 绑定的内容模式。
    pub content_mode: ContentMode,
    /// 绑定的双路径 fader 稳态。
    pub faders: FaderBinding,
}

impl ContentBinding {
    /// 模式对应的自洽绑定（auto 按直通安全默认）。
    pub const fn for_mode(mode: ContentMode) -> Self {
        Self {
            content_mode: mode,
            faders: FaderBinding::for_mode(mode),
        }
    }

    /// 失败安全绑定：全静音 + auto（未判定）。
    pub const fn safe_muted() -> Self {
        Self {
            content_mode: ContentMode::Auto,
            faders: FaderBinding::both_muted(),
        }
    }

    /// 是否双上混。
    pub fn is_double_upmix(&self) -> bool {
        self.faders.is_double_upmix()
    }

    /// 校验原子绑定：①不得双上混；②fader 必须是合法稳态；
    /// ③stereo 必须对应上混路径、multichannel 必须对应直通路径、auto 不得指向上混路径
    /// （auto 未判定时默认直通；both-muted 安全态对所有模式允许）。
    pub fn validate(&self) -> Result<(), ContentError> {
        self.faders.validate()?;
        if self.is_double_upmix() {
            return Err(error(
                ContentErrorCode::DoubleUpmix,
                "both Aux1(upmix) and Aux2(passthrough) are open",
            ));
        }
        if self.faders.is_silent() {
            return Ok(());
        }
        let expected = FaderBinding::for_mode(self.content_mode);
        if self.faders != expected {
            return Err(error(
                ContentErrorCode::IncoherentBinding,
                format!(
                    "content_mode {} requires faders {:?}, got {:?}",
                    self.content_mode, expected, self.faders
                ),
            ));
        }
        Ok(())
    }

    /// 到目标模式的原子切换计划。
    pub fn plan_to(&self, mode: ContentMode) -> FaderSwitchPlan {
        FaderSwitchPlan::plan(self.content_mode, self.faders, mode)
    }
}

/// 一次 fader 写入（W19 SetFaderVolume [fader_id, volume_db]）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaderWrite {
    /// fader id（S1：1=Aux1 上混，2=Aux2 直通）。
    pub fader_id: i32,
    /// 目标 dB。
    pub volume_db: f64,
}

impl FaderWrite {
    /// W19 命令 JSON：{"SetFaderVolume":[fader_id,db]}（单键对象）。
    pub fn to_command_json(&self) -> JsonValue {
        json_object(vec![(
            CMD_SET_FADER_VOLUME,
            JsonValue::Array(vec![
                JsonValue::Int(i64::from(self.fader_id)),
                JsonValue::Number(self.volume_db),
            ]),
        )])
    }

    /// W19 命令 JSON 文本。
    pub fn to_command_text(&self) -> Result<String, ContentError> {
        json_encode(&self.to_command_json())
            .map_err(|e| error(ContentErrorCode::Encode, format!("fader command: {e}")))
    }
}

/// 解析 W19 SetFaderVolume 载荷 [fader_id:int, volume_db:float]（S1 侧模拟用）。
pub fn parse_set_fader_volume(payload: &JsonValue) -> Result<(i32, f64), ContentError> {
    let items = payload.as_array().ok_or_else(|| {
        error(
            ContentErrorCode::FieldType,
            "SetFaderVolume payload must be an array",
        )
    })?;
    if items.len() != 2 {
        return Err(error(
            ContentErrorCode::FieldRange,
            "SetFaderVolume payload must be [fader_id, volume_db]",
        ));
    }
    let fader_raw = items[0].as_i64().ok_or_else(|| {
        error(
            ContentErrorCode::FieldType,
            "SetFaderVolume fader_id must be an integer",
        )
    })?;
    let fader_id = i32::try_from(fader_raw).map_err(|_| {
        error(
            ContentErrorCode::FieldRange,
            "SetFaderVolume fader_id out of range",
        )
    })?;
    if fader_id < 0 {
        return Err(error(
            ContentErrorCode::FieldRange,
            "SetFaderVolume fader_id must be >= 0",
        ));
    }
    let volume_db = items[1].as_f64().ok_or_else(|| {
        error(
            ContentErrorCode::FieldType,
            "SetFaderVolume volume_db must be a number",
        )
    })?;
    if !volume_db.is_finite() || !(VOLUME_MIN_DB..=VOLUME_MAX_DB).contains(&volume_db) {
        return Err(error(
            ContentErrorCode::FieldRange,
            "SetFaderVolume volume_db out of -150..=+50",
        ));
    }
    Ok((fader_id, volume_db))
}

/// 双路径（Aux1/Aux2）切换计划：**原子计算的模式+Fader 目标**，写入顺序保证任何中间态
/// 都不出现双上混（先关旧路径，再开新路径；中间最多 both-muted）。
#[derive(Clone, Debug, PartialEq)]
pub struct FaderSwitchPlan {
    /// 切换前绑定的内容模式。
    pub from_mode: ContentMode,
    /// 切换前 fader 绑定。
    pub from: FaderBinding,
    /// 目标模式。
    pub target_mode: ContentMode,
    /// 目标 fader 绑定。
    pub to: FaderBinding,
    /// 按顺序执行的写入（0 条 = 幂等，无需切换）。
    pub writes: Vec<FaderWrite>,
}

impl FaderSwitchPlan {
    /// 计算从 from 到目标模式的切换计划。
    ///
    /// 顺序：①关闭所有不该继续打开、当前打开的路径；②打开目标路径。
    /// 因此任意前缀状态都最多一条路径有声（不会双上混）；若 from 本身是 S1 已知的
    /// Camilla 0/0 双开启动态，也会先关闭再打开，第一次写入后即安全。
    pub fn plan(from_mode: ContentMode, from: FaderBinding, target_mode: ContentMode) -> Self {
        let to = FaderBinding::for_mode(target_mode);
        let mut writes = Vec::new();
        let mut current = from;
        // ① 关闭阶段：先 Aux1 后 Aux2；只关闭"当前开、目标要关"的路径。
        if current.aux_stereo_db == AUX_OPEN_DB && to.aux_stereo_db != AUX_OPEN_DB {
            writes.push(FaderWrite {
                fader_id: FADER_AUX_STEREO,
                volume_db: AUX_MUTE_DB,
            });
            current.aux_stereo_db = AUX_MUTE_DB;
        }
        if current.aux_pass_db == AUX_OPEN_DB && to.aux_pass_db != AUX_OPEN_DB {
            writes.push(FaderWrite {
                fader_id: FADER_AUX_PASS,
                volume_db: AUX_MUTE_DB,
            });
            current.aux_pass_db = AUX_MUTE_DB;
        }
        // ② 打开阶段：只打开目标路径；此时其它路径已关闭，不会双开。
        if to.aux_stereo_db == AUX_OPEN_DB && current.aux_stereo_db != AUX_OPEN_DB {
            writes.push(FaderWrite {
                fader_id: FADER_AUX_STEREO,
                volume_db: AUX_OPEN_DB,
            });
            current.aux_stereo_db = AUX_OPEN_DB;
        }
        if to.aux_pass_db == AUX_OPEN_DB && current.aux_pass_db != AUX_OPEN_DB {
            writes.push(FaderWrite {
                fader_id: FADER_AUX_PASS,
                volume_db: AUX_OPEN_DB,
            });
            current.aux_pass_db = AUX_OPEN_DB;
        }
        let _ = current;
        Self {
            from_mode,
            from,
            target_mode,
            to,
            writes,
        }
    }

    /// 是否无需切换（目标绑定 == 当前绑定）。
    pub fn is_idempotent(&self) -> bool {
        self.from == self.to
    }

    /// 是否安全（写完后 == 目标绑定，且任意中间态不双上混）。
    pub fn is_safe(&self) -> bool {
        self.validate().is_ok()
    }

    /// 校验：写入序列可执行、任意中间态不双上混、最后一条写入后 == 目标绑定。
    pub fn validate(&self) -> Result<(), ContentError> {
        self.from.validate()?;
        self.to.validate()?;
        let mut current = self.from;
        for write in &self.writes {
            current = apply_write(current, *write)?;
            if current.is_double_upmix() {
                return Err(error(
                    ContentErrorCode::DoubleUpmix,
                    "fader switch plan would double-open Aux1/Aux2",
                ));
            }
        }
        if current != self.to {
            return Err(error(
                ContentErrorCode::IncoherentBinding,
                "fader switch plan does not reach the target binding",
            ));
        }
        Ok(())
    }

    /// 每个写入之后的状态（不含初始态），用于断言无中间双上混。
    pub fn intermediate_bindings(&self) -> Vec<FaderBinding> {
        let mut current = self.from;
        let mut states = Vec::with_capacity(self.writes.len());
        for write in &self.writes {
            current = apply_write(current, *write).unwrap_or(current);
            states.push(current);
        }
        states
    }

    /// 计划对应的 W19 命令列表（JSON 文本），可直接喂 mock Camilla WS。
    pub fn commands_json(&self) -> Result<Vec<String>, ContentError> {
        self.writes
            .iter()
            .map(FaderWrite::to_command_text)
            .collect()
    }
}

fn apply_write(mut binding: FaderBinding, write: FaderWrite) -> Result<FaderBinding, ContentError> {
    if write.fader_id == FADER_AUX_STEREO {
        binding.aux_stereo_db = write.volume_db;
    } else if write.fader_id == FADER_AUX_PASS {
        binding.aux_pass_db = write.volume_db;
    } else {
        return Err(error(
            ContentErrorCode::InvalidFader,
            format!("unknown Aux fader id {}", write.fader_id),
        ));
    }
    binding.validate()?;
    Ok(binding)
}

/// 原子切换器：持有当前 ContentBinding；应用成功才把模式与 fader 一起提交，
/// 写入失败则进入失败安全（both-muted + auto），绝不留下"新模式 + 旧 fader"或双上混。
#[derive(Clone, Debug, PartialEq)]
pub struct ContentSwitcher {
    binding: ContentBinding,
}

impl ContentSwitcher {
    /// 以给定绑定构造（校验原子绑定不自洽则拒收）。
    pub fn new(binding: ContentBinding) -> Result<Self, ContentError> {
        binding.validate()?;
        Ok(Self { binding })
    }

    /// 安全默认构造：直通（auto 默认直通，对应推荐启动参数 --gain1=-120 --gain2=0）。
    pub fn safe_default() -> Self {
        Self {
            binding: ContentBinding::for_mode(ContentMode::Multichannel),
        }
    }

    /// S1 已知的 Camilla 默认启动态（Aux1/Aux2 均 0 dB = 双上混叠加）；不校验，
    /// 供模拟"启动瞬间双路径叠加"风险；首个安全计划会先关掉一条路径。
    pub fn camilla_startup_default() -> Self {
        Self {
            binding: ContentBinding {
                content_mode: ContentMode::Auto,
                faders: FaderBinding::camilla_startup_default(),
            },
        }
    }

    /// 当前原子绑定。
    pub fn binding(&self) -> &ContentBinding {
        &self.binding
    }

    /// 当前是否安全（非双上混）。
    pub fn is_safe(&self) -> bool {
        self.binding.faders.is_safe()
    }

    /// 到目标模式的切换计划（不执行）。
    pub fn plan_to(&self, mode: ContentMode) -> FaderSwitchPlan {
        self.binding.plan_to(mode)
    }

    /// 启动安全计划：从当前（可能 S1 双开启动态）到直通稳态；已安全且为直通时返回幂等计划。
    pub fn startup_safe_plan(&self) -> FaderSwitchPlan {
        self.binding.plan_to(ContentMode::Multichannel)
    }

    /// 原子应用目标模式：按计划顺序写 fader；全部成功后一次性提交（模式+fader 同值）。
    /// 任一次写入失败 → 绑定进入失败安全（both-muted + auto）并返回 fader_write_failed。
    pub fn apply<F>(
        &mut self,
        mode: ContentMode,
        mut write: F,
    ) -> Result<FaderSwitchPlan, ContentError>
    where
        F: FnMut(FaderWrite) -> Result<(), String>,
    {
        let plan = self.plan_to(mode);
        plan.validate()?;
        for item in &plan.writes {
            if let Err(detail) = write(*item) {
                self.binding = ContentBinding::safe_muted();
                return Err(error(
                    ContentErrorCode::FaderWriteFailed,
                    format!("SetFaderVolume({}) failed: {detail}", item.fader_id),
                ));
            }
        }
        self.binding = ContentBinding::for_mode(mode);
        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// S1 autodetect daemon 状态机（cmd_autodetect 等价语义；纯时间注入，无 sleep）
// ---------------------------------------------------------------------------

/// 一次 daemon 轮询的输出。
#[derive(Clone, Debug, PartialEq)]
pub struct ContentDaemonTick {
    /// 本轮 S1 运行态（只记录；P1 不消费）。
    pub state: ContentState,
    /// 本轮判定（含 active/confidence 等）。
    pub detection: DetectionResult,
    /// 需要下发的 fader 切换计划（None = 绑定已正确/或 dry-run；幂等计划不返回）。
    pub fader_plan: Option<FaderSwitchPlan>,
}

/// S1 内容检测 daemon 的纯状态机模拟（tspi-upmix autodetect）。
///
/// 语义：每 500 ms 取一次 RMS → detect → 启动冻结/override/hold 迟滞 → 写 state →
/// 按 content_mode 切 Aux1/Aux2（仅在 effective 位置变化时）。首轮 current=multichannel
/// （auto 默认直通）。本结构不睡眠、不读时钟：时间由 tick(now_ms, ...) 显式传入。
#[derive(Clone, Debug)]
pub struct ContentDaemon {
    config: ContentConfig,
    switcher: ContentSwitcher,
    current: ContentMode,
    override_mode: Option<ContentMode>,
    pending: Option<DetectedKind>,
    pending_since_ms: Option<f64>,
    detection: DetectionResult,
    fader_error: Option<String>,
    dry_run: bool,
    last_state: Option<ContentState>,
    container_layout: String,
}

impl ContentDaemon {
    /// 构造（校验配置；推荐安全启动：直通 fader 绑定）。
    pub fn new(config: ContentConfig) -> Result<Self, ContentError> {
        config.validate()?;
        Ok(Self {
            config,
            switcher: ContentSwitcher::safe_default(),
            current: ContentMode::Multichannel,
            override_mode: None,
            pending: None,
            pending_since_ms: None,
            detection: DetectionResult {
                detected: DetectedKind::Unknown,
                confidence: 0.0,
                active_channels: Vec::new(),
                rms_min_db: None,
                reason: ContentReason::NoData,
            },
            fader_error: None,
            dry_run: false,
            last_state: None,
            container_layout: S1_DEFAULT_CONTAINER_LAYOUT.to_string(),
        })
    }

    /// 构造 S1 已知不安全启动态（Camilla Aux 0/0 双开），用于模拟/验证启动安全计划。
    pub fn new_unsafe_startup(config: ContentConfig) -> Result<Self, ContentError> {
        let mut daemon = Self::new(config)?;
        daemon.switcher = ContentSwitcher::camilla_startup_default();
        Ok(daemon)
    }

    /// 设置覆盖模式（None=无覆盖文件；Some(Auto)=覆盖文件写 auto）。
    pub fn set_override(&mut self, override_mode: Option<ContentMode>) {
        self.override_mode = override_mode;
    }

    /// 当前覆盖值。
    pub fn override_mode(&self) -> Option<ContentMode> {
        self.override_mode
    }

    /// dry-run（只写 state、不下发 fader 计划）。
    pub fn set_dry_run(&mut self, dry_run: bool) {
        self.dry_run = dry_run;
    }

    /// 当前生效模式（daemon 维护的 current）。
    pub fn current_mode(&self) -> ContentMode {
        self.current
    }

    /// 当前原子绑定。
    pub fn switcher(&self) -> &ContentSwitcher {
        &self.switcher
    }

    /// 最近一轮 state。
    pub fn last_state(&self) -> Option<&ContentState> {
        self.last_state.as_ref()
    }

    /// 覆盖容器布局（默认 h16-7.1.4）。
    pub fn set_container_layout(&mut self, layout: impl Into<String>) {
        self.container_layout = layout.into();
    }

    /// 标记 fader 计划已成功应用（宿主在真实写成功后调用）。
    pub fn mark_faders_applied(&mut self, plan: &FaderSwitchPlan) -> Result<(), ContentError> {
        let binding = ContentBinding::for_mode(plan.target_mode);
        binding.validate()?;
        self.switcher = ContentSwitcher::new(binding)?;
        self.fader_error = None;
        Ok(())
    }

    /// 标记 fader 写入失败（宿主在任一次 SetFaderVolume 失败后调用）：进入失败安全
    /// （both-muted + auto），下一轮 state 的 detector=fallback、reason=fader-error: ...。
    pub fn mark_fader_error(&mut self, detail: impl Into<String>) {
        self.switcher = ContentSwitcher::new(ContentBinding::safe_muted())
            .unwrap_or_else(|_| ContentSwitcher::safe_default());
        self.fader_error = Some(detail.into());
    }

    /// 一轮 daemon 轮询（等价 S1 cmd_autodetect 循环体）。
    ///
    /// now_ms：距 daemon 启动的毫秒（单调，>=0）；rms_db：各角色 RMS（dB，可含缺失角色）；
    /// updated_unix：写 state 的时间戳（由调用方提供）。
    pub fn tick(
        &mut self,
        now_ms: f64,
        rms_db: &[(&str, f64)],
        updated_unix: u64,
    ) -> Result<ContentDaemonTick, ContentError> {
        if !now_ms.is_finite() || now_ms < 0.0 {
            return Err(error(
                ContentErrorCode::InvalidConfig,
                "now_ms must be finite and >= 0",
            ));
        }
        let detection = detect_content(rms_db, &self.config)?;
        let override_field = self.override_mode;
        let manual = matches!(
            override_field,
            Some(ContentMode::Stereo) | Some(ContentMode::Multichannel)
        );
        let frozen =
            self.config.startup_freeze_ms > 0 && now_ms < f64::from(self.config.startup_freeze_ms);
        let mut detector = ContentDetectorKind::Energy;
        let reason_string: String;
        let content_mode: ContentMode;
        if manual {
            let forced = override_field.unwrap_or(ContentMode::Auto);
            content_mode = forced;
            detector = ContentDetectorKind::Manual;
            reason_string = ContentReason::Override(forced).to_reason_string();
            self.pending = None;
            self.pending_since_ms = None;
        } else if frozen {
            content_mode = ContentMode::Multichannel;
            reason_string = REASON_STARTUP_FREEZE.to_string();
            self.pending = None;
            self.pending_since_ms = None;
        } else {
            let candidate = match detection.detected {
                DetectedKind::Stereo | DetectedKind::Multichannel => Some(detection.detected),
                DetectedKind::Unknown => None,
            };
            let current_kind = match self.current {
                ContentMode::Stereo => DetectedKind::Stereo,
                ContentMode::Multichannel => DetectedKind::Multichannel,
                ContentMode::Auto => DetectedKind::Unknown,
            };
            match candidate {
                None => {
                    self.pending = None;
                    self.pending_since_ms = None;
                    reason_string = detection.reason.to_reason_string();
                }
                Some(kind) if kind == current_kind => {
                    self.pending = None;
                    self.pending_since_ms = None;
                    reason_string = detection.reason.to_reason_string();
                }
                Some(kind) => {
                    if self.pending != Some(kind) {
                        self.pending = Some(kind);
                        self.pending_since_ms = Some(now_ms);
                    }
                    let held_ms = self
                        .pending_since_ms
                        .map(|since| now_ms - since)
                        .unwrap_or(0.0);
                    if held_ms >= f64::from(self.config.hold_ms) {
                        self.current = if kind == DetectedKind::Stereo {
                            ContentMode::Stereo
                        } else {
                            ContentMode::Multichannel
                        };
                        self.pending = None;
                        self.pending_since_ms = None;
                    }
                    reason_string = detection.reason.to_reason_string();
                }
            }
            content_mode = self.current;
        }
        // fader 错误：本轮 state 标 fallback + fader-error: detail（下一轮恢复能量检测）。
        let mut final_reason = reason_string;
        if let Some(detail) = self.fader_error.take() {
            detector = ContentDetectorKind::Fallback;
            final_reason = format!("{REASON_FADER_ERROR_PREFIX}: {detail}");
        }
        let state = self.build_state(
            content_mode,
            &detection,
            final_reason,
            detector,
            rms_db,
            updated_unix,
        );
        let fader_plan = if self.dry_run {
            None
        } else {
            let plan = self.switcher.plan_to(content_mode);
            if plan.writes.is_empty() || plan.is_idempotent() {
                None
            } else {
                Some(plan)
            }
        };
        self.detection = detection.clone();
        self.last_state = Some(state.clone());
        Ok(ContentDaemonTick {
            state,
            detection,
            fader_plan,
        })
    }

    /// Camilla WS 不可用/连接失败时的合成 state（S1 行为：auto/unknown/ws-error/fallback，
    /// 不下发 fader）。
    pub fn tick_ws_error(&mut self, detail: &str, updated_unix: u64) -> ContentDaemonTick {
        let state = ContentState {
            content_mode: ContentMode::Auto,
            detected: DetectedKind::Unknown,
            override_mode: self.override_mode,
            confidence: 0.0,
            active_channels: Vec::new(),
            rms_db: Vec::new(),
            threshold_db: self.config.threshold_db,
            hold_ms: self.config.hold_ms,
            reason: format!("{REASON_WS_ERROR_PREFIX}: {detail}"),
            h16_internal_upmix: None,
            container_layout: Some(self.container_layout.clone()),
            detector: ContentDetectorKind::Fallback,
            updated_unix,
        };
        self.last_state = Some(state.clone());
        ContentDaemonTick {
            state,
            detection: self.detection.clone(),
            fader_plan: None,
        }
    }

    fn build_state(
        &self,
        content_mode: ContentMode,
        detection: &DetectionResult,
        reason: String,
        detector: ContentDetectorKind,
        rms_db: &[(&str, f64)],
        updated_unix: u64,
    ) -> ContentState {
        let mut rounded: Vec<(String, f64)> = Vec::with_capacity(rms_db.len());
        for (role, db) in rms_db {
            if !db.is_finite() {
                continue;
            }
            rounded.push(((*role).to_string(), round_to(*db, 1)));
        }
        ContentState {
            content_mode,
            detected: detection.detected,
            override_mode: self.override_mode,
            confidence: detection.confidence,
            active_channels: detection.active_channels.clone(),
            rms_db: rounded,
            threshold_db: self.config.threshold_db,
            hold_ms: self.config.hold_ms,
            reason,
            h16_internal_upmix: detection.detected.h16_internal_upmix(),
            container_layout: Some(self.container_layout.clone()),
            detector,
            updated_unix,
        }
    }
}

// ---------------------------------------------------------------------------
// P1 显式拒绝（C07/R47；DEC §1/§6：P1 不实现内容自动上混，也不消费 S1 状态）
// ---------------------------------------------------------------------------

/// P1 播放模式对应的内容面（只描述"P1 该模式有没有内容判定/上混"，不含 DSP 图细节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P1PlaybackContent {
    /// 模式名（toslink / target_multichannel / aes67 / dante / off）。
    pub mode: &'static str,
    /// P1 是否消费内容检测（恒 false；内容检测属 S1，P1 不实现）。
    pub content_detection: bool,
    /// P1 是否做节目上混（恒 false）。
    pub upmix: bool,
    /// 是否直通映射（按目标布局/角色选择、零填；不做 upmix/downmix）。
    pub direct_mapping: bool,
    /// 是否 stereo strict（Toslink 2.0：FL/FR 直出，其余输出静音）。
    pub stereo_strict: bool,
}

impl P1PlaybackContent {
    /// 线上 JSON（供 REST/报告）。
    pub fn to_json(&self) -> JsonValue {
        json_object(vec![
            ("mode", JsonValue::String(self.mode.to_string())),
            ("content_detection", JsonValue::Bool(self.content_detection)),
            ("upmix", JsonValue::Bool(self.upmix)),
            ("direct_mapping", JsonValue::Bool(self.direct_mapping)),
            ("stereo_strict", JsonValue::Bool(self.stereo_strict)),
        ])
    }
}

/// P1 播放模式 → 内容面处置（T44 模式名；未知模式拒收，不猜）。
///
/// - toslink：stereo strict FL/FR 直出，无上混、无内容检测（T23/T44 self_check）；
/// - target_multichannel：按目标布局直通映射，无上混、无内容检测；
/// - aes67 / dante：AoIP 端点直通映射，无上混、无内容检测；
/// - off：无输出、无内容面；
/// - 其它 → unknown_playback_mode。
pub fn p1_playback_content(mode: &str) -> Result<P1PlaybackContent, ContentError> {
    let normalized = mode.trim().to_ascii_lowercase();
    let content = match normalized.as_str() {
        "toslink" => P1PlaybackContent {
            mode: "toslink",
            content_detection: false,
            upmix: false,
            direct_mapping: true,
            stereo_strict: true,
        },
        "target_multichannel" => P1PlaybackContent {
            mode: "target_multichannel",
            content_detection: false,
            upmix: false,
            direct_mapping: true,
            stereo_strict: false,
        },
        "aes67" => P1PlaybackContent {
            mode: "aes67",
            content_detection: false,
            upmix: false,
            direct_mapping: true,
            stereo_strict: false,
        },
        "dante" => P1PlaybackContent {
            mode: "dante",
            content_detection: false,
            upmix: false,
            direct_mapping: true,
            stereo_strict: false,
        },
        "off" => P1PlaybackContent {
            mode: "off",
            content_detection: false,
            upmix: false,
            direct_mapping: false,
            stereo_strict: false,
        },
        _ => {
            return Err(error(
                ContentErrorCode::UnknownPlaybackMode,
                format!("unknown P1 playback mode: {mode}"),
            ))
        }
    };
    Ok(content)
}

/// P1 input-mode GET 状态壳（compat-matrix R47 冻结）：
/// {"status":"not_supported","reason":"p1-no-content-upmix"}。
pub fn p1_input_mode_get_status() -> JsonValue {
    json_object(vec![
        (
            "status",
            JsonValue::String(P1_INPUT_MODE_STATUS.to_string()),
        ),
        (
            "reason",
            JsonValue::String(P1_NO_CONTENT_UPMMIX_REASON.to_string()),
        ),
    ])
}

/// P1 input-mode GET 状态壳文本。
pub fn p1_input_mode_get_status_text() -> Result<String, ContentError> {
    json_encode(&p1_input_mode_get_status())
        .map_err(|e| error(ContentErrorCode::Encode, format!("input-mode status: {e}")))
}

/// P1 input-mode PUT 拒绝（R47：400 not_supported；不删除用户旧文件、不写覆盖文件）。
pub fn p1_reject_input_mode_put(requested: &str) -> ContentError {
    error(
        ContentErrorCode::P1NotSupported,
        format!(
            "input-mode PUT '{}' rejected: {} (P1 has no content upmix)",
            requested, P1_NO_CONTENT_UPMMIX_REASON
        ),
    )
}

/// P1 不允许任何 S1 内容状态驱动播放切换：stereo（上混）与 multichannel（内容驱动直通）都拒绝。
/// 调用方只能使用显式用户模式（T44 toslink/target_multichannel 等）。
pub fn p1_apply_s1_content(content_mode: ContentMode) -> Result<(), ContentError> {
    Err(p1_not_supported(format!(
        "S1 content_mode {} cannot drive P1 playback: {}",
        content_mode.as_str(),
        P1_NO_CONTENT_UPMMIX_REASON
    )))
}

/// P1 内容模式折算：S1 解析出的 stereo 不会在 P1 变成上混路径，而是按显式播放模式直通
/// （toslink=FL/FR strict，target_multichannel=目标布局直通）。返回值恒为直通语义的
/// Multichannel（无上混路径），用于对拍/审计断言"P1 无双上混"。
pub const fn p1_direct_content_mode(_s1_mode: ContentMode) -> ContentMode {
    ContentMode::Multichannel
}

/// P1 是否允许 Aux 路径切换（恒 false；S1 fader 1/2 只作兼容资料，不得切上混路径）。
pub const fn p1_aux_fader_path_switch_supported() -> bool {
    false
}

/// P1 若请求 Aux 路径切换 → 显式拒绝（不得静默忽略）。
pub fn p1_reject_aux_fader_path_switch(fader_id: i32) -> ContentError {
    error(
        ContentErrorCode::P1NotSupported,
        format!(
            "Aux path switch via fader {} is retired in P1: {}",
            fader_id, P1_NO_CONTENT_UPMMIX_REASON
        ),
    )
}

/// T44 mode legacy status 的 input_content 字段：P1 恒 null（不消费 S1 状态）。
pub fn p1_legacy_status_input_content() -> JsonValue {
    JsonValue::Null
}
