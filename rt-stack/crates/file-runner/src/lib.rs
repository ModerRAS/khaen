//! [NONRT] TSPI 自研离线 file runner（crate `khaen-file-runner`）—— WBS **T14** 写域。
//!
//! 本 crate 是 P1 自研 RT-first DSP 栈的**文件输入执行器**（file runner）：解析/解包自有文件
//! 格式，把 core plan 在同一语义下执行，并产出 identity 保护与完整图版本/元数据。它**不直接
//! 占用 ALSA**：不打开声卡、不启动线程、不做 RT 准入，供 T26 host 把“文件解析 + 固定 buffer
//! source 接口”接到实际播放链（经有界 SPSC 按播放时轴驱动 DAC）。WBS T14 行：
//! “自有 file 解析/解包与 runner 执行 core+IR/flush/metadata；导出供 host 接实际播放的固定
//! buffer source 接口（不直接占 ALSA），锁精度与 version”。
//!
//! # 文件与数据合同（全部本 crate 自有实现，零第三方依赖）
//!
//! | 模块 | 文件 | 合同 |
//! | --- | --- | --- |
//! | `wav` | RIFF/WAVE 输入 + f32 系数/输出 WAV | 自有解析/解包/写回；采样格式与 T22 `rt-host::format` 逐位同语义（S16/S24/S32/F32）；固定 f32 精度 |
//! | `fir` | `tspi-fir/v1` manifest | 对齐 T55 reporter 的字段面与 `spec/fir-toolbox.md` §2.2 校验规则 1–7（taps/sha256/finite/reference/端点/预算） |
//! | `ir` | `tspi-cal-ir-draft/v1` 校准 IR 草案 | 对齐 T13 calibration adapter 的 version/hash/role/fs/taps/placement/P/phase/D、stale 六原因与 `PostEncoding` 二选一（禁止双叠） |
//! | `graph` / `session` | `khaen-file-runner/v1` session manifest | 对齐 T12 `core::compile` 的 `CompileInput`/`CompileOptions`/`CompiledPlans` 与 T09 `WorkerDagPlan`；完整图版本 = 规范化 SHA-256（含 coefficients/roles/fs/B/D/IR binding，不含 plan_version） |
//! | `runner` | 执行 + `khaen-file-runner-report/v1` 元数据 | core factorized 图经 T09 四 worker 静态 DAG 执行；flush = L_F−1（T03 SPEC §3.3）；identity 保护 = G=I 但保留 P_post（core-v1 §5.5 fallback） |
//! | `source` | 固定 buffer block source（host 接口） | `B x N` planar 块 + flush 零 + block 对齐；runner 与 T26 host 共用同一输入切分，不占 ALSA |
//!
//! # 明确非目标（NOT_RUN，不得据此声称）
//!
//! - 不碰板端/ALSA/CueMix/服务/模式；不 git、不改 crate 外 Cargo.toml/lib.rs、不改 spec/reference；
//! - 不实现上混/下混/自动造声道（A_map 只选择/零填，由 `core::compile::CompileInput::validate` 强制）；
//! - 不执行 fused 图（`FusionForm::FusedGp` 只作为 T12 设计期产物进入图版本/元数据；生产执行形态是
//!   factorized，fused 的 RT 执行归 T26/T56 hybrid DAG）；runner 元数据如实记录 chosen/reason，不假合并省算；
//! - 不做真实播放、不做 RT 时限/soak、不做跨架构数值差异实测。
//!
//! 权威输入（只读）：`rt-stack/spec/core-v1.md`（T02 v1.1.1 §6.3/§11/§12.3/§15.1）、
//! `rt-stack/reference/SPEC.md`（T03 §3 长度/尾/flush/identity 保护）、`rt-stack/spec/fir-toolbox.md`
//! （T04 §2/§7.2/§9）、`rt-stack/spec/compat-matrix.md`（T05 Y12–Y24）、
//! `scratch/lead/planner/{TSPI-RT-STACK-WBS.md,DSP-HOST-DECISION.md}`。
//!
//! # 精度与版本锁
//!
//! - **精度锁**：`precision` 必须是 `f32-strict`（[`PRECISION_LOCK`]）。系数按 `f64 as f32`
//!   单次舍入进图；执行/历史/输出全程 f32；无隐式 dither/clip/归一化（decode 层按 T22 语义
//!   sanitize/clip 并计数）；f64 只用于设计期/测试参考。
//! - **版本锁**：session 声明 `abi_version`（必须 =1）与 `plan_version`（>=1，进程内提交计数，
//!   **不**参与图 hash）；图 hash 是规范化 SHA-256，任何 role/A/G/P/IR/fs/B/D/保护策略变化都改变
//!   hash，而 `plan_version`/`created_unix`/`x_` 扩展字段不改变 hash（`graph_id` 可选；提供则必须匹配）。

