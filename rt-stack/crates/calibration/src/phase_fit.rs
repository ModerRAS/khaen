//! [NONRT] delay/polarity 实测估计 + 整数样本契约 + FIR 执行方向（WBS T65）—— 本文件是 WBS T65 写域。
//!
//! 本模块是新 daemon 校准链中「相对 delay / polarity 单 owner」：从**实测复频响**估计每输出角色的
//! 相对到达延迟与极性，给出置信度、参考通道/参考测点，并生成可直接交给 T40 的因果整数延迟 FIR 执行
//! op；未知/有歧义一律拒绝（**不猜**），并同时验证 FIR 执行的相位方向（T49 公共 D0 只扣一次）。
//!
//! # 职责边界（WBS T65 DoD）
//!
//! 1. **delay/polarity 实测估计**：T65 不采音、不解卷积（T62/T48 产物），只消费已经是「已去公共 D0」的
//!    T49 复频响；用幅度加权匹配滤波 + 相位斜率加权最小二乘估计每个 (测点, 角色) 的残余到达延迟，
//!    再用去延迟后的加权相干和估计极性符号与 margin。
//! 2. **置信/参考**：每通道给出延迟置信度（相位线性 RMS + 匹配峰突出度）与极性 margin/跨测点一致性；
//!    参考测点必须显式声明（单一 design 测点时可默认），参考通道 = 到达最晚（CONTRACT：D=0 通道）；
//!    其余 design 测点只用于极性一致性与诊断，**不参与**延迟应用的拟合（holdout 走廊不进本模块）。
//! 3. **相对整样本契约（CONTRACT §3.1）**：`D_i = round(max_j t_j − t_i)`（t=实测到达），施加到通道 i，
//!    `min(D)==0`，`D=0` 的通道是到达最晚的参考通道；单位采样点、整数、≥0、上限 floor(0.2·fs)；
//!    超过上限或到达搜索窗边界时**报错不截断**；`reference_index`（若给了 index 映射）指向 D=0 通道。
//! 4. **FIR 执行 phase 方向正确（T49 CommonD0 只扣一次）**：
//!    - 测量复响应约定（T49/T62 `fr.csv` 口径）：`X(f) = |X|·e^{+jφ}`，残余到达延迟 τ 贡献
//!      `φ(f) = +2π·f·τ/fs`（正相位斜率 = 更晚到达；对应 CONTRACT 的
//!      `absolute_phase_deg = phase_deg − 360·f·D0/fs` 与 DSP-HOST-DECISION §2.3「只扣 D0 一次」）。
//!    - 物理延迟 D 的 FIR（T40）按标准 DTFT 是 `H(ω)=e^{−jωD}`；本模块的测量复响应是其**共轭域**，
//!      所以把 FIR 作用到测量响应上必须乘 `conj(H) = e^{+jωD}`（极性 invert 再乘 −1）；**方向写反会
//!      把相对错位放大而不是抵消**——`execution` 校验用 T40 真实系数响应 `conj` 后作用到参考测点复响应，
//!      要求各通道残余到达差 ≤0.5 sample 且补偿后极性 = normal；测试用「错误方向」负向用例证明会失败。
//!    - 公共 D0：`DePhasedResponse` 只有两个构造入口（aligned_segment 去 D0 一次 / absolute 不再扣），
//!      类型层面没有二次去相位入口；fit 再校验 plan.phase_ref 与每个响应的 basis、delay_removed 与
//!      plan.common_d0 完全一致——同一 D0 只扣一次，未知 basis 直接拒绝。
//! 5. **产物**：确定性 `tspi-phase-fit/v1` report + `tspi-phase-ops/v1` ops（角色→整数 delay/极性，
//!    供 T66 编译）+ 每通道 T40 `PhaseDesign`（含系数、D、onset、taps、验证结果、系数 sha256）与
//!    `PhaseFitArtifacts`（job StageCtx / 目录写盘，路径 `phase/design-report.json`、`phase/ops.json`）。
//!
//! # 权威输入（只读）
//!
//! - WBS T65 行与 §1/§5/§6/§7（scratch/lead/planner/TSPI-RT-STACK-WBS.md）；
//! - DSP-HOST-DECISION §2.3（公共时间轴/clock_ppm、按实际 crop/coarse/frac/slot 恢复、
//!   不盲套 CSV 标签、不双扣 fractional delay、避免物理极性重复乘）与 §2.4（相对 delay/polarity、
//!   未知不猜）；
//! - calibration-spec/CONTRACT.md §2/§3.1（delay_samples 方向与 min=0/reference_index）、§3.2（极性 XOR）、
//!   §5 校验规则；calibration-spec/ACCEPTANCE.md §3（T1 逐通道延迟/极性精度）、§5（T3 残余延迟 ≤0.5 sample）、
//!   §6（T4 pair 相位/抵消）、§0.2（相对相位必须同一次采集/公共时间基准）、§0.3（测量时校准层清零）；
//! - rt-stack/spec/mimo-t6.md §4.1（只扣声明公共 D0；极性只依据声明字段，不得再次乘极性；未知极性→无效）
//!   与 §3.5（重复延迟极差/公共 D 残差门）；
//! - rt-stack/README.md §3 写域纪律；
//! - 上游实现（只读）：T63 `crate::positions`（MultipointPlan/质量门/design/validation 分组）、
//!   T49 `tspi_modal_design::timebase`（CommonD0/PhaseBasis 冻结语义）、T40 `tspi_fir_designer::phase`
//!   （整数延迟/极性 FIR 与解析验证）、T60 `crate::job`（StageCtx/artifact/sha256）。
//!
//! # 稳定错误码（REST/log/evidence 引用；未知一律拒收）
//!
//! | code | 语义 |
//! | --- | --- |
//! | E_PHF_CONFIG | 选项/字段非法（频带/阈值/`max_delay_ms`/搜索窗等） |
//! | E_PHF_FS | 采样率不是 P1 的 48k/96k |
//! | E_PHF_PLAN | 多点 plan 非法（T63 PositionsError 透传） |
//! | E_PHF_RESPONSE | 响应点非法（空/NaN/非递增/越 fs/2） |
//! | E_PHF_RESPONSE_UNKNOWN | 响应引用了 plan 之外/未知角色的测点 |
//! | E_PHF_RESPONSE_MISSING | design 测点×participant 覆盖不全（缺响应） |
//! | E_PHF_RESPONSE_DUPLICATE | 同一 (测点, 角色) 重复给响应 |
//! | E_PHF_HOLDOUT_CONTAMINATION | 输入含 validation/holdout 测点（holdout 不进拟合） |
//! | E_PHF_PHASE_BASIS | plan.phase_ref 与响应 basis 不一致（混淆 aligned/absolute） |
//! | E_PHF_COMMON_D0_MISMATCH | 响应已扣 D0 与 plan.common_d0 不一致（双扣/漏扣风险） |
//! | E_PHF_DOUBLE_DEPHASE | 同一响应被二次去相位（构造入口/anechoic 声明冲突） |
//! | E_PHF_QUALITY_MISSING / _BLOCKED / _STALE | design scope 质量门缺失/不允许继续/plan 变更后 stale |
//! | E_PHF_REFERENCE_POSITION | 多 design 测点却未显式声明参考测点 / 参考测点非法 |
//! | E_PHF_BAND | 频带内（或幅度掩码后）有效点不足 min_points |
//! | E_PHF_DELAY | 延迟估计非有限/越界（|τ| 超搜索窗语义不符） |
//! | E_PHF_DELAY_SEARCH | 匹配滤波峰值落在搜索窗边界（不猜，要求扩大搜索窗） |
//! | E_PHF_DELAY_LIMIT | 契约延迟超过 floor(0.2·fs)（CONTRACT 200 ms 上限，不截断） |
//! | E_PHF_DELAY_LOW_CONFIDENCE | 延迟置信度低于门（相位非线性/峰不突出） |
//! | E_PHF_POLARITY_UNKNOWN | 极性 margin 不足/跨测点未知（不得猜） |
//! | E_PHF_POLARITY_CONFLICT | 跨测点极性矛盾 / 与 T63 质量报告极性矛盾 |
//! | E_PHF_POLARITY_LOW_CONFIDENCE | 极性置信度低于门 |
//! | E_PHF_FIR | T40 FIR 设计/验证失败（预算/数值） |
//! | E_PHF_EXECUTION | FIR 执行方向校验失败（残余到达差超容差/补偿后极性非 normal） |
//! | E_PHF_NUMERIC | 内部数值下溢（零能量响应等） |
//!
//! # 未测（NOT_RUN，禁止当已验证）
//!
//! - 无真实 UMIK/MOTU/板端数据：测试全部为解析合成复响应夹具；无真实房间噪声/多径/削波/相干性；
//! - 不采音、不解卷积、不做 holdout 密封/两轮复现（T62/T67/T68/T69）；不判 T3/T4/T6 通过；
//! - 不打开 ALSA/不碰 CueMix/服务/模式/S1；不 git commit；不改 Cargo.toml/lib.rs/public_registration.rs/CLI，
//!   不新增依赖（只用 std + 既有 crate）。
//!
//! 私有测试：`tests/T65/phase_fit_cases.rs` 由本文件末尾 `#[cfg(test)] mod t65_tests { include!(...) }`
//! 编译执行（cargo 不自动发现 tests/<Txx>/；公共注册属 integrator，不改 public_registration.rs）。

use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use tspi_dsp_core::rt::SAMPLE_RATES_HZ;
use tspi_fir_designer::phase::{design_integer, IntegerDelaySpec, PhaseDesign, Polarity};
use tspi_modal_design::timebase::{CommonD0, PhaseBasis};

use crate::job::{sha256_hex, ArtifactKind, ArtifactRecord, StageCtx, StageFailure};
use crate::positions::{MultipointPlan, PositionQualityReport, QualityScope};

// ---------------------------------------------------------------------------
// schema / 常量
// ---------------------------------------------------------------------------

/// 报告 artifact schema（确定性 JSON；`phase/design-report.json`）。
pub const PHASE_FIT_SCHEMA: &str = "tspi-phase-fit/v1";
/// 执行 ops artifact schema（角色→integer delay/polarity；供 T66 编译）。
pub const PHASE_OPS_SCHEMA: &str = "tspi-phase-ops/v1";
/// report artifact 相对路径（job artifacts_dir 语义）。
pub const PHASE_FIT_ARTIFACT_PATH: &str = "phase/design-report.json";
/// ops artifact 相对路径。
pub const PHASE_OPS_ARTIFACT_PATH: &str = "phase/ops.json";
/// 工具名（report provenance）。
pub const PHASE_FIT_TOOL_NAME: &str = "tspi-phase-fit";
/// 工具版本（report provenance；实现版本冻结）。
pub const PHASE_FIT_TOOL_VERSION: &str = "t65-1";

/// CONTRACT 延迟上限（ms；channelmap v2 `delay_ms` 0..200）。
pub const MAX_CONTRACT_DELAY_MS: f64 = 200.0;
/// 每 session 最多通道/角色数（CONTRACT channels ≤32）。
pub const MAX_CHANNELS: usize = 32;
/// 频带内最少有效点数（少于该值不能估计，报 E_PHF_BAND）。
pub const MIN_BAND_POINTS: usize = 8;
/// 默认延迟估计频带（Hz；避开极低模态区与高频单元/声学相位弯曲）。
pub const DEFAULT_DELAY_BAND_HZ: [f64; 2] = [200.0, 4_000.0];
/// 默认极性估计频带（Hz）。
pub const DEFAULT_POLARITY_BAND_HZ: [f64; 2] = [80.0, 4_000.0];
/// 默认匹配滤波搜索半窗（ms；残余到达延迟 ±search）。
pub const DEFAULT_SEARCH_RANGE_MS: f64 = 50.0;
/// 搜索半窗上限（ms；超过即拒绝，避免无界搜索）。
pub const MAX_SEARCH_RANGE_MS: f64 = 500.0;
/// 幅度掩码动态范围（dB；频带内相对最大幅度低于该值的点不参与估计）。
pub const DEFAULT_MAGNITUDE_MASK_DB: f64 = 40.0;
/// 延迟置信度门（0..1；低于即 E_PHF_DELAY_LOW_CONFIDENCE）。
pub const DEFAULT_DELAY_CONFIDENCE_MIN: f64 = 0.5;
/// 极性 margin 门（0..1；低于即 E_PHF_POLARITY_UNKNOWN）。
pub const DEFAULT_POLARITY_MARGIN_MIN: f64 = 0.30;
/// 相位线性 RMS 归一门（deg；用于延迟置信度的 c_rms 子分）。
pub const DEFAULT_DELAY_PHASE_RMS_MAX_DEG: f64 = 30.0;
/// FIR 执行方向校验容差（sample；T1/T3 级 <=0.5）。
pub const DEFAULT_EXECUTION_TOLERANCE_SAMPLES: f64 = 0.5;
/// 参考通道歧义阈值（sample；max 与次大差小于该值 -> reference_ambiguous=true）。
pub const REFERENCE_AMBIGUITY_SAMPLES: f64 = 0.5;
/// 匹配滤波粗搜步长（sample；之后在 ±步长内 1-sample 细搜）。
pub const COARSE_SEARCH_STEP_SAMPLES: i64 = 8;
/// 亚采样相位斜率精修允许的最大 |ε|（sample；超出视为提取不可靠）。
pub const SUB_SAMPLE_REFINE_LIMIT: f64 = 1.0;
/// 通道 index 合理性上界（设备通道域 sanity，不是 MOTU 22ch 硬编码）。
pub const MAX_DEVICE_CHANNEL_INDEX: u32 = 256;

/// 测量复响应相位约定（report/ops 都写入，防止下游再扣一次相位或反号）。
pub const PHASE_CONVENTION_STATEMENT: &str = "measured X(f)=|X|*exp(+j*phi)，残余到达延迟 tau 贡献 phi=+2*pi*f*tau/fs（正斜率=更晚到达；T49 de_phase_analysis 已按声明 CommonD0 只扣一次）；物理延迟 D 的 FIR 在标准 DTFT 下 H=e^{-j*w*D}，故作用到本测量复响应必须乘 conj(H)=e^{+j*w*D}，方向写反会把错位放大";
/// 整数样本契约（report 写入，防止把「相对最早到达」误当 D）。
pub const DELAY_CONTRACT_STATEMENT: &str = "D_i = round(max_j t_j - t_i)（施加到通道 i 的延迟补偿；t=实测到达）；min(D)=0；D=0 的通道=到达最晚=参考通道；单位 samples@fs 的整数，0<=D<=floor(0.2*fs)；超过上限报错不截断";
/// 极性基准声明（report 写入；T1 电学环回不覆盖声学绝对极性，C-P1-12）。
pub const POLARITY_BASIS_STATEMENT: &str = "极性符号按 UMIK-1 手册约定（正压->正样本）解释：invert=该通道实测物理极性与约定相反，应用时乘 -1；ACCEPTANCE T1 只标定 MOTU 播放/采集段，不覆盖声学绝对极性（C-P1-12），本模块只给约定下的 correction，不宣称声学绝对极性已标定";

/// FIR 执行方向说明（report 写入）。
pub const FIR_EXECUTION_STATEMENT: &str = "每通道执行 op = integer delta delay D_i + 极性 invert（T40 design_integer）；执行校验把 T40 真实 FIR 系数响应 conj 后作用到参考测点复响应，要求各通道残余到达差 <= tolerance sample 且补偿后极性=normal；错误方向（乘 H 而非 conj(H)）在负向用例中必须失败";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// T65 [NONRT] 错误：稳定错误码 + 可读消息（未知/歧义一律拒绝，不猜）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseFitError {
    /// 稳定错误码（见模块文档表）。
    pub code: &'static str,
    /// 人读细节。
    pub message: String,
}

impl PhaseFitError {
    /// 构造。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        PhaseFitError {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for PhaseFitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PhaseFitError {}

fn phase_err<T>(code: &'static str, message: impl Into<String>) -> Result<T, PhaseFitError> {
    Err(PhaseFitError::new(code, message))
}

fn plan_error(error: crate::positions::PositionsError) -> PhaseFitError {
    PhaseFitError::new("E_PHF_PLAN", error.to_string())
}

// ---------------------------------------------------------------------------
// 复响应（已去公共 D0 一次） / 去相位构造入口
// ---------------------------------------------------------------------------

/// 单个频率点的复响应（线性幅度域；T49 口径 `X = |X|·e^{+jφ}`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseFitPoint {
    /// 频率（Hz，>0 且 <fs/2）。
    pub freq_hz: f64,
    /// 实部。
    pub re: f64,
    /// 虚部。
    pub im: f64,
}

impl PhaseFitPoint {
    /// 构造（只做字段赋值；序列级校验见各 response 构造入口）。
    pub fn new(freq_hz: f64, re: f64, im: f64) -> Self {
        PhaseFitPoint { freq_hz, re, im }
    }

    /// 线性幅度。
    pub fn magnitude(&self) -> f64 {
        self.re.hypot(self.im)
    }

    /// 主值相位（deg，(-180,180]）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }
}

/// **已去公共 D0 一次**的实测复响应（构造入口只有两个；类型层面没有二次去相位入口）。
///
/// - [DePhasedResponse::from_aligned_segment]：producer 相位含传播延迟（`aligned_segment`），
///   本模块按声明 `CommonD0` **只扣一次**（complex *= e^{-j·2π·f·D0/fs}，与 T49 `de_phase_analysis` 同式）；
/// - [DePhasedResponse::from_absolute]：`absolute` 语义（相位已是总相位），**绝不再扣** D0/delay
///   （`delay_removed_samples=0`）。
///
/// 与 T49 相同：common D0 是唯一去相位值；T48 `analysis.delay_samples` 只作 provenance，不进入相位扣除。
#[derive(Clone, Debug, PartialEq)]
pub struct DePhasedResponse {
    position: String,
    channel: String,
    points: Vec<PhaseFitPoint>,
    basis: PhaseBasis,
    delay_removed_samples: f64,
}

impl DePhasedResponse {
    /// 由 `aligned_segment` 原始相位构造：按 [CommonD0] **扣一次**公共整数 D0。
    ///
    /// 输入原始点相位约定与 T49 一致（延迟贡献 +2π·f·t/fs）；输出点已去 D0，残余到达延迟 τ 仍以
    /// `+2π·f·τ/fs` 表示。`fs` 必须是 P1 的 48k/96k。
    pub fn from_aligned_segment(
        position: impl Into<String>,
        channel: impl Into<String>,
        fs: u32,
        raw_points: Vec<PhaseFitPoint>,
        common_d0: CommonD0,
    ) -> Result<Self, PhaseFitError> {
        validate_sample_rate(fs)?;
        validate_response_points(&raw_points, fs)?;
        let d0 = common_d0.as_f64();
        let fs_f = fs as f64;
        let mut points = Vec::with_capacity(raw_points.len());
        for point in raw_points {
            let rotation = -2.0 * std::f64::consts::PI * point.freq_hz * d0 / fs_f;
            let (sin, cos) = rotation.sin_cos();
            points.push(PhaseFitPoint {
                freq_hz: point.freq_hz,
                re: point.re * cos - point.im * sin,
                im: point.re * sin + point.im * cos,
            });
        }
        Ok(DePhasedResponse {
            position: position.into(),
            channel: channel.into(),
            points,
            basis: PhaseBasis::AlignedSegment,
            delay_removed_samples: d0,
        })
    }

    /// 由 `absolute` 相位构造：**不再扣任何** D0/delay（否则就是双去相位）。
    pub fn from_absolute(
        position: impl Into<String>,
        channel: impl Into<String>,
        fs: u32,
        points: Vec<PhaseFitPoint>,
    ) -> Result<Self, PhaseFitError> {
        validate_sample_rate(fs)?;
        validate_response_points(&points, fs)?;
        Ok(DePhasedResponse {
            position: position.into(),
            channel: channel.into(),
            points,
            basis: PhaseBasis::Absolute,
            delay_removed_samples: 0.0,
        })
    }

    /// 测点 label。
    pub fn position(&self) -> &str {
        &self.position
    }

    /// 角色/通道名。
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// 已去相位的频率点（升序）。
    pub fn points(&self) -> &[PhaseFitPoint] {
        &self.points
    }

    /// 相位基准语义（aligned_segment / absolute）。
    pub fn basis(&self) -> PhaseBasis {
        self.basis
    }

    /// 本响应已从相位扣除的延迟（sample；aligned_segment=声明的 D0，absolute=0）。
    pub fn delay_removed_samples(&self) -> f64 {
        self.delay_removed_samples
    }
}

fn validate_sample_rate(fs: u32) -> Result<(), PhaseFitError> {
    if SAMPLE_RATES_HZ.contains(&fs) {
        Ok(())
    } else {
        phase_err(
            "E_PHF_FS",
            format!("sample rate {fs} Hz is not a P1 rate {SAMPLE_RATES_HZ:?}"),
        )
    }
}

fn validate_response_points(points: &[PhaseFitPoint], fs: u32) -> Result<(), PhaseFitError> {
    if points.is_empty() {
        return phase_err(
            "E_PHF_RESPONSE",
            "response needs at least 1 finite frequency point",
        );
    }
    let nyquist = fs as f64 / 2.0;
    let mut previous = f64::NEG_INFINITY;
    for (index, point) in points.iter().enumerate() {
        if !point.freq_hz.is_finite() || !point.re.is_finite() || !point.im.is_finite() {
            return phase_err(
                "E_PHF_RESPONSE",
                format!("point {index} is not finite: {}", point.freq_hz),
            );
        }
        if !(point.freq_hz > 0.0) || point.freq_hz >= nyquist {
            return phase_err(
                "E_PHF_RESPONSE",
                format!(
                    "point {index} frequency {} Hz outside (0, fs/2={nyquist})",
                    point.freq_hz
                ),
            );
        }
        if point.freq_hz <= previous {
            return phase_err(
                "E_PHF_RESPONSE",
                format!("frequencies must be strictly increasing (point {index})"),
            );
        }
        previous = point.freq_hz;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 每通道频带覆盖 / index 映射 / 选项 / 请求
// ---------------------------------------------------------------------------

/// 单通道估计频带覆盖（缺省用 options 的 delay/polarity 频带；低频专用通道用窄带）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChannelBand {
    /// 角色/通道名（必须在 plan.participants 内）。
    pub channel: String,
    /// 延迟与极性的共同估计频带（Hz）。
    pub band_hz: [f64; 2],
}

impl PhaseChannelBand {
    /// 构造。
    pub fn new(channel: impl Into<String>, band_hz: [f64; 2]) -> Self {
        PhaseChannelBand {
            channel: channel.into(),
            band_hz,
        }
    }

    /// 字段级校验（越界拒绝；不猜）。
    pub fn validate(&self, fs: u32) -> Result<(), PhaseFitError> {
        validate_band_hz(self.band_hz, fs, "channel_bands")
    }
}

/// 角色→设备通道号映射（CONTRACT `channels[].index`；可选，未知不猜）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseChannelIndex {
    /// 角色/通道名。
    pub channel: String,
    /// 播放设备通道号（0-based，MOTU 物理通道域）。
    pub index: u32,
}

impl PhaseChannelIndex {
    /// 构造。
    pub fn new(channel: impl Into<String>, index: u32) -> Self {
        PhaseChannelIndex {
            channel: channel.into(),
            index,
        }
    }
}

/// delay/polarity 拟合选项（全部显式写进 report，可复现）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFitOptions {
    /// 延迟估计频带（Hz）；单通道可被 [PhaseChannelBand] 覆盖。
    pub delay_band_hz: [f64; 2],
    /// 极性估计频带（Hz）；单通道可被 [PhaseChannelBand] 覆盖。
    pub polarity_band_hz: [f64; 2],
    /// 参考测点 label（应用延迟的参考位置）；多 design 测点必须显式给。
    pub reference_position: Option<String>,
    /// 契约延迟上限（ms；>0 且 <=200）。
    pub max_delay_ms: f64,
    /// 匹配滤波搜索半窗（ms；±search，超过峰边界即报错不猜）。
    pub search_range_ms: f64,
    /// 幅度掩码动态范围（dB；频带内相对最大幅度低于该值的点不参与估计）。
    pub magnitude_mask_db: f64,
    /// 频带内最少有效点数。
    pub min_points: usize,
    /// 延迟置信度门（0..1）。
    pub delay_confidence_min: f64,
    /// 极性 margin 门（0..1）。
    pub polarity_margin_min: f64,
    /// 相位线性 RMS 归一门（deg；延迟置信度子分用）。
    pub delay_phase_rms_max_deg: f64,
    /// FIR 执行方向校验容差（sample）。
    pub execution_tolerance_samples: f64,
    /// 是否要求 T63 design scope 质量门（缺 SNR/clock/phase 不继续）。
    pub require_design_quality: bool,
}