#![forbid(unsafe_code)]

pub mod fir;
pub mod graph;
pub mod ir;
pub mod json;
pub mod runner;
pub mod session;
pub mod sha256;
pub mod source;
pub mod wav;

use std::fmt;

/// file runner session manifest schema（输入，`khaen-file-runner/v1`）。
pub const SESSION_SCHEMA: &str = "khaen-file-runner/v1";
/// 运行报告 schema（输出，`khaen-file-runner-report/v1`）。
pub const REPORT_SCHEMA: &str = "khaen-file-runner-report/v1";
/// 图版本 hash 算法标识（规范化序列化 + SHA-256）。
pub const GRAPH_HASH_ALGO: &str = "sha256-canonical-v1";
/// 图版本 hash 域前缀（避免与 plan graph_id / IR hash 混用）。
pub const GRAPH_HASH_DOMAIN: &str = "TSPI-FILE-RUNNER-GRAPH-v1";
/// 精度锁唯一合法值（f32 执行路径；f64 只作离线参考）。
pub const PRECISION_LOCK: &str = "f32-strict";
/// runner 名称（写进报告）。
pub const RUNNER_NAME: &str = "khaen-file-runner";
/// runner 版本（= crate 版本）。
pub const RUNNER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// JSON 解析最大嵌套深度（拒绝病态输入）。
pub const MAX_JSON_DEPTH: usize = 64;
/// 设计期 taps 上限（spec/fir-toolbox.md §2.1；>16384 只能离线设计）。
pub const MAX_DESIGN_TAPS: u32 = 16_384;
/// 常规 profile 每 route taps 上限（core-v1 §3.3 TAPS_NORMAL_MAX）。
pub const ROUTE_TAPS_REGULAR_MAX: u32 = 4_096;
/// extended profile 每 route taps 上限（core-v1 §3.3 TAPS_EXTENDED_MAX）。
pub const ROUTE_TAPS_EXTENDED_MAX: u32 = 16_384;

/// 保护策略：binding 失效时用 G=I 但保留 P_post 的 fallback 图（core-v1 §5.5）。
pub const FALLBACK_IDENTITY_P_POST: &str = "identity_p_post";
/// 保护策略：binding 失效/执行故障时安全静音（全零输出）。
pub const FALLBACK_MUTE: &str = "mute";
/// 触发原因：session 声明的 graph_id 与重算不一致（版本/stale）。
pub const TRIGGER_GRAPH_HASH_MISMATCH: &str = "graph_hash_mismatch";
/// 触发原因：校准 IR 与当前 fs/layout/roles/profile/target/calibration 不匹配（stale）。
pub const TRIGGER_IR_STALE: &str = "ir_stale";
/// 触发原因：`core::compile` 拒绝当前图（taps/预算/拓扑不可行）。
pub const TRIGGER_COMPILE_REJECT: &str = "compile_reject";
/// 触发原因：session 声明需要 IR 绑定但 manifest 未提供。
pub const TRIGGER_IR_ABSENT: &str = "ir_absent";