impl Default for PhaseFitOptions {
    fn default() -> Self {
        PhaseFitOptions {
            delay_band_hz: DEFAULT_DELAY_BAND_HZ,
            polarity_band_hz: DEFAULT_POLARITY_BAND_HZ,
            reference_position: None,
            max_delay_ms: MAX_CONTRACT_DELAY_MS,
            search_range_ms: DEFAULT_SEARCH_RANGE_MS,
            magnitude_mask_db: DEFAULT_MAGNITUDE_MASK_DB,
            min_points: MIN_BAND_POINTS,
            delay_confidence_min: DEFAULT_DELAY_CONFIDENCE_MIN,
            polarity_margin_min: DEFAULT_POLARITY_MARGIN_MIN,
            delay_phase_rms_max_deg: DEFAULT_DELAY_PHASE_RMS_MAX_DEG,
            execution_tolerance_samples: DEFAULT_EXECUTION_TOLERANCE_SAMPLES,
            require_design_quality: true,
        }
    }
}

impl PhaseFitOptions {
    /// 严格校验（越界全部拒绝；返回非阻塞 warning）。
    pub fn validate(&self, fs: u32) -> Result<Vec<String>, PhaseFitError> {
        validate_sample_rate(fs)?;
        validate_band_hz(self.delay_band_hz, fs, "delay_band_hz")?;
        validate_band_hz(self.polarity_band_hz, fs, "polarity_band_hz")?;
        let mut warnings = Vec::new();
        if let Some(reference) = &self.reference_position {
            if reference.trim().is_empty() || reference.trim() != reference.as_str() {
                return phase_err(
                    "E_PHF_CONFIG",
                    "reference_position must be non-empty and trimmed",
                );
            }
        }
        if !self.max_delay_ms.is_finite()
            || self.max_delay_ms <= 0.0
            || self.max_delay_ms > MAX_CONTRACT_DELAY_MS + 1e-9
        {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "max_delay_ms={} outside (0, {MAX_CONTRACT_DELAY_MS}] (CONTRACT delay cap)",
                    self.max_delay_ms
                ),
            );
        }
        if !self.search_range_ms.is_finite()
            || self.search_range_ms <= 0.0
            || self.search_range_ms > MAX_SEARCH_RANGE_MS + 1e-9
        {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "search_range_ms={} outside (0, {MAX_SEARCH_RANGE_MS}]",
                    self.search_range_ms
                ),
            );
        }
        if !self.magnitude_mask_db.is_finite() || !(0.0..=120.0).contains(&self.magnitude_mask_db) {
            return phase_err(
                "E_PHF_CONFIG",
                "magnitude_mask_db must be finite in [0,120]",
            );
        }
        if self.min_points < MIN_BAND_POINTS || self.min_points > 4096 {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "min_points={} outside [{MIN_BAND_POINTS}, 4096]",
                    self.min_points
                ),
            );
        }
        for (name, value) in [
            ("delay_confidence_min", self.delay_confidence_min),
            ("polarity_margin_min", self.polarity_margin_min),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return phase_err(
                    "E_PHF_CONFIG",
                    format!("{name}={value} must be finite in [0,1]"),
                );
            }
        }
        if !self.delay_phase_rms_max_deg.is_finite() || self.delay_phase_rms_max_deg <= 0.0 {
            return phase_err(
                "E_PHF_CONFIG",
                "delay_phase_rms_max_deg must be finite and > 0",
            );
        }
        if !self.execution_tolerance_samples.is_finite()
            || self.execution_tolerance_samples <= 0.0
            || self.execution_tolerance_samples > 2.0
        {
            return phase_err(
                "E_PHF_CONFIG",
                "execution_tolerance_samples must be finite in (0,2]",
            );
        }
        if !self.require_design_quality {
            warnings.push(
                "require_design_quality=false：未强制 T63 design scope SNR/clock/phase 质量门（仅开发/单元测试）"
                    .to_string(),
            );
        }
        Ok(warnings)
    }
}

fn validate_band_hz(band_hz: [f64; 2], fs: u32, what: &str) -> Result<(), PhaseFitError> {
    let [lo, hi] = band_hz;
    let hi_cap = 20_000.0_f64.min(0.499 * fs as f64);
    if !lo.is_finite() || !hi.is_finite() || lo < 20.0 || hi > hi_cap + 1e-9 || !(hi > lo) {
        return phase_err(
            "E_PHF_CONFIG",
            format!("{what}=[{lo},{hi}] invalid: require 20<=lo<hi<={hi_cap:.3} (fs={fs})"),
        );
    }
    Ok(())
}

fn resolve_channel_band(
    options: &PhaseFitOptions,
    channel_bands: &[PhaseChannelBand],
    channel: &str,
    polarity_band: bool,
) -> [f64; 2] {
    if let Some(entry) = channel_bands.iter().find(|entry| entry.channel == channel) {
        return entry.band_hz;
    }
    if polarity_band {
        options.polarity_band_hz
    } else {
        options.delay_band_hz
    }
}

/// 一次 delay/polarity 拟合请求（只读借入 plan/实测复响应/质量门）。
pub struct PhaseFitRequest<'a> {
    /// 多点 plan（提供 session/layout/fs/participants/common_d0/phase_ref/design 分组）。
    pub plan: &'a MultipointPlan,
    /// 已去公共 D0 一次的实测复响应（按 plan 的 design 测点 × participants 完整覆盖）。
    pub responses: &'a [DePhasedResponse],
    /// 单通道频带覆盖（可选；缺省用 options 频带）。
    pub channel_bands: &'a [PhaseChannelBand],
    /// 角色→设备通道号映射（可选；CONTRACT index，未知不猜）。
    pub channel_indices: &'a [PhaseChannelIndex],
    /// T63 design scope 质量报告（require_design_quality=true 时必需）。
    pub design_quality: Option<&'a PositionQualityReport>,
    /// 选项。
    pub options: PhaseFitOptions,
    /// producer 写入的创建时间（u64；只做 provenance，不参与确定性 hash）。
    pub created_unix: u64,
}

impl<'a> PhaseFitRequest<'a> {
    /// 构造（缺省 options/无 frequency 覆盖/无 index 映射）。
    pub fn new(plan: &'a MultipointPlan, responses: &'a [DePhasedResponse]) -> Self {
        PhaseFitRequest {
            plan,
            responses,
            channel_bands: &[],
            channel_indices: &[],
            design_quality: None,
            options: PhaseFitOptions::default(),
            created_unix: 0,
        }
    }

    /// 设置选项。
    pub fn with_options(mut self, options: PhaseFitOptions) -> Self {
        self.options = options;
        self
    }

    /// 设置单通道频带覆盖。
    pub fn with_channel_bands(mut self, channel_bands: &'a [PhaseChannelBand]) -> Self {
        self.channel_bands = channel_bands;
        self
    }

    /// 设置角色→设备通道号映射。
    pub fn with_channel_indices(mut self, channel_indices: &'a [PhaseChannelIndex]) -> Self {
        self.channel_indices = channel_indices;
        self
    }

    /// 设置 T63 design scope 质量报告。
    pub fn with_design_quality(mut self, quality: &'a PositionQualityReport) -> Self {
        self.design_quality = Some(quality);
        self
    }

    /// 设置 created_unix。
    pub fn with_created_unix(mut self, created_unix: u64) -> Self {
        self.created_unix = created_unix;
        self
    }
}

// ---------------------------------------------------------------------------
// 公开报告结构（tspi-phase-fit/v1 / tspi-phase-ops/v1）
// ---------------------------------------------------------------------------

/// quality 门摘要。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFitQualitySummary {
    /// 是否要求质量门。
    pub required: bool,
    /// 是否提供报告。
    pub present: bool,
    /// scope 机器名（design）。
    pub scope: String,
    /// 质量报告的 plan_sha256（stale 判定）。
    pub plan_sha256: String,
    /// 是否允许继续拟合。
    pub continue_allowed: bool,
    /// 可用测点数。
    pub points_usable: usize,
    /// 总测点数。
    pub points_total: usize,
}

/// 单（design）测点的延迟/极性估计报告。
#[derive(Clone, Debug, PartialEq)]
pub struct PhasePositionReport {
    /// 测点 label。
    pub position: String,
    /// 该测点估计的残余到达延迟（samples；可为负=早于声明 D0）。
    pub delay_raw_samples: f64,
    /// 延迟置信度（0..1；相位线性 RMS 与匹配峰突出度取小）。
    pub delay_confidence: f64,
    /// 去整数延迟后的相位线性 RMS（deg）。
    pub delay_phase_rms_deg: f64,
    /// 匹配滤波峰/均值突出度。
    pub delay_prominence: f64,
    /// 参与延迟估计的频点数。
    pub delay_points_used: usize,
    /// 该测点测得极性（+1/-1；低于 margin 门时为 null，不猜）。
    pub polarity: Option<i32>,
    /// 极性 margin（0..1）。
    pub polarity_margin: f64,
    /// 参与极性估计的频点数。
    pub polarity_points_used: usize,
}

/// 每通道 T40 FIR 执行声明。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFirReport {
    /// 整数延迟 D（samples）。
    pub delay_samples: u32,
    /// 设计极性（校准 invert 语义：normal/invert）。
    pub polarity: Polarity,
    /// 支撑 taps（D+1，最小因果支撑）。
    pub taps: usize,
    /// 有效支撑起点（onset，samples；=D）。
    pub onset_samples: usize,
    /// delay_ms = D/fs*1000。
    pub delay_ms: f64,
    /// 系数 canonical sha256（T66/apply 可对拍）。
    pub coefficients_sha256: String,
    /// T40 声明频带判据是否通过（design.verify().passed()）。
    pub verification_passed: bool,
    /// 是否 §4.2 全带强制。
    pub spec_band_required: bool,
    /// 是否 §4.2 全带合规。
    pub spec_band_compliant: bool,
}

/// 每通道 FIR 执行方向校验（参考测点）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseExecutionReport {
    /// 补偿后该通道残余到达延迟（samples）。
    pub residual_delay_samples: f64,
    /// 补偿后参考通道残余到达延迟（samples）。
    pub reference_residual_delay_samples: f64,
    /// residual − reference（samples；方向正确时应 ≈0）。
    pub delta_samples: f64,
    /// |delta| <= tolerance。
    pub within_tolerance: bool,
    /// 补偿后测得极性（+1/-1；null=未知）。
    pub compensated_polarity_sign: Option<i32>,
    /// 补偿后极性 margin。
    pub compensated_polarity_margin: f64,
    /// 方向正确（within_tolerance 且补偿后极性=+1）。
    pub direction_ok: bool,
}

/// 单通道拟合报告。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChannelReport {
    /// 角色/通道名。
    pub channel: String,
    /// 设备通道号（CONTRACT index；未提供为 null）。
    pub index: Option<u32>,
    /// 参考测点上的原始相对延迟（samples；>=0，fractional）。
    pub delay_raw_samples: f64,
    /// 契约整数延迟补偿（samples；D_i = round(max tau − tau_i)，min=0）。
    pub delay_samples: u32,
    /// delay_ms = delay_samples/fs*1000。
    pub delay_ms: f64,
    /// 亚采样残差 = raw − delay（[-0.5, 0.5]）。
    pub fractional_residual_samples: f64,
    /// 延迟置信度（参考测点）。
    pub delay_confidence: f64,
    /// 是否参考通道（D=0、到达最晚）。
    pub reference: bool,
    /// 校准极性修正（invert = 应用时乘 −1）。
    pub polarity: Polarity,
    /// 实测物理极性符号（+1=与 UMIK 正压约定同相；−1=反相）。
    pub measured_polarity_sign: i32,
    /// 极性置信度（各 design 测点 margin 的最小值）。
    pub polarity_confidence: f64,
    /// 极性 margin 最小值。
    pub polarity_margin_min: f64,
    /// 与 T63 质量报告极性是否一致（未提供/未知=null）。
    pub producer_polarity_agrees: Option<bool>,
    /// 全部 design 测点估计（应用用参考测点；其余只作一致性/诊断）。
    pub positions: Vec<PhasePositionReport>,
    /// T40 FIR 执行声明。
    pub fir: PhaseFirReport,
    /// FIR 执行方向校验。
    pub execution: PhaseExecutionReport,
}

/// 全局检查位（all_pass 是 T65 内部可交付判据，不等于 T3/T4/T6 通过）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFitChecks {
    /// 契约整数：raw−round(raw) 在 ±0.5 sample 内。
    pub integer_delay_contract: bool,
    /// 全通道 min(delay_samples)==0。
    pub min_zero: bool,
    /// 全通道 delay_samples <= floor(0.2·fs)。
    pub max_within_cap: bool,
    /// 参考通道 D==0 且其 tau 为最大。
    pub reference_is_latest: bool,
    /// 全通道 |fractional_residual|<=0.5。
    pub fractional_residual_within_half_sample: bool,
    /// 全通道极性已确定（未知已报错，成功路径恒 true）。
    pub polarity_known: bool,
    /// 全通道 T40 FIR 设计/验证通过。
    pub fir_design_ok: bool,
    /// 全通道 FIR 执行方向校验通过。
    pub fir_direction_ok: bool,
    /// 全部检查通过。
    pub all_pass: bool,
}

/// 选项报告（可复现）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFitOptionsReport {
    /// 频带（Hz）。
    pub delay_band_hz: [f64; 2],
    /// 频带（Hz）。
    pub polarity_band_hz: [f64; 2],
    /// 参考测点。
    pub reference_position: String,
    /// 延迟上限（ms）。
    pub max_delay_ms: f64,
    /// 搜索半窗（ms）。
    pub search_range_ms: f64,
    /// 幅度掩码（dB）。
    pub magnitude_mask_db: f64,
    /// 最少频点。
    pub min_points: usize,
    /// 延迟置信度门。
    pub delay_confidence_min: f64,
    /// 极性 margin 门。
    pub polarity_margin_min: f64,
    /// 相位 RMS 归一门（deg）。
    pub delay_phase_rms_max_deg: f64,
    /// 执行容差（sample）。
    pub execution_tolerance_samples: f64,
    /// 是否强制质量门。
    pub require_design_quality: bool,
}

/// 一次完整 delay/polarity 拟合报告（tspi-phase-fit/v1）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseFitReport {
    /// schema 常量。
    pub schema: &'static str,
    /// 工具名。
    pub tool_name: &'static str,
    /// 工具版本。
    pub tool_version: &'static str,
    /// 创建时间（producer provenance）。
    pub created_unix: u64,
    /// 采样率（Hz）。
    pub fs: u32,
    /// session id。
    pub session_id: String,
    /// 布局 id。
    pub layout: String,
    /// plan 自 hash。
    pub plan_sha256: String,
    /// 测量输入 canonical hash。
    pub measurement_sha256: String,
    /// T6 FC-04 声明公共整数 D0。
    pub common_d0_samples: i64,
    /// plan 相位基准语义（aligned_segment/absolute）。
    pub phase_basis: String,
    /// 参考测点 label。
    pub reference_position: String,
    /// 参考通道（D=0，到达最晚）。
    pub reference_channel: String,
    /// 参考通道设备 index（未提供映射=null）。
    pub reference_index: Option<u32>,
    /// 参考通道与次晚通道的到达差（samples；单通道时 null）。
    pub reference_tie_margin_samples: Option<f64>,
    /// 参考通道是否歧义（tie margin < 0.5 sample）。
    pub reference_ambiguous: bool,
    /// 选项。
    pub options: PhaseFitOptionsReport,
    /// 质量门摘要。
    pub quality: PhaseFitQualitySummary,
    /// 极性基准声明（UMIK 正压约定；声学绝对极性未标定，C-P1-12）。
    pub polarity_basis: &'static str,
    /// 逐通道报告（plan.participants 顺序）。
    pub channels: Vec<PhaseChannelReport>,
    /// 检查位。
    pub checks: PhaseFitChecks,
    /// 警告（非阻塞）。
    pub warnings: Vec<String>,
}

/// 单通道执行 op（T66 编译最小输入：角色→整数 delay/极性）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChannelOp {
    /// 角色/通道名。
    pub channel: String,
    /// 设备通道号（可选）。
    pub index: Option<u32>,
    /// 整数延迟补偿（samples）。
    pub delay_samples: u32,
    /// delay_ms。
    pub delay_ms: f64,
    /// 校准极性修正。
    pub polarity: Polarity,
}

/// 单通道 T40 FIR 产物（系数 + 设计元数据；供 T66 直接使用/复核）。
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPhaseFir {
    /// 角色/通道名。
    pub channel: String,
    /// 设备通道号（可选）。
    pub index: Option<u32>,
    /// T40 设计（含系数/延迟/极性/验证声明）。
    pub design: PhaseDesign,
}

// ---------------------------------------------------------------------------
// 内部估计器：幅度加权匹配滤波（整样本）+ 相位斜率 LS（亚采样）+ 极性相干和
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct DelayEstimate {
    delay_samples: f64,
    prominence: f64,
    phase_rms_deg: f64,
    points_used: usize,
    boundary_hit: bool,
    refine_ok: bool,
}

#[derive(Clone, Debug)]
struct PolarityEstimate {
    sign: Option<i32>,
    margin: f64,
    points_used: usize,
}

/// 频带内选点：先按频带，再按相对最大幅度的动态范围掩码（噪声地板点不参与）。
fn select_band_points<'a>(
    points: &'a [PhaseFitPoint],
    band_hz: [f64; 2],
    min_points: usize,
    mask_db: f64,
) -> Result<Vec<&'a PhaseFitPoint>, PhaseFitError> {
    let in_band: Vec<&'a PhaseFitPoint> = points
        .iter()
        .filter(|point| point.freq_hz >= band_hz[0] && point.freq_hz <= band_hz[1])
        .collect();
    if in_band.len() < min_points {
        return phase_err(
            "E_PHF_BAND",
            format!(
                "band [{},{}] Hz has {} points < min_points {min_points}",
                band_hz[0],
                band_hz[1],
                in_band.len()
            ),
        );
    }
    let max_magnitude = in_band
        .iter()
        .map(|point| point.magnitude())
        .fold(0.0_f64, f64::max);
    if !(max_magnitude > 0.0) || !max_magnitude.is_finite() {
        return phase_err(
            "E_PHF_NUMERIC",
            format!(
                "band [{},{}] Hz has zero/non-finite magnitude",
                band_hz[0], band_hz[1]
            ),
        );
    }
    let floor = max_magnitude * 10.0_f64.powf(-mask_db / 20.0);
    let kept: Vec<&'a PhaseFitPoint> = in_band
        .into_iter()
        .filter(|point| point.magnitude() >= floor)
        .collect();
    if kept.len() < min_points {
        return phase_err(
            "E_PHF_BAND",
            format!(
                "band [{},{}] Hz keeps {} points after {mask_db} dB magnitude mask < min_points {min_points}",
                band_hz[0], band_hz[1], kept.len()
            ),
        );
    }
    Ok(kept)
}

fn wrap_radians(radians: f64) -> f64 {
    let mut value = radians.rem_euclid(2.0 * std::f64::consts::PI);
    if value > std::f64::consts::PI {
        value -= 2.0 * std::f64::consts::PI;
    }
    value
}

/// 匹配滤波幅度（0..1；单位延迟处所有点同相 -> 1）。
fn matched_amplitude(points: &[&PhaseFitPoint], fs: f64, delay_samples: f64) -> f64 {
    let mut acc_re = 0.0_f64;
    let mut acc_im = 0.0_f64;
    let mut norm = 0.0_f64;
    for point in points {
        let weight = point.re * point.re + point.im * point.im;
        let theta = 2.0 * std::f64::consts::PI * point.freq_hz * delay_samples / fs;
        let (sin, cos) = theta.sin_cos();
        // X * e^{-j*theta}
        acc_re += weight * (point.re * cos + point.im * sin);
        acc_im += weight * (point.im * cos - point.re * sin);
        norm += weight;
    }
    if norm > 0.0 {
        acc_re.hypot(acc_im) / norm
    } else {
        0.0
    }
}

/// 去整数延迟后的加权相位斜率 LS + 亚采样精修；返回 (delay, 线性 RMS deg)。
fn refine_delay(points: &[&PhaseFitPoint], fs: f64, integer_samples: i64) -> (f64, f64, bool) {
    let fs_f = fs;
    let mut unwrapped: Vec<(f64, f64, f64)> = Vec::with_capacity(points.len());
    let mut previous: Option<f64> = None;
    for point in points {
        let theta = 2.0 * std::f64::consts::PI * point.freq_hz * integer_samples as f64 / fs_f;
        let mut phi = wrap_radians(point.im.atan2(point.re) - theta);
        if let Some(previous_phi) = previous {
            while phi - previous_phi > std::f64::consts::PI {
                phi -= 2.0 * std::f64::consts::PI;
            }
            while previous_phi - phi > std::f64::consts::PI {
                phi += 2.0 * std::f64::consts::PI;
            }
        }
        previous = Some(phi);
        let weight = point.re * point.re + point.im * point.im;
        unwrapped.push((point.freq_hz, phi, weight));
    }
    let mut sum_w = 0.0_f64;
    let mut sum_wf = 0.0_f64;
    let mut sum_wff = 0.0_f64;
    let mut sum_wphi = 0.0_f64;
    let mut sum_wfphi = 0.0_f64;
    for (freq, phi, weight) in &unwrapped {
        sum_w += *weight;
        sum_wf += *weight * *freq;
        sum_wff += *weight * *freq * *freq;
        sum_wphi += *weight * *phi;
        sum_wfphi += *weight * *freq * *phi;
    }
    let determinant = sum_w * sum_wff - sum_wf * sum_wf;
    if !(determinant.abs() > 1e-18) || !(sum_w > 0.0) {
        return (integer_samples as f64, 180.0, false);
    }
    let slope = (sum_w * sum_wfphi - sum_wf * sum_wphi) / determinant;
    let intercept = (sum_wphi - slope * sum_wf) / sum_w;
    let mut residual_energy = 0.0_f64;
    for (freq, phi, weight) in &unwrapped {
        let fitted = intercept + slope * *freq;
        residual_energy += *weight * (*phi - fitted) * (*phi - fitted);
    }
    let rms_radians = (residual_energy / sum_w).sqrt();
    let rms_deg = rms_radians.to_degrees();
    let epsilon = slope * fs_f / (2.0 * std::f64::consts::PI);
    let refine_ok = epsilon.is_finite() && epsilon.abs() <= SUB_SAMPLE_REFINE_LIMIT;
    let delay = integer_samples as f64 + if refine_ok { epsilon } else { 0.0 };
    (
        delay,
        if rms_deg.is_finite() { rms_deg } else { 180.0 },
        refine_ok,
    )
}

/// 单测点单通道延迟估计：粗搜（±search，步长 8）+ 细搜（±8，步长 1）+ 相位斜率精修。
fn estimate_delay(
    points: &[PhaseFitPoint],
    fs: f64,
    band_hz: [f64; 2],
    options: &PhaseFitOptions,
) -> Result<DelayEstimate, PhaseFitError> {
    let selected = select_band_points(
        points,
        band_hz,
        options.min_points,
        options.magnitude_mask_db,
    )?;
    let range =
        ((options.search_range_ms * fs / 1000.0).round() as i64).max(COARSE_SEARCH_STEP_SAMPLES);
    let step = COARSE_SEARCH_STEP_SAMPLES;
    let mut best_score = f64::NEG_INFINITY;
    let mut best_delay = 0_i64;
    let mut score_sum = 0.0_f64;
    let mut candidate_count = 0_usize;
    let mut candidate = -range;
    while candidate <= range {
        let score = matched_amplitude(&selected, fs, candidate as f64);
        score_sum += score;
        candidate_count += 1;
        if score > best_score {
            best_score = score;
            best_delay = candidate;
        }
        candidate += step;
    }
    if candidate_count == 0 || !best_score.is_finite() {
        return phase_err("E_PHF_NUMERIC", "empty matched-filter candidate set");
    }
    let coarse_at_edge = best_delay == -range || best_delay == range;
    let fine_lo = (best_delay - step).max(-range);
    let fine_hi = (best_delay + step).min(range);
    let mut fine_delay = best_delay;
    let mut fine_score = best_score;
    let mut fine_candidate = fine_lo;
    while fine_candidate <= fine_hi {
        let score = matched_amplitude(&selected, fs, fine_candidate as f64);
        if score > fine_score {
            fine_score = score;
            fine_delay = fine_candidate;
        }
        fine_candidate += 1;
    }
    let mean_score = score_sum / candidate_count as f64;
    let prominence = if mean_score > 0.0 {
        fine_score / mean_score
    } else {
        1.0
    };
    let (delay_samples, phase_rms_deg, refine_ok) = refine_delay(&selected, fs, fine_delay);
    let boundary_hit = coarse_at_edge || delay_samples.abs() + 1.0 >= range as f64;
    if !delay_samples.is_finite() {
        return phase_err("E_PHF_DELAY", "estimated delay is not finite");
    }
    Ok(DelayEstimate {
        delay_samples,
        prominence,
        phase_rms_deg,
        points_used: selected.len(),
        boundary_hit,
        refine_ok,
    })
}