/// 稳定错误码：schema 不匹配/缺 schema。
pub const E_FILE_SCHEMA: &str = "E_FILE_SCHEMA";
/// JSON 语法/结构错误。
pub const E_FILE_JSON: &str = "E_FILE_JSON";
/// JSON 字段缺失/类型不符。
pub const E_FILE_FIELD: &str = "E_FILE_FIELD";
/// RIFF/WAVE 容器结构错误。
pub const E_FILE_WAV: &str = "E_FILE_WAV";
/// 采样格式/位深/编码不支持（不做隐式 24-in-32 等解释）。
pub const E_FILE_FORMAT: &str = "E_FILE_FORMAT";
/// 通道数不匹配/超容量。
pub const E_FILE_CHANNELS: &str = "E_FILE_CHANNELS";
/// 采样率不匹配/非 runtime rate。
pub const E_FILE_RATE: &str = "E_FILE_RATE";
/// 帧数/data 长度非整帧或越界。
pub const E_FILE_FRAMES: &str = "E_FILE_FRAMES";
/// 文件/manifest/系数 SHA-256 不一致（与 E_FIR_HASH 同义）。
pub const E_FILE_HASH: &str = "E_FILE_HASH";
/// taps 与文件样本数/支撑不符（与 E_FIR_TAPS 同义）。
pub const E_FILE_TAPS: &str = "E_FILE_TAPS";
/// 延迟语义不符（E_FIR_DELAY / IR delay_q16 非整数等）。
pub const E_FILE_DELAY: &str = "E_FILE_DELAY";
/// 系数/样本非有限或全零（与 E_FIR_NUMERIC 同义）。
pub const E_FILE_NUMERIC: &str = "E_FILE_NUMERIC";
/// 参考归一化不满足 ±0.01 dB（与 E_FIR_REF 同义）。
pub const E_FILE_REF: &str = "E_FILE_REF";
/// taps/延迟/RAM/融合长度超预算（与 E_FIR_BUDGET 同义，不静默截断）。
pub const E_FILE_BUDGET: &str = "E_FILE_BUDGET";
/// profile 不合法或未准入（regular/extended/offline_only）。
pub const E_FILE_PROFILE: &str = "E_FILE_PROFILE";
/// 校准 IR 草案结构/语义错误。
pub const E_FILE_IR: &str = "E_FILE_IR";
/// 校准 IR 与当前上下文 stale（STALE_FS/…）。
pub const E_FILE_IR_STALE: &str = "E_FILE_IR_STALE";
/// 精度锁不匹配（只接受 f32-strict）。
pub const E_FILE_PRECISION: &str = "E_FILE_PRECISION";
/// ABI/plan 版本不合法。
pub const E_FILE_VERSION: &str = "E_FILE_VERSION";
/// 图版本/hash/版本绑定不合法。
pub const E_FILE_GRAPH: &str = "E_FILE_GRAPH";
/// role/A_map/participants/scatter 不合法。
pub const E_FILE_ROLES: &str = "E_FILE_ROLES";
/// 路径表/系数槽不完整、重复或长度不符。
pub const E_FILE_PATH_TABLE: &str = "E_FILE_PATH_TABLE";
/// 容量/预算不足。
pub const E_FILE_CAPACITY: &str = "E_FILE_CAPACITY";
/// core 执行失败（整块/整图无效；绝不发布半图）。
pub const E_FILE_EXEC: &str = "E_FILE_EXEC";
/// 保护策略/fallback 图构造失败。
pub const E_FILE_PROTECTION: &str = "E_FILE_PROTECTION";
/// blob 源找不到/读取失败/路径逃逸。
pub const E_FILE_BLOB: &str = "E_FILE_BLOB";
/// 输出编码（WAV/metadata）失败。
pub const E_FILE_OUTPUT: &str = "E_FILE_OUTPUT";
/// 内部不变量（不应发生；发生时如实上报，不 panic）。
pub const E_FILE_INTERNAL: &str = "E_FILE_INTERNAL";

/// file-runner 稳定错误：[`RunnerError::code`] 是 `E_FILE_*` 字符串。
///
/// 本类型是 NONRT 控制/离线路径类型；可分配、可 Drop，**不得进入 RT 调用图**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerError {
    code: &'static str,
    message: String,
}

impl RunnerError {
    /// 构造错误（code 必须是稳定 `E_FILE_*`；message 为人类可读细节）。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人类可读细节（不含路径外的私有数据）。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RunnerError {}

/// file-runner 结果别名。
pub type RunnerResult<T> = Result<T, RunnerError>;

/// 未测项（写进每份报告，禁止把 NOT_RUN 说成已验证）。
pub const NOT_VERIFIED: [&str; 7] = [
    "board/ALSA/RT/soak: NOT_RUN (file runner never opens a device)",
    "real playback through MOTU: NOT_RUN (T26 host integration)",
    "fused graph execution: NOT_RUN (design-time artifact only; factorized core DAG executed)",
    "cross-arch numerical differences: NOT_RUN (f32 tolerance gate only)",
    "AES67/Dante/PTP: NOT_RUN (no network/clock access in this crate)",
    "power-loss/flush recovery: NOT_RUN (offline execution only)",
    "T13/T55 crate-level round-trip: NOT_RUN (data-contract alignment; no crate dependency added)",
];