fn delay_confidence(estimate: &DelayEstimate, options: &PhaseFitOptions) -> f64 {
    if estimate.boundary_hit || !estimate.refine_ok || !estimate.phase_rms_deg.is_finite() {
        return 0.0;
    }
    let c_rms = (1.0 - estimate.phase_rms_deg / options.delay_phase_rms_max_deg).clamp(0.0, 1.0);
    let c_prominence = ((estimate.prominence - 1.0) / 3.0).clamp(0.0, 1.0);
    c_rms.min(c_prominence)
}

/// 极性估计：去估计延迟后的加权相干和 s∈[-1,1]；margin=|s|，低于门 -> 未知（不猜）。
fn estimate_polarity(
    points: &[PhaseFitPoint],
    fs: f64,
    band_hz: [f64; 2],
    delay_samples: f64,
    options: &PhaseFitOptions,
) -> Result<PolarityEstimate, PhaseFitError> {
    let selected = select_band_points(
        points,
        band_hz,
        options.min_points,
        options.magnitude_mask_db,
    )?;
    let mut numerator = 0.0_f64;
    let mut denominator = 0.0_f64;
    for point in &selected {
        let magnitude = point.magnitude();
        if !(magnitude > 0.0) {
            continue;
        }
        let weight = magnitude * magnitude;
        let theta = 2.0 * std::f64::consts::PI * point.freq_hz * delay_samples / fs;
        let (sin, cos) = theta.sin_cos();
        let rotated_real = point.re * cos + point.im * sin;
        numerator += weight * (rotated_real / magnitude);
        denominator += weight;
    }
    if !(denominator > 0.0) {
        return phase_err("E_PHF_NUMERIC", "polarity estimate has zero energy");
    }
    let sign_value = numerator / denominator;
    let margin = sign_value.abs().clamp(0.0, 1.0);
    let sign = if margin >= options.polarity_margin_min {
        Some(if sign_value > 0.0 { 1 } else { -1 })
    } else {
        None
    };
    Ok(PolarityEstimate {
        sign,
        margin,
        points_used: selected.len(),
    })
}

// ---------------------------------------------------------------------------
// 响应收集 / 覆盖 / 相位基准 / 测量 hash
// ---------------------------------------------------------------------------

use tspi_modal_design::ingest::PositionRole;

#[derive(Clone, Copy, Debug)]
struct ResponseSlot<'p, 'r> {
    position: &'p str,
    channel: &'p str,
    points: &'r [PhaseFitPoint],
}

fn collect_response_slots<'p, 'r>(
    plan: &'p MultipointPlan,
    responses: &'r [DePhasedResponse],
    plan_basis: PhaseBasis,
    common_d0: f64,
) -> Result<Vec<ResponseSlot<'p, 'r>>, PhaseFitError> {
    if plan.participants.len() > MAX_CHANNELS {
        return phase_err(
            "E_PHF_PLAN",
            format!(
                "plan has {} participants > MAX_CHANNELS {MAX_CHANNELS}",
                plan.participants.len()
            ),
        );
    }
    // 1) 每个响应：已知测点（非 holdout）、已知角色、basis 与 D0 一致（D0 只扣一次）。
    for response in responses {
        let position = match plan.position(response.position()) {
            Some(position) => position,
            None => {
                return phase_err(
                    "E_PHF_RESPONSE_UNKNOWN",
                    format!(
                        "response references unknown position {:?}",
                        response.position()
                    ),
                )
            }
        };
        if position.role == PositionRole::Validation {
            return phase_err(
                "E_PHF_HOLDOUT_CONTAMINATION",
                format!(
                    "response for validation/holdout position {:?} must not enter the phase fit",
                    response.position()
                ),
            );
        }
        if !plan
            .participants
            .iter()
            .any(|participant| participant == response.channel())
        {
            return phase_err(
                "E_PHF_RESPONSE_UNKNOWN",
                format!(
                    "response references unknown participant {:?}",
                    response.channel()
                ),
            );
        }
        match (plan_basis, response.basis()) {
            (PhaseBasis::AlignedSegment, PhaseBasis::AlignedSegment) => {
                if (response.delay_removed_samples() - common_d0).abs() > 1e-9 {
                    return phase_err(
                        "E_PHF_COMMON_D0_MISMATCH",
                        format!(
                            "response {}/{} removed D0={} but plan.common_d0={common_d0}（同一 D0 只扣一次）",
                            response.position(),
                            response.channel(),
                            response.delay_removed_samples()
                        ),
                    );
                }
            }
            (PhaseBasis::Absolute, PhaseBasis::Absolute) => {
                if response.delay_removed_samples() != 0.0 {
                    return phase_err(
                        "E_PHF_DOUBLE_DEPHASE",
                        format!(
                            "absolute response {}/{} must not have a removed delay (got {})",
                            response.position(),
                            response.channel(),
                            response.delay_removed_samples()
                        ),
                    );
                }
            }
            _ => {
                return phase_err(
                    "E_PHF_PHASE_BASIS",
                    format!(
                        "plan.phase_ref basis {:?} does not match response {}/{} basis {:?}; refusing to guess whether D0 was already removed",
                        plan_basis,
                        response.position(),
                        response.channel(),
                        response.basis()
                    ),
                );
            }
        }
    }
    // 2) 覆盖：design 测点 × participants 恰好各一个（缺失/重复都拒绝）。
    let design_labels: Vec<&str> = plan
        .design_positions()
        .map(|position| position.label.as_str())
        .collect();
    let mut slots = Vec::with_capacity(design_labels.len() * plan.participants.len());
    for label in &design_labels {
        for participant in &plan.participants {
            let mut matched: Option<&'r DePhasedResponse> = None;
            for response in responses {
                if response.position() == *label && response.channel() == participant.as_str() {
                    if matched.is_some() {
                        return phase_err(
                            "E_PHF_RESPONSE_DUPLICATE",
                            format!("duplicate response for position {label:?} / channel {participant:?}"),
                        );
                    }
                    matched = Some(response);
                }
            }
            match matched {
                Some(response) => slots.push(ResponseSlot {
                    position: label,
                    channel: participant.as_str(),
                    points: response.points(),
                }),
                None => {
                    return phase_err(
                        "E_PHF_RESPONSE_MISSING",
                        format!(
                        "missing response for design position {label:?} / channel {participant:?}"
                    ),
                    )
                }
            }
        }
    }
    Ok(slots)
}

fn measurement_sha256(plan: &MultipointPlan, slots: &[ResponseSlot<'_, '_>]) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "tspi-phase-fit-measurement/v1");
    let _ = writeln!(text, "fs={}", plan.sample_rate_hz);
    let _ = writeln!(text, "plan_sha256={}", plan.plan_sha256());
    let _ = writeln!(text, "common_d0_samples={}", plan.common_d0.samples());
    let _ = writeln!(text, "phase_ref={}", plan.phase_ref);
    for slot in slots {
        let _ = writeln!(text, "slot|{}|{}", slot.position, slot.channel);
        for point in slot.points {
            let _ = writeln!(
                text,
                "{:.17e},{:.17e},{:.17e}",
                point.freq_hz, point.re, point.im
            );
        }
    }
    sha256_hex(text.as_bytes())
}

fn coefficient_sha256(design: &PhaseDesign) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "tspi-phase-fir-coeffs/v1");
    let _ = writeln!(
        text,
        "fs={:.17e} delay={:.17e} polarity={} taps={}",
        design.fs,
        design.delay_samples,
        design.polarity.as_str(),
        design.taps
    );
    for coefficient in &design.coefficients {
        let _ = writeln!(text, "{coefficient:.17e}");
    }
    sha256_hex(text.as_bytes())
}

/// 把 T40 FIR 作用到**测量复响应域**：X_out = X * conj(H_fir)。
///
/// T40 FIR 是标准 DTFT 的 H(ω)=Σh[n]e^{-jωn}；本栈测量复响应是 T49 口径（延迟贡献 +jωτ），
/// 即其**共轭域**，所以必须乘 conj(H)。方向写反（乘 H）会把相对错位放大而不是抵消，
/// 见 `t65_phase_fit_fir_execution_direction_is_correct` 负向用例。
pub fn apply_measured_domain_fir(
    points: &[PhaseFitPoint],
    design: &PhaseDesign,
) -> Vec<PhaseFitPoint> {
    points
        .iter()
        .map(|point| {
            let response = design.response_at(point.freq_hz);
            let (mul_re, mul_im) = (response.re, -response.im);
            PhaseFitPoint {
                freq_hz: point.freq_hz,
                re: point.re * mul_re - point.im * mul_im,
                im: point.re * mul_im + point.im * mul_re,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 拟合入口
// ---------------------------------------------------------------------------

/// 一次 delay/polarity 拟合的完整产出（report + 执行 ops + T40 FIR + artifact）。
#[derive(Clone, Debug)]
pub struct PhaseFitOutput {
    /// 确定性报告（tspi-phase-fit/v1）。
    pub report: PhaseFitReport,
    /// 执行 ops（角色→整数 delay/极性；按 plan.participants 顺序）。
    pub ops: Vec<PhaseChannelOp>,
    /// T40 FIR（系数 + 设计元数据；按 plan.participants 顺序）。
    pub firs: Vec<ChannelPhaseFir>,
    /// 待发布 artifact（report + ops）。
    pub artifacts: PhaseFitArtifacts,
}

impl PhaseFitOutput {
    /// report JSON（确定性；同输入同字节）。
    pub fn report_json(&self) -> String {
        self.report.to_json()
    }

    /// ops JSON（确定性；同输入同字节）。
    pub fn ops_json(&self) -> String {
        phase_ops_json(&self.report, &self.ops)
    }
}

fn validate_channel_bands(
    plan: &MultipointPlan,
    channel_bands: &[PhaseChannelBand],
    fs: u32,
) -> Result<(), PhaseFitError> {
    let mut names: Vec<String> = Vec::new();
    for entry in channel_bands {
        if !plan
            .participants
            .iter()
            .any(|participant| participant == &entry.channel)
        {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "channel_bands references unknown participant {:?}",
                    entry.channel
                ),
            );
        }
        if names.iter().any(|name| name == &entry.channel) {
            return phase_err(
                "E_PHF_CONFIG",
                format!("duplicate channel_bands entry for {:?}", entry.channel),
            );
        }
        names.push(entry.channel.clone());
        entry.validate(fs)?;
    }
    Ok(())
}

fn validate_channel_indices(
    plan: &MultipointPlan,
    channel_indices: &[PhaseChannelIndex],
) -> Result<(), PhaseFitError> {
    let mut channels: Vec<String> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for entry in channel_indices {
        if !plan
            .participants
            .iter()
            .any(|participant| participant == &entry.channel)
        {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "channel_indices references unknown participant {:?}",
                    entry.channel
                ),
            );
        }
        if entry.index > MAX_DEVICE_CHANNEL_INDEX {
            return phase_err(
                "E_PHF_CONFIG",
                format!(
                    "channel {:?} index {} > MAX_DEVICE_CHANNEL_INDEX {MAX_DEVICE_CHANNEL_INDEX}",
                    entry.channel, entry.index
                ),
            );
        }
        if channels.iter().any(|channel| channel == &entry.channel) {
            return phase_err(
                "E_PHF_CONFIG",
                format!("duplicate channel_indices entry for {:?}", entry.channel),
            );
        }
        if indices.contains(&entry.index) {
            return phase_err(
                "E_PHF_CONFIG",
                format!("duplicate device channel index {}", entry.index),
            );
        }
        channels.push(entry.channel.clone());
        indices.push(entry.index);
    }
    Ok(())
}

fn find_channel_index(channel_indices: &[PhaseChannelIndex], channel: &str) -> Option<u32> {
    channel_indices
        .iter()
        .find(|entry| entry.channel == channel)
        .map(|entry| entry.index)
}

fn evaluate_quality_gate(
    request: &PhaseFitRequest<'_>,
) -> Result<PhaseFitQualitySummary, PhaseFitError> {
    let plan_sha256 = request.plan.plan_sha256();
    match request.design_quality {
        Some(quality) => {
            if quality.scope != QualityScope::Design {
                return phase_err(
                    "E_PHF_QUALITY_BLOCKED",
                    format!(
                        "quality report scope={} but phase fit requires design scope",
                        quality.scope.as_str()
                    ),
                );
            }
            if quality.plan_sha256 != plan_sha256 {
                return phase_err(
                    "E_PHF_QUALITY_STALE",
                    format!(
                        "quality report plan_sha256={} != current plan {plan_sha256}",
                        quality.plan_sha256
                    ),
                );
            }
            if request.options.require_design_quality && !quality.continue_allowed {
                return phase_err(
                    "E_PHF_QUALITY_BLOCKED",
                    format!(
                        "T63 design quality continue_allowed=false (points_usable={}/{})",
                        quality.points_usable, quality.points_total
                    ),
                );
            }
            Ok(PhaseFitQualitySummary {
                required: request.options.require_design_quality,
                present: true,
                scope: quality.scope.as_str().to_string(),
                plan_sha256: quality.plan_sha256.clone(),
                continue_allowed: quality.continue_allowed,
                points_usable: quality.points_usable,
                points_total: quality.points_total,
            })
        }
        None => {
            if request.options.require_design_quality {
                return phase_err(
                    "E_PHF_QUALITY_MISSING",
                    "require_design_quality=true but no T63 design scope quality report (SNR/clock/phase gate missing)",
                );
            }
            Ok(PhaseFitQualitySummary {
                required: false,
                present: false,
                scope: QualityScope::Design.as_str().to_string(),
                plan_sha256,
                continue_allowed: false,
                points_usable: 0,
                points_total: 0,
            })
        }
    }
}

fn producer_polarity_agreement(
    quality: Option<&PositionQualityReport>,
    reference_position: &str,
    channel: &str,
    measured_sign: i32,
) -> Option<bool> {
    let quality = quality?;
    let point = quality
        .points
        .iter()
        .find(|point| point.label == reference_position)?;
    let measurement = point
        .measurements
        .iter()
        .find(|measurement| measurement.speaker == channel)?;
    let producer_sign = measurement.polarity?;
    Some(producer_sign == measured_sign)
}

/// 执行 delay/polarity 拟合（纯 [NONRT]；不采音/不写盘/不打开设备）。
///
/// 未知/歧义/超限 -> Err（稳定错误码）；成功 -> 确定性 report + ops + T40 FIR + artifact。
pub fn fit_phase(request: &PhaseFitRequest<'_>) -> Result<PhaseFitOutput, PhaseFitError> {
    request.plan.validate().map_err(plan_error)?;
    let fs = request.plan.sample_rate_hz;
    validate_sample_rate(fs)?;
    let mut warnings = request.options.validate(fs)?;
    warnings.push(
        "极性按 UMIK-1 正压->正样本约定解释；声学绝对极性未标定（ACCEPTANCE §3/C-P1-12），只给约定下的 correction"
            .to_string(),
    );
    validate_channel_bands(request.plan, request.channel_bands, fs)?;
    validate_channel_indices(request.plan, request.channel_indices)?;
    let plan_basis = request.plan.phase_basis().map_err(plan_error)?;
    let common_d0 = request.plan.common_d0.as_f64();
    if plan_basis == PhaseBasis::DelayRemoved {
        return phase_err(
            "E_PHF_PHASE_BASIS",
            "plan.phase_ref=delay_removed has no machine-readable removed delay; declare aligned_segment + CommonD0 (T49 v2)",
        );
    }
    let slots = collect_response_slots(request.plan, request.responses, plan_basis, common_d0)?;
    let quality = evaluate_quality_gate(request)?;
    if quality.present && !quality.continue_allowed && !request.options.require_design_quality {
        warnings.push(
            "T63 design quality continue_allowed=false 但 require_design_quality=false（开发模式继续，结果不可当 T6 证据）"
                .to_string(),
        );
    }
    // 参考测点：单 design 测点可默认；多测点必须显式声明（不猜）。
    let design_labels: Vec<String> = request
        .plan
        .design_positions()
        .map(|position| position.label.clone())
        .collect();
    let reference_position = match &request.options.reference_position {
        Some(label) => {
            if !design_labels.contains(label) {
                return phase_err(
                    "E_PHF_REFERENCE_POSITION",
                    format!(
                        "reference_position {label:?} is not a design position {design_labels:?}"
                    ),
                );
            }
            label.clone()
        }
        None => {
            if design_labels.len() == 1 {
                design_labels[0].clone()
            } else {
                return phase_err(
                    "E_PHF_REFERENCE_POSITION",
                    format!(
                        "{} design positions declared: reference_position must be explicit (estimation never guesses the seat)",
                        design_labels.len()
                    ),
                );
            }
        }
    };
    let reference_position_index = design_labels
        .iter()
        .position(|label| label == &reference_position)
        .expect("reference position validated above");
    let measurement_sha256 = measurement_sha256(request.plan, &slots);
    let fs_f = fs as f64;
    let channels: Vec<String> = request.plan.participants.clone();
    // 逐 design 测点逐通道估计（参考测点驱动应用；其余只作一致性/诊断）。
    let mut delay_estimates: Vec<Vec<DelayEstimate>> = Vec::with_capacity(design_labels.len());
    let mut delay_confidences: Vec<Vec<f64>> = Vec::with_capacity(design_labels.len());
    let mut polarity_estimates: Vec<Vec<PolarityEstimate>> =
        Vec::with_capacity(design_labels.len());
    for position_index in 0..design_labels.len() {
        let mut delay_row = Vec::with_capacity(channels.len());
        let mut confidence_row = Vec::with_capacity(channels.len());
        let mut polarity_row = Vec::with_capacity(channels.len());
        for (channel_index, channel) in channels.iter().enumerate() {
            let slot = &slots[position_index * channels.len() + channel_index];
            let delay_band =
                resolve_channel_band(&request.options, request.channel_bands, channel, false);
            let estimate = estimate_delay(slot.points, fs_f, delay_band, &request.options)?;
            if estimate.boundary_hit {
                return phase_err(
                    "E_PHF_DELAY_SEARCH",
                    format!(
                        "position {:?} channel {channel:?}: matched-filter peak hit search boundary ±{} ms; refusing to guess (raise search_range_ms)",
                        design_labels[position_index], request.options.search_range_ms
                    ),
                );
            }
            let confidence = delay_confidence(&estimate, &request.options);
            let polarity_band =
                resolve_channel_band(&request.options, request.channel_bands, channel, true);
            let polarity = estimate_polarity(
                slot.points,
                fs_f,
                polarity_band,
                estimate.delay_samples,
                &request.options,
            )?;
            delay_row.push(estimate);
            confidence_row.push(confidence);
            polarity_row.push(polarity);
        }
        delay_estimates.push(delay_row);
        delay_confidences.push(confidence_row);
        polarity_estimates.push(polarity_row);
    }
    // 参考通道 = 参考测点上到达最晚（tau 最大）；D=0。
    let reference_delays = &delay_estimates[reference_position_index];
    let max_tau = reference_delays
        .iter()
        .map(|estimate| estimate.delay_samples)
        .fold(f64::NEG_INFINITY, f64::max);
    let reference_channel_index = reference_delays
        .iter()
        .position(|estimate| estimate.delay_samples == max_tau)
        .expect("max tau must come from an estimate");
    let tie_margin: Option<f64> = if channels.len() > 1 {
        let mut sorted: Vec<f64> = reference_delays
            .iter()
            .map(|estimate| estimate.delay_samples)
            .collect();
        sorted.sort_by(|left, right| right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal));
        Some(sorted[0] - sorted[1])
    } else {
        None
    };
    let reference_ambiguous = tie_margin
        .map(|margin| margin < REFERENCE_AMBIGUITY_SAMPLES)
        .unwrap_or(false);
    if reference_ambiguous {
        warnings.push(format!(
            "reference channel {:?} tie margin {:.6} sample < {:.3}: reference_ambiguous=true（D=0 归属按 plan 顺序取第一个最大）",
            channels[reference_channel_index],
            tie_margin.unwrap_or(0.0),
            REFERENCE_AMBIGUITY_SAMPLES
        ));
    }
    if channels.len() < 2 {
        warnings.push("单通道 plan：D 恒为 0，无相对 delay/polarity 交叉验证".to_string());
    }
    if request.channel_indices.is_empty() {
        warnings.push(
            "未提供 channel_indices：report.reference_index=null；T66/T57 必须按 overlay 实际解析把角色映射到 DAC index（本模块不猜）"
                .to_string(),
        );
    }
    // 整数样本契约：D_i = round(max tau − tau_i)，min=0，上限 floor(0.2·fs)，超限报错不截断。
    let max_delay_samples = (request.options.max_delay_ms * fs_f / 1000.0).floor() as u32;
    let mut raw_delays = Vec::with_capacity(channels.len());
    let mut delay_samples = Vec::with_capacity(channels.len());
    for (channel_index, channel) in channels.iter().enumerate() {
        let raw = (max_tau - reference_delays[channel_index].delay_samples).max(0.0);
        if raw > max_delay_samples as f64 + 1e-9 {
            return phase_err(
                "E_PHF_DELAY_LIMIT",
                format!(
                    "channel {channel:?} needs D={raw:.3} samples > cap {max_delay_samples} ({} ms @ fs={fs}); CONTRACT rejects instead of truncating",
                    request.options.max_delay_ms
                ),
            );
        }
        let rounded = raw.round();
        if rounded > max_delay_samples as f64 {
            return phase_err(
                "E_PHF_DELAY_LIMIT",
                format!(
                    "channel {channel:?} rounded D={rounded} exceeds cap {max_delay_samples} samples"
                ),
            );
        }
        let confidence = delay_confidences[reference_position_index][channel_index];
        if confidence < request.options.delay_confidence_min {
            return phase_err(
                "E_PHF_DELAY_LOW_CONFIDENCE",
                format!(
                    "channel {channel:?} delay confidence {confidence:.3} < {:.3} (phase_rms={:.3} deg, prominence={:.3})",
                    request.options.delay_confidence_min,
                    reference_delays[channel_index].phase_rms_deg,
                    reference_delays[channel_index].prominence
                ),
            );
        }
        raw_delays.push(raw);
        delay_samples.push(rounded as u32);
    }
    // 极性（全 design 测点一致 + margin 门；未知/矛盾 -> 拒绝）。
    let mut corrections = Vec::with_capacity(channels.len());
    let mut measured_signs = Vec::with_capacity(channels.len());
    let mut polarity_confidences = Vec::with_capacity(channels.len());
    let mut producer_agreements = Vec::with_capacity(channels.len());
    for (channel_index, channel) in channels.iter().enumerate() {
        let mut signs: Vec<i32> = Vec::with_capacity(design_labels.len());
        let mut margin_min = f64::INFINITY;
        for position_index in 0..design_labels.len() {
            let polarity = &polarity_estimates[position_index][channel_index];
            margin_min = margin_min.min(polarity.margin);
            match polarity.sign {
                Some(sign) => signs.push(sign),
                None => {
                    return phase_err(
                        "E_PHF_POLARITY_UNKNOWN",
                        format!(
                            "position {:?} channel {channel:?}: polarity margin {:.3} < {:.3}; refusing to guess",
                            design_labels[position_index],
                            polarity.margin,
                            request.options.polarity_margin_min
                        ),
                    );
                }
            }
        }
        if signs.windows(2).any(|window| window[0] != window[1]) {
            return phase_err(
                "E_PHF_POLARITY_CONFLICT",
                format!("channel {channel:?}: measured polarity differs across design positions {signs:?}"),
            );
        }
        let measured_sign = signs[0];
        let polarity_confidence = margin_min.clamp(0.0, 1.0);
        if polarity_confidence < request.options.polarity_margin_min {
            return phase_err(
                "E_PHF_POLARITY_LOW_CONFIDENCE",
                format!(
                    "channel {channel:?} polarity confidence {polarity_confidence:.3} < {:.3}",
                    request.options.polarity_margin_min
                ),
            );
        }
        let agreement = producer_polarity_agreement(
            request.design_quality,
            &reference_position,
            channel,
            measured_sign,
        );
        if agreement == Some(false) {
            return phase_err(
                "E_PHF_POLARITY_CONFLICT",
                format!(
                    "channel {channel:?}: measured polarity {measured_sign:+} conflicts with T63 production polarity at {reference_position:?}"
                ),
            );
        }
        corrections.push(if measured_sign < 0 {
            Polarity::Invert
        } else {
            Polarity::Normal
        });
        measured_signs.push(measured_sign);
        polarity_confidences.push(polarity_confidence);
        producer_agreements.push(agreement);
    }
    // T40 FIR：每通道 h[n]=sign·delta[n−D]（最小因果支撑），并做解析验证。
    let mut designs: Vec<PhaseDesign> = Vec::with_capacity(channels.len());
    let mut fir_reports: Vec<PhaseFirReport> = Vec::with_capacity(channels.len());
    for channel_index in 0..channels.len() {
        let delay = delay_samples[channel_index];
        let spec =
            IntegerDelaySpec::new(fs_f, delay as u64).with_polarity(corrections[channel_index]);
        let design = design_integer(&spec)
            .map_err(|error| PhaseFitError::new("E_PHF_FIR", error.to_string()))?;
        let verification = design.verify();
        if !verification.passed() {
            return phase_err(
                "E_PHF_FIR",
                format!(
                    "channel {:?}: T40 integer-delay FIR verification failed (D={delay})",
                    channels[channel_index]
                ),
            );
        }
        let coefficients_sha256 = coefficient_sha256(&design);
        fir_reports.push(PhaseFirReport {
            delay_samples: delay,
            polarity: corrections[channel_index],
            taps: design.taps,
            onset_samples: design.onset_samples(),
            delay_ms: design.delay_ms,
            coefficients_sha256,
            verification_passed: verification.passed(),
            spec_band_required: design.spec_band_required,
            spec_band_compliant: verification.checks.spec_band_compliant,
        });
        designs.push(design);
    }
    // FIR 执行方向校验（参考测点）：conj(H) 作用后残余到达差 <= tolerance 且补偿后极性=normal。
    let reference_slot =
        &slots[reference_position_index * channels.len() + reference_channel_index];
    let reference_channel = channels[reference_channel_index].clone();
    let reference_compensated =
        apply_measured_domain_fir(reference_slot.points, &designs[reference_channel_index]);
    let reference_delay_band = resolve_channel_band(
        &request.options,
        request.channel_bands,
        &reference_channel,
        false,
    );
    let reference_residual_estimate = estimate_delay(
        &reference_compensated,
        fs_f,
        reference_delay_band,
        &request.options,
    )?;
    let reference_residual_delay = reference_residual_estimate.delay_samples;
    let mut executions: Vec<PhaseExecutionReport> = Vec::with_capacity(channels.len());
    for channel_index in 0..channels.len() {
        let channel = &channels[channel_index];
        let slot = &slots[reference_position_index * channels.len() + channel_index];
        let compensated = apply_measured_domain_fir(slot.points, &designs[channel_index]);
        let delay_band =
            resolve_channel_band(&request.options, request.channel_bands, channel, false);
        let estimate = estimate_delay(&compensated, fs_f, delay_band, &request.options)?;
        if estimate.boundary_hit {
            return phase_err(
                "E_PHF_EXECUTION",
                format!("channel {channel:?}: compensated residual delay hit search boundary"),
            );
        }
        let polarity_band =
            resolve_channel_band(&request.options, request.channel_bands, channel, true);
        let polarity = estimate_polarity(
            &compensated,
            fs_f,
            polarity_band,
            estimate.delay_samples,
            &request.options,
        )?;
        let delta = estimate.delay_samples - reference_residual_delay;
        let within_tolerance = delta.abs() <= request.options.execution_tolerance_samples + 1e-9;
        let polarity_ok = polarity.sign == Some(1)
            && polarity.margin + 1e-9 >= request.options.polarity_margin_min;
        let direction_ok = within_tolerance && polarity_ok;
        let execution = PhaseExecutionReport {
            residual_delay_samples: estimate.delay_samples,
            reference_residual_delay_samples: reference_residual_delay,
            delta_samples: delta,
            within_tolerance,
            compensated_polarity_sign: polarity.sign,
            compensated_polarity_margin: polarity.margin,
            direction_ok,
        };
        if !direction_ok {
            return phase_err(
                "E_PHF_EXECUTION",
                format!(
                    "channel {channel:?}: FIR direction check failed (residual={:.6}, reference={:.6}, delta={:.6} sample, tolerance={}, compensated polarity={:?} margin={:.3}); refusing to publish half-aligned ops",
                    execution.residual_delay_samples,
                    execution.reference_residual_delay_samples,
                    execution.delta_samples,
                    request.options.execution_tolerance_samples,
                    execution.compensated_polarity_sign,
                    execution.compensated_polarity_margin
                ),
            );
        }
        executions.push(execution);
    }
    // 组装报告 / ops / FIR。
    let reference_index = find_channel_index(request.channel_indices, &reference_channel);
    let mut channel_reports: Vec<PhaseChannelReport> = Vec::with_capacity(channels.len());
    let mut ops: Vec<PhaseChannelOp> = Vec::with_capacity(channels.len());
    let mut firs: Vec<ChannelPhaseFir> = Vec::with_capacity(channels.len());
    for channel_index in 0..channels.len() {
        let channel = channels[channel_index].clone();
        let index = find_channel_index(request.channel_indices, &channel);
        let raw = raw_delays[channel_index];
        let delay = delay_samples[channel_index];
        let mut positions = Vec::with_capacity(design_labels.len());
        for position_index in 0..design_labels.len() {
            let estimate = &delay_estimates[position_index][channel_index];
            let polarity = &polarity_estimates[position_index][channel_index];
            positions.push(PhasePositionReport {
                position: design_labels[position_index].clone(),
                delay_raw_samples: estimate.delay_samples,
                delay_confidence: delay_confidences[position_index][channel_index],
                delay_phase_rms_deg: estimate.phase_rms_deg,
                delay_prominence: estimate.prominence,
                delay_points_used: estimate.points_used,
                polarity: polarity.sign,
                polarity_margin: polarity.margin,
                polarity_points_used: polarity.points_used,
            });
        }
        let fir_report = fir_reports[channel_index].clone();
        let execution = executions[channel_index].clone();
        channel_reports.push(PhaseChannelReport {
            channel: channel.clone(),
            index,
            delay_raw_samples: raw,
            delay_samples: delay,
            delay_ms: delay as f64 / fs_f * 1000.0,
            fractional_residual_samples: raw - delay as f64,
            delay_confidence: delay_confidences[reference_position_index][channel_index],
            reference: channel_index == reference_channel_index,
            polarity: corrections[channel_index],
            measured_polarity_sign: measured_signs[channel_index],
            polarity_confidence: polarity_confidences[channel_index],
            polarity_margin_min: polarity_confidences[channel_index],
            producer_polarity_agrees: producer_agreements[channel_index],
            positions,
            fir: fir_report,
            execution,
        });
        ops.push(PhaseChannelOp {
            channel: channel.clone(),
            index,
            delay_samples: delay,
            delay_ms: delay as f64 / fs_f * 1000.0,
            polarity: corrections[channel_index],
        });
        firs.push(ChannelPhaseFir {
            channel,
            index,
            design: designs[channel_index].clone(),
        });
    }
    let integer_delay_contract = channel_reports
        .iter()
        .all(|report| (report.delay_raw_samples - report.delay_samples as f64).abs() <= 0.5 + 1e-9);
    let min_zero = channel_reports
        .iter()
        .map(|report| report.delay_samples)
        .min()
        .unwrap_or(1)
        == 0;
    let max_within_cap = channel_reports
        .iter()
        .all(|report| report.delay_samples <= max_delay_samples);
    let reference_is_latest = channel_reports
        .iter()
        .find(|report| report.reference)
        .map(|report| report.delay_samples == 0)
        .unwrap_or(false)
        && reference_delays[reference_channel_index].delay_samples == max_tau;
    let fractional_residual_within_half_sample = channel_reports
        .iter()
        .all(|report| report.fractional_residual_samples.abs() <= 0.5 + 1e-9);
    let fir_design_ok = fir_reports.iter().all(|report| report.verification_passed);
    let fir_direction_ok = executions.iter().all(|execution| execution.direction_ok);
    let checks = PhaseFitChecks {
        integer_delay_contract,
        min_zero,
        max_within_cap,
        reference_is_latest,
        fractional_residual_within_half_sample,
        polarity_known: true,
        fir_design_ok,
        fir_direction_ok,
        all_pass: integer_delay_contract
            && min_zero
            && max_within_cap
            && reference_is_latest
            && fractional_residual_within_half_sample
            && fir_design_ok
            && fir_direction_ok,
    };
    let options_report = PhaseFitOptionsReport {
        delay_band_hz: request.options.delay_band_hz,
        polarity_band_hz: request.options.polarity_band_hz,
        reference_position: reference_position.clone(),
        max_delay_ms: request.options.max_delay_ms,
        search_range_ms: request.options.search_range_ms,
        magnitude_mask_db: request.options.magnitude_mask_db,
        min_points: request.options.min_points,
        delay_confidence_min: request.options.delay_confidence_min,
        polarity_margin_min: request.options.polarity_margin_min,
        delay_phase_rms_max_deg: request.options.delay_phase_rms_max_deg,
        execution_tolerance_samples: request.options.execution_tolerance_samples,
        require_design_quality: request.options.require_design_quality,
    };
    let report = PhaseFitReport {
        schema: PHASE_FIT_SCHEMA,
        tool_name: PHASE_FIT_TOOL_NAME,
        tool_version: PHASE_FIT_TOOL_VERSION,
        created_unix: request.created_unix,
        fs,
        session_id: request.plan.session_id.clone(),
        layout: request.plan.layout.clone(),
        plan_sha256: request.plan.plan_sha256(),
        measurement_sha256,
        common_d0_samples: request.plan.common_d0.samples(),
        phase_basis: plan_basis.as_str().to_string(),
        reference_position,
        reference_channel,
        reference_index,
        reference_tie_margin_samples: tie_margin,
        reference_ambiguous,
        options: options_report,
        quality,
        polarity_basis: POLARITY_BASIS_STATEMENT,
        channels: channel_reports,
        checks,
        warnings,
    };
    let report_json = report.to_json();
    let ops_json = phase_ops_json(&report, &ops);
    let report_sha256 = sha256_hex(report_json.as_bytes());
    let ops_sha256 = sha256_hex(ops_json.as_bytes());
    let artifacts = PhaseFitArtifacts {
        report_sha256: report_sha256.clone(),
        entries: vec![
            PhaseFitArtifact {
                kind: ArtifactKind::DesignReport,
                path: PHASE_FIT_ARTIFACT_PATH.to_string(),
                bytes: report_json.into_bytes(),
                sha256: report_sha256,
            },
            PhaseFitArtifact {
                kind: ArtifactKind::CalibrationDraft,
                path: PHASE_OPS_ARTIFACT_PATH.to_string(),
                bytes: ops_json.into_bytes(),
                sha256: ops_sha256,
            },
        ],
    };
    Ok(PhaseFitOutput {
        report,
        ops,
        firs,
        artifacts,
    })
}

// ---------------------------------------------------------------------------
// 确定性 JSON（无 serde 依赖；字段顺序固定、f64 17 位有效数字）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Uint(u64),
    Int(i64),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    fn num(value: f64) -> J {
        if value.is_finite() {
            J::Num(value)
        } else {
            J::Null
        }
    }

    fn string(value: &str) -> J {
        J::Str(value.to_string())
    }

    fn option_uint(value: Option<u32>) -> J {
        match value {
            Some(value) => J::Uint(value as u64),
            None => J::Null,
        }
    }

    fn option_num(value: Option<f64>) -> J {
        match value {
            Some(value) => J::num(value),
            None => J::Null,
        }
    }

    fn option_sign(value: Option<i32>) -> J {
        match value {
            Some(value) => J::Int(value as i64),
            None => J::Null,
        }
    }

    fn write_json_string(out: &mut String, text: &str) {
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
                other if (other as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", other as u32);
                }
                other => out.push(other),
            }
        }
        out.push('"');
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            J::Uint(value) => {
                let _ = write!(out, "{value}");
            }
            J::Int(value) => {
                let _ = write!(out, "{value}");
            }
            J::Num(value) => {
                let _ = write!(out, "{value:.17e}");
            }
            J::Str(value) => J::write_json_string(out, value),
            J::Arr(values) => {
                out.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    value.write(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    J::write_json_string(out, key);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

fn position_report_json(report: &PhasePositionReport) -> J {
    J::Obj(vec![
        ("position", J::string(&report.position)),
        ("delay_raw_samples", J::num(report.delay_raw_samples)),
        ("delay_confidence", J::num(report.delay_confidence)),
        ("delay_phase_rms_deg", J::num(report.delay_phase_rms_deg)),
        ("delay_prominence", J::num(report.delay_prominence)),
        (
            "delay_points_used",
            J::Uint(report.delay_points_used as u64),
        ),
        ("polarity", J::option_sign(report.polarity)),
        ("polarity_margin", J::num(report.polarity_margin)),
        (
            "polarity_points_used",
            J::Uint(report.polarity_points_used as u64),
        ),
    ])
}

fn fir_report_json(report: &PhaseFirReport) -> J {
    J::Obj(vec![
        ("delay_samples", J::Uint(report.delay_samples as u64)),
        ("polarity", J::string(report.polarity.as_str())),
        ("taps", J::Uint(report.taps as u64)),
        ("onset_samples", J::Uint(report.onset_samples as u64)),
        ("delay_ms", J::num(report.delay_ms)),
        (
            "coefficients_sha256",
            J::string(&report.coefficients_sha256),
        ),
        ("verification_passed", J::Bool(report.verification_passed)),
        ("spec_band_required", J::Bool(report.spec_band_required)),
        ("spec_band_compliant", J::Bool(report.spec_band_compliant)),
    ])
}

fn execution_report_json(report: &PhaseExecutionReport) -> J {
    J::Obj(vec![
        (
            "residual_delay_samples",
            J::num(report.residual_delay_samples),
        ),
        (
            "reference_residual_delay_samples",
            J::num(report.reference_residual_delay_samples),
        ),
        ("delta_samples", J::num(report.delta_samples)),
        ("within_tolerance", J::Bool(report.within_tolerance)),
        (
            "compensated_polarity_sign",
            J::option_sign(report.compensated_polarity_sign),
        ),
        (
            "compensated_polarity_margin",
            J::num(report.compensated_polarity_margin),
        ),
        ("direction_ok", J::Bool(report.direction_ok)),
    ])
}

fn channel_report_json(report: &PhaseChannelReport) -> J {
    let positions = J::Arr(report.positions.iter().map(position_report_json).collect());
    J::Obj(vec![
        ("name", J::string(&report.channel)),
        ("index", J::option_uint(report.index)),
        ("reference", J::Bool(report.reference)),
        ("delay_raw_samples", J::num(report.delay_raw_samples)),
        ("delay_samples", J::Uint(report.delay_samples as u64)),
        ("delay_ms", J::num(report.delay_ms)),
        (
            "fractional_residual_samples",
            J::num(report.fractional_residual_samples),
        ),
        ("delay_confidence", J::num(report.delay_confidence)),
        ("polarity", J::string(report.polarity.as_str())),
        (
            "measured_polarity_sign",
            J::Int(report.measured_polarity_sign as i64),
        ),
        ("polarity_confidence", J::num(report.polarity_confidence)),
        ("polarity_margin_min", J::num(report.polarity_margin_min)),
        (
            "producer_polarity_agrees",
            match report.producer_polarity_agrees {
                Some(value) => J::Bool(value),
                None => J::Null,
            },
        ),
        ("positions", positions),
        ("fir", fir_report_json(&report.fir)),
        ("execution", execution_report_json(&report.execution)),
    ])
}

fn options_report_json(report: &PhaseFitOptionsReport) -> J {
    J::Obj(vec![
        (
            "delay_band_hz",
            J::Arr(vec![
                J::num(report.delay_band_hz[0]),
                J::num(report.delay_band_hz[1]),
            ]),
        ),
        (
            "polarity_band_hz",
            J::Arr(vec![
                J::num(report.polarity_band_hz[0]),
                J::num(report.polarity_band_hz[1]),
            ]),
        ),
        ("reference_position", J::string(&report.reference_position)),
        ("max_delay_ms", J::num(report.max_delay_ms)),
        ("search_range_ms", J::num(report.search_range_ms)),
        ("magnitude_mask_db", J::num(report.magnitude_mask_db)),
        ("min_points", J::Uint(report.min_points as u64)),
        ("delay_confidence_min", J::num(report.delay_confidence_min)),
        ("polarity_margin_min", J::num(report.polarity_margin_min)),
        (
            "delay_phase_rms_max_deg",
            J::num(report.delay_phase_rms_max_deg),
        ),
        (
            "execution_tolerance_samples",
            J::num(report.execution_tolerance_samples),
        ),
        (
            "require_design_quality",
            J::Bool(report.require_design_quality),
        ),
    ])
}

fn quality_summary_json(report: &PhaseFitQualitySummary) -> J {
    J::Obj(vec![
        ("required", J::Bool(report.required)),
        ("present", J::Bool(report.present)),
        ("scope", J::string(&report.scope)),
        ("plan_sha256", J::string(&report.plan_sha256)),
        ("continue_allowed", J::Bool(report.continue_allowed)),
        ("points_usable", J::Uint(report.points_usable as u64)),
        ("points_total", J::Uint(report.points_total as u64)),
    ])
}

fn checks_json(report: &PhaseFitChecks) -> J {
    J::Obj(vec![
        (
            "integer_delay_contract",
            J::Bool(report.integer_delay_contract),
        ),
        ("min_zero", J::Bool(report.min_zero)),
        ("max_within_cap", J::Bool(report.max_within_cap)),
        ("reference_is_latest", J::Bool(report.reference_is_latest)),
        (
            "fractional_residual_within_half_sample",
            J::Bool(report.fractional_residual_within_half_sample),
        ),
        ("polarity_known", J::Bool(report.polarity_known)),
        ("fir_design_ok", J::Bool(report.fir_design_ok)),
        ("fir_direction_ok", J::Bool(report.fir_direction_ok)),
        ("all_pass", J::Bool(report.all_pass)),
    ])
}

impl PhaseFitReport {
    /// 确定性 report JSON（tspi-phase-fit/v1；同输入同字节）。
    pub fn to_json(&self) -> String {
        let tool = J::Obj(vec![
            ("name", J::string(self.tool_name)),
            ("version", J::string(self.tool_version)),
        ]);
        let measurement = J::Obj(vec![("sha256", J::string(&self.measurement_sha256))]);
        let dephase = J::Obj(vec![
            ("common_d0_samples", J::Int(self.common_d0_samples)),
            ("phase_basis", J::string(&self.phase_basis)),
            ("removed_once", J::Bool(true)),
            ("convention", J::string(PHASE_CONVENTION_STATEMENT)),
        ]);
        let reference = J::Obj(vec![
            ("position", J::string(&self.reference_position)),
            ("channel", J::string(&self.reference_channel)),
            ("index", J::option_uint(self.reference_index)),
            (
                "tie_margin_samples",
                J::option_num(self.reference_tie_margin_samples),
            ),
            ("ambiguous", J::Bool(self.reference_ambiguous)),
        ]);
        let channels = J::Arr(self.channels.iter().map(channel_report_json).collect());
        let warnings = J::Arr(self.warnings.iter().map(|line| J::string(line)).collect());
        J::Obj(vec![
            ("schema", J::string(self.schema)),
            ("tool", tool),
            ("created_unix", J::Uint(self.created_unix)),
            ("fs", J::Uint(self.fs as u64)),
            ("session_id", J::string(&self.session_id)),
            ("layout", J::string(&self.layout)),
            ("plan_sha256", J::string(&self.plan_sha256)),
            ("measurement", measurement),
            ("dephase", dephase),
            ("delay_contract", J::string(DELAY_CONTRACT_STATEMENT)),
            ("fir_execution", J::string(FIR_EXECUTION_STATEMENT)),
            ("reference", reference),
            ("options", options_report_json(&self.options)),
            ("quality", quality_summary_json(&self.quality)),
            ("polarity_basis", J::string(self.polarity_basis)),
            ("channels", channels),
            ("checks", checks_json(&self.checks)),
            ("warnings", warnings),
        ])
        .to_json()
    }
}

fn phase_ops_json(report: &PhaseFitReport, ops: &[PhaseChannelOp]) -> String {
    let tool = J::Obj(vec![
        ("name", J::string(report.tool_name)),
        ("version", J::string(report.tool_version)),
    ]);
    let channels = J::Arr(
        ops.iter()
            .map(|op| {
                J::Obj(vec![
                    ("name", J::string(&op.channel)),
                    ("index", J::option_uint(op.index)),
                    ("delay_samples", J::Uint(op.delay_samples as u64)),
                    ("delay_ms", J::num(op.delay_ms)),
                    ("polarity", J::string(op.polarity.as_str())),
                ])
            })
            .collect(),
    );
    J::Obj(vec![
        ("schema", J::string(PHASE_OPS_SCHEMA)),
        ("tool", tool),
        ("fs", J::Uint(report.fs as u64)),
        ("plan_sha256", J::string(&report.plan_sha256)),
        (
            "measurement",
            J::Obj(vec![("sha256", J::string(&report.measurement_sha256))]),
        ),
        ("common_d0_samples", J::Int(report.common_d0_samples)),
        ("phase_basis", J::string(&report.phase_basis)),
        ("reference_channel", J::string(&report.reference_channel)),
        ("reference_index", J::option_uint(report.reference_index)),
        ("phase_convention", J::string(PHASE_CONVENTION_STATEMENT)),
        ("delay_contract", J::string(DELAY_CONTRACT_STATEMENT)),
        ("fir_execution", J::string(FIR_EXECUTION_STATEMENT)),
        ("polarity_basis", J::string(POLARITY_BASIS_STATEMENT)),
        ("channels", channels),
    ])
    .to_json()
}

// ---------------------------------------------------------------------------
// artifacts（T60 StageCtx / 目录写盘）
// ---------------------------------------------------------------------------

/// 一个待发布的 phase-fit artifact。
#[derive(Clone, Debug)]
pub struct PhaseFitArtifact {
    /// artifact 类别（job 登记用）。
    pub kind: ArtifactKind,
    /// 相对路径（job artifacts_dir 语义；含 phase/ 子目录）。
    pub path: String,
    /// 文件字节（确定性 JSON）。
    pub bytes: Vec<u8>,
    /// 内容 sha256。
    pub sha256: String,
}

/// 待发布 artifact 集合（report + ops）。
#[derive(Clone, Debug)]
pub struct PhaseFitArtifacts {
    /// report 内容 sha256（与 entries[0].sha256 相同）。
    pub report_sha256: String,
    /// 待登记/写盘条目。
    pub entries: Vec<PhaseFitArtifact>,
}

impl PhaseFitArtifacts {
    /// 直接写到一个目录（相对路径含 phase/ 子目录，自动建父目录）。
    pub fn write_to_dir(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut written = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let path = dir.join(&entry.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &entry.bytes)?;
            written.push(path);
        }
        Ok(written)
    }

    /// 通过 T60 StageCtx 登记 artifact（同 job 重跑幂等；内容变化由 job.rs 拒收）。
    pub fn write_stage_artifacts(
        &self,
        ctx: &StageCtx<'_>,
    ) -> Result<Vec<ArtifactRecord>, StageFailure> {
        let mut records = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            records.push(ctx.write_artifact(
                entry.kind.clone(),
                &entry.path,
                &entry.bytes,
                &entry.sha256,
            )?);
        }
        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// 私有测试（include! tests/T65；integrator 不要再注册进 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t65_tests {
    use super::*;

    include!("../tests/T65/phase_fit_cases.rs");
}
