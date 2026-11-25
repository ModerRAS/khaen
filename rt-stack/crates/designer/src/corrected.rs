//! [NONRT] corrected / lowdelay 显式幅相族 FIR 设计（WBS T37）—— T37 写域。
//!
//! 本模块只实现 corrected/lowdelay 家族（spec fir-toolbox v1.2-draft §3.6）：目标来自 T11
//! 目标导入（§6，**必须带显式相位**：phase_ref=absolute 或 delay_removed），用频域加权最小二乘
//! 在**因果冲激响应**上逼近**复目标**（幅度+相位一起拟合），并独立复算复误差判据。
//!
//! 明确非目标 / 不得冒充：
//! - **不把最小相位（或线性相位）当原矩阵等价**。corrected/lowdelay 是显式幅相目标设计：
//!   报告必须写 magnitude_only 与 not-equivalent 声明；EquivalenceClaim 非 None 直接
//!   E_FIR_EQUIV_CLAIM。模块内 min_phase_from_magnitude_table 只是**诊断/oracle**（用于测试
//!   证明最小相位替代会违反相位门），不是设计家族输出，也不得被当成本家族结果。
//! - 不生成 WAV / tspi-fir/v1 manifest / 20-20k Endpoint 报告文件（T55）；不导入目标文件（T11）；
//!   不做 P/G 融合（T12）；不做运行期/板端/RT 路径；不判定 15/25/40 ms 整链预算（T56/T26）。
//! - 不做 target_sha256 加权（spec §3.6 的 weights: target_sha256 语义在 v1.2-draft 未定义，
//!   显式 E_FIR_FAMILY 拒收，不静默猜一个）。
//!
//! 权威契约：
//! - spec/fir-toolbox.md v1.2-draft §3.6（参数/参考构造/判据 1–8）、§1.2（响应测量程序）、
//!   §1.3（支撑/延迟）、§1.4（振铃）、§1.5（参考归一化）、§1.6（精度/确定性）、
//!   §2.2/§2.5（manifest 字段与 E_FIR_* 码）、§6（目标导入/相位语义/消费映射）、§7.2（准入边界）。
//! - evidence/T04/RULING-fs2-2026-11-17.md（带相位目标点禁止 fs/2，由 T11 拒收）。
//!
//! 设计算法（可复现，作者裁决记录见 evidence/T37/MANIFEST.md）：
//! 1. 取对称 FFT 频点集合 S（正/负频成对，权重 1 于 [band_lo, band_hi]），复目标
//!    T(f)=10^(mag_db/20)·exp(j·φ_tot(f))；φ_tot 语义严格按 §6.6：absolute 用给定总相位；
//!    delay_removed 用 φ_ex − 360·f·D/fs（D 必须=target_delay_samples）。
//! 2. 实系数因果 h（L=taps）上的最小二乘 normal equation 是 Toeplitz 方程
//!    T·h = b，T[(p−n)] = Re Σ_{k∈S} e^{jω_k(p−n)}，b = Re Σ_{k∈S} t_k e^{−jω_k p}；
//!    用一次 IFFT 得 b、用 circulant 嵌入（w 的 IFFT 取前 L 点）做 Toeplitz matvec，
//!    CG 迭代求解（加 ridge 正则，报告实际 ridge/CG 迭代/相对残差）。N_fft 为 2 的幂，
//!    带宽内频点数不足（< 2·taps）即 E_FIR_BUDGET/INFEASIBLE，不静默降级。
//! 3. 因果约束：对 φ_tot 数值求 GD（§1.2 口径），带内 min GD < −0.01 sample 即
//!    E_FIR_NONCAUSAL（目标在声明 D 下不可因果，不猜、不截）；h 本身按构造因果。
//! 4. 独立验证：不复用 LS 网格，按 §1.2 测量网格（对数 20 Hz–min(20 kHz,0.499·fs)，
//!    N>=max(8192,8·L)）用直接 DTFT 复算 |H|/相位/解析 GD，逐条判 §3.6 判据并输出
//!    E_FIR_* 诚实报告（不达标 = accepted=false，design_strict 给 E_FIR_BUDGET/INFEASIBLE）。
//!
//! 裁决/假设（写入 evidence/T37/MANIFEST.md §6，非规范变更）：
//! - A1：rms_error_db 解释为归一化 RMS 复误差 20log10( sqrt(mean|H−T|² / mean|T|²) )，
//!   下限 −240 dB；比宣称值小即通过。
//! - A2：weights: uniform 可算；target_sha256 未定义 → 拒收。
//! - A3：LS ridge 默认 1e-6（系数尺度），不改变目标幅度（无隐藏整体增益）；若 CG 不收敛
//!   以报告失败（accepted=false），不静默放宽。
//! - A4：analysis 默认 normative（§1.2），测试可用 compact；两者网格定义均落盘在报告。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次 design 系数逐位一致（无时间戳/随机数）。

use std::f64::consts::PI;
use std::fmt;

use crate::targets::{
    log_frequency_grid, PhaseRef, PhaseTargetClass, TargetBudget, TargetCurve, TargetError,
    TargetErrorCode, TargetFamily,
};

/// 家族 id（§1.7/§2.2）。
pub const FAMILY_CORRECTED: &str = "corrected";
/// 家族 id（§1.7/§2.2；lowdelay 是本模块的同一显式幅相设计族的低延迟模式）。
pub const FAMILY_LOWDELAY: &str = "lowdelay";
/// 系数产物 schema（§2.2；本模块只做设计/验证，WAV/manifest 由 T55 落盘）。
pub const FIR_SCHEMA: &str = "tspi-fir/v1";
/// 规范 §3.6/§2.2：最小 taps。
pub const MIN_TAPS: usize = 8;
/// 规范 §2.1/§7.2：设计 taps 上限。
pub const MAX_TAPS: usize = 16_384;
/// 规范 §7.2：常规 profile 每路由有效支撑上限（仅事实常量，准入归 T56）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;
/// 规范 §7.2：extended profile 上限。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;
/// 设计默认采样率（Hz）。
pub const DEFAULT_FS: f64 = 48_000.0;
/// 规范 §1.2/§6：测量/目标下界（Hz）。
pub const BAND_LO_HZ: f64 = 20.0;
/// 规范 §1.2/§6：测量上界（Hz）。
pub const BAND_HI_HZ: f64 = 20_000.0;
/// 规范 §1.2/§3.1：Nyquist 护栏比例 0.499·fs。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;
/// 规范 §1.2：|H| 下限（−240 dB）。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;
/// 规范 §3.6：默认带内幅度误差上限（dB）。
pub const DEFAULT_MAX_MAG_ERROR_DB: f64 = 1.0;
/// 规范 §3.6：默认带内相位误差上限（deg）。
pub const DEFAULT_MAX_PHASE_ERROR_DEG: f64 = 5.0;
/// 规范 §3.6：默认归一化 RMS 复误差上限（dB，解释见 A1）。
pub const DEFAULT_RMS_ERROR_DB: f64 = 0.5;
/// LS ridge 默认值（A3；相对系数尺度）。
pub const DEFAULT_RIDGE: f64 = 1e-6;
/// 规范 §3.6 判据 1：|D_used − D_target| <= 0.01 sample。
pub const DELAY_CONSISTENCY_SAMPLES: f64 = 0.01;
/// 因果容差（samples）：目标 GD 低于此值即 E_FIR_NONCAUSAL。
pub const CAUSALITY_TOLERANCE_SAMPLES: f64 = 0.01;
/// 规范 §1.5/§2.2 规则 4：参考归一化容差（dB）。
pub const REFERENCE_TOL_DB: f64 = 0.01;
/// 规范 §6.3：target_delay_samples 上限 200 ms。
pub const MAX_DELAY_MS: f64 = 200.0;
/// LS 最小 FFT 尺寸。
pub const MIN_LS_FFT: usize = 4_096;
/// LS 最大 FFT 尺寸（更大 taps 的网格分辨率不足 -> INFEASIBLE，不静默降级）。
pub const MAX_LS_FFT: usize = 65_536;
/// LS FFT 尺寸 = 16*taps（至少 MIN_LS_FFT，至多 MAX_LS_FFT）。
pub const LS_FFT_PER_TAP: usize = 16;
/// 带内每个未知量所需最少 FFT 频点数（网格可判性）。
pub const MIN_LS_BINS_PER_TAP: usize = 2;
/// CG 最大迭代次数。
pub const CG_MAX_ITERATIONS: usize = 400;
/// CG 相对残差收敛门（对 (T+ridge·I)h=b）。
pub const CG_RELATIVE_TOLERANCE: f64 = 1e-11;
/// 规范 §1.3：有效支撑 eps 相对门。
pub const EFFECTIVE_SUPPORT_EPS: f64 = 1e-6;
/// 规范 §1.4：振铃排除窗 0.333 ms。
pub const RINGING_WINDOW_SEC: f64 = 0.000333;
/// lowdelay 默认 delay_max_samples（48k 下 1.333 ms）；调用方可显式覆盖。
pub const DEFAULT_LOWDELAY_MAX_SAMPLES: u32 = 64;
/// 规范 §3.2 判据 2 的最小相位一致性门（diagnostic 参照；本家族不作等价声明）。
pub const MIN_PHASE_CONSISTENCY_LIMIT_DEG: f64 = 2.0;
/// 最小相位诊断默认 FFT 下限。
pub const MIN_PHASE_FFT_MIN: usize = 8_192;
/// 最小相位诊断 FFT 上限。
pub const MIN_PHASE_FFT_MAX: usize = 65_536;
/// 规范 §3.6 判据 6 的必填声明（幅度-only 设计）。
pub const NOT_EQUIVALENT_MAGNITUDE_ONLY: &str =
    "magnitude_only: true; not equivalent to measured complex matrix";
/// 显式幅相设计的非等价声明（不得把最小相位/线性相位当原矩阵等价）。
pub const NOT_EQUIVALENT_EXPLICIT_COMPLEX: &str =
    "explicit amplitude+phase target; not equivalent to measured complex matrix and not a minimum-phase or linear-phase reformulation";
/// 规范 §6.8：flat 内置目标的幅度（diagnostic/tests 用）。
pub const FLAT_TARGET_DB: f64 = 0.0;

/// E_FIR_* 失败码字符串（规范 §2.5；含 T11 透传的 E_TGT_* 码）。
pub const E_FIR_SCHEMA: &str = "E_FIR_SCHEMA";
/// 频带/参数非法。
pub const E_FIR_BAND: &str = "E_FIR_BAND";
/// taps 越界/长度不符。
pub const E_FIR_TAPS: &str = "E_FIR_TAPS";
/// 延迟非法/不一致。
pub const E_FIR_DELAY: &str = "E_FIR_DELAY";
/// 系数非有限/数值失败。
pub const E_FIR_NUMERIC: &str = "E_FIR_NUMERIC";
/// 参考归一化失败。
pub const E_FIR_REF: &str = "E_FIR_REF";
/// 支撑/可行性/延迟/融合长度超预算（INFEASIBLE，不静默截断）。
pub const E_FIR_BUDGET: &str = "E_FIR_BUDGET";
/// 目标在声明 D 下不可因果。
pub const E_FIR_NONCAUSAL: &str = "E_FIR_NONCAUSAL";
/// corrected 冒充 linear/min 等价。
pub const E_FIR_EQUIV_CLAIM: &str = "E_FIR_EQUIV_CLAIM";
/// 家族自洽/互斥检查失败。
pub const E_FIR_FAMILY: &str = "E_FIR_FAMILY";

/// corrected/lowdelay 的 E_FIR_*/E_TGT_* 错误（T11 错误原样透传码，不降级）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrectedError {
    /// 失败码（规范 §2.5 / §6.9 字面值）。
    pub code: &'static str,
    /// 人读细节（含期望/实际值）。
    pub message: String,
}

impl CorrectedError {
    /// 构造错误。
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 码字符串。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 细节。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CorrectedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CorrectedError {}

impl From<TargetError> for CorrectedError {
    fn from(error: TargetError) -> Self {
        // T11 的 E_TGT_* 码原样透传（§3.6 判据 1：目标无效不设计）。
        let code = error.code_str();
        let detail = match error.field {
            Some(field) => format!("{field}: {}", error.detail),
            None => error.detail,
        };
        Self {
            code,
            message: detail,
        }
    }
}

fn err(code: &'static str, message: impl Into<String>) -> CorrectedError {
    CorrectedError::new(code, message)
}

/// 设计模式（spec §3.6 参数 mode: corrected|lowdelay）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectedMode {
    /// 显式复目标修正；D 由设计声明（absolute 目标只报告，delay_removed 目标必须= D_target）。
    Corrected,
    /// 低延迟折中：D <= delay_max_samples，必须报告误差-延迟折中扫描（§3.6 判据 5）。
    LowDelay,
}

impl CorrectedMode {
    /// 家族 id 字面值（§1.7）。
    pub fn id(self) -> &'static str {
        match self {
            CorrectedMode::Corrected => FAMILY_CORRECTED,
            CorrectedMode::LowDelay => FAMILY_LOWDELAY,
        }
    }

    /// 对应的 T11 目标消费族（§6.10：两族都必须 phase）。
    pub fn target_family(self) -> TargetFamily {
        match self {
            CorrectedMode::Corrected => TargetFamily::Corrected,
            CorrectedMode::LowDelay => TargetFamily::LowDelay,
        }
    }
}

/// §3.6 参数 weights。target_sha256 语义在 v1.2-draft 未定义，显式拒收（A2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weighting {
    /// 均匀权重（带内每个 FFT 频点权重 1）。
    Uniform,
    /// 规范列出但语义未定义：本实现拒收（E_FIR_FAMILY，UNRULED），不静默实现猜测语义。
    TargetSha256,
}

impl Weighting {
    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            Weighting::Uniform => "uniform",
            Weighting::TargetSha256 => "target_sha256",
        }
    }
}

/// 等价性声明（§3.8：corrected 必须带显式相位目标，不冒充 linear/min 等价）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EquivalenceClaim {
    /// 不声明任何 family 等价（唯一合法值）。
    None,
    /// 声称与 linear 等价 -> E_FIR_EQUIV_CLAIM。
    LinearPhase,
    /// 声称与最小相位等价 -> E_FIR_EQUIV_CLAIM（尤其不得当原矩阵等价）。
    MinimumPhase,
}

impl EquivalenceClaim {
    /// 字面值。
    pub fn as_str(self) -> &'static str {
        match self {
            EquivalenceClaim::None => "none",
            EquivalenceClaim::LinearPhase => "linear_phase",
            EquivalenceClaim::MinimumPhase => "minimum_phase",
        }
    }
}

/// 群延迟类别（§3.6 判据 4：报告事实类别，不预设 corrected 必为频变）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupDelayClass {
    /// 带内 GD 统计起伏 <= 0.01 sample（纯延迟目标可达）。
    Constant,
    /// 带内 GD 频变（显式幅相目标的常态）。
    FrequencyVarying,
}

impl GroupDelayClass {
    /// 字面值（manifest §2.2 group_delay.class）。
    pub fn as_str(self) -> &'static str {
        match self {
            GroupDelayClass::Constant => "constant",
            GroupDelayClass::FrequencyVarying => "frequency_varying",
        }
    }
}

/// corrected/lowdelay 设计参数（spec §3.6 参数表；构造器给默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct CorrectedSpec {
    /// corrected | lowdelay。
    pub mode: CorrectedMode,
    /// 运行采样率（Hz）；必须与目标 runtime_rate 一致（§6.3）。
    pub fs: f64,
    /// 支撑长度 L（taps），[MIN_TAPS, MAX_TAPS]。
    pub taps: usize,
    /// 声明体延迟 D（整数 samples，>=0）。
    pub delay_samples: u32,
    /// lowdelay 的 D 上限（corrected 必须 None；lowdelay 必须 Some）。
    pub delay_max_samples: Option<u32>,
    /// 声明频带 [lo,hi]（Hz），20 <= lo < hi <= min(20000, 0.499·fs)。
    pub band_hz: [f64; 2],
    /// 带内幅度误差上限（dB，§3.6 判据 2）。
    pub max_mag_error_db: f64,
    /// 带内相位误差上限（deg，§3.6 判据 2）。
    pub max_phase_error_deg: f64,
    /// 归一化 RMS 复误差上限（dB，A1 解释）。
    pub rms_error_db: f64,
    /// 权重（uniform；target_sha256 拒收）。
    pub weights: Weighting,
    /// LS ridge 正则（>=0；默认 DEFAULT_RIDGE）。
    pub ridge: f64,
    /// 显式允许幅度-only 目标（§3.6 判据 6 的诚实降级；默认 false 即 E_TGT_PHASE_UNSUPPORTED）。
    pub allow_magnitude_only: bool,
    /// 等价性声明；非 None 即 E_FIR_EQUIV_CLAIM。
    pub equivalence_claim: EquivalenceClaim,
}

impl CorrectedSpec {
    /// corrected 构造器：默认 uniform 权重、默认误差门/ridge、无等价声明。
    pub fn corrected(fs: f64, taps: usize, delay_samples: u32, band_hz: [f64; 2]) -> Self {
        Self {
            mode: CorrectedMode::Corrected,
            fs,
            taps,
            delay_samples,
            delay_max_samples: None,
            band_hz,
            max_mag_error_db: DEFAULT_MAX_MAG_ERROR_DB,
            max_phase_error_deg: DEFAULT_MAX_PHASE_ERROR_DEG,
            rms_error_db: DEFAULT_RMS_ERROR_DB,
            weights: Weighting::Uniform,
            ridge: DEFAULT_RIDGE,
            allow_magnitude_only: false,
            equivalence_claim: EquivalenceClaim::None,
        }
    }

    /// lowdelay 构造器：D 默认取 delay_max_samples（调用方可 with_delay 覆盖，覆盖值必须 <= D_max）。
    pub fn lowdelay(fs: f64, taps: usize, delay_max_samples: u32, band_hz: [f64; 2]) -> Self {
        let mut spec = Self::corrected(fs, taps, delay_max_samples, band_hz);
        spec.mode = CorrectedMode::LowDelay;
        spec.delay_max_samples = Some(delay_max_samples);
        spec
    }

    /// 覆盖声明延迟 D（samples）。
    pub fn with_delay(mut self, delay_samples: u32) -> Self {
        self.delay_samples = delay_samples;
        self
    }

    /// 覆盖 lowdelay D 上限。
    pub fn with_delay_max(mut self, delay_max_samples: u32) -> Self {
        self.delay_max_samples = Some(delay_max_samples);
        self
    }

    /// 覆盖误差门（dB / deg / dB）。
    pub fn with_limits(
        mut self,
        max_mag_error_db: f64,
        max_phase_error_deg: f64,
        rms_error_db: f64,
    ) -> Self {
        self.max_mag_error_db = max_mag_error_db;
        self.max_phase_error_deg = max_phase_error_deg;
        self.rms_error_db = rms_error_db;
        self
    }

    /// 覆盖权重。
    pub fn with_weights(mut self, weights: Weighting) -> Self {
        self.weights = weights;
        self
    }

    /// 覆盖 ridge。
    pub fn with_ridge(mut self, ridge: f64) -> Self {
        self.ridge = ridge;
        self
    }

    /// 显式允许幅度-only 目标（报告 magnitude_only=true 与非等价声明）。
    pub fn with_allow_magnitude_only(mut self, allow: bool) -> Self {
        self.allow_magnitude_only = allow;
        self
    }

    /// 设置等价性声明（非 None 会在 validate 时 E_FIR_EQUIV_CLAIM）。
    pub fn with_equivalence_claim(mut self, claim: EquivalenceClaim) -> Self {
        self.equivalence_claim = claim;
        self
    }

    /// 家族 id 字面值。
    pub fn family_id(&self) -> &'static str {
        self.mode.id()
    }

    /// 对应 T11 目标消费族。
    pub fn target_family(&self) -> TargetFamily {
        self.mode.target_family()
    }

    /// delay_ms = D/fs·1000。
    pub fn delay_ms(&self) -> f64 {
        self.delay_samples as f64 / self.fs * 1000.0
    }
}

/// 通过 validate 的参数/派生量（构造与验证共用同一口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedCorrectedSpec {
    /// 原参数。
    pub spec: CorrectedSpec,
    /// 声明频带 [lo,hi]。
    pub band_hz: [f64; 2],
    /// D/fs·1000。
    pub delay_ms: f64,
    /// 目标 profile 的延迟上限（samples）——delay_removed 时 D 必须等于它。
    pub target_delay_max_samples: u32,
    /// 验证期警告（非拒收）。
    pub warnings: Vec<String>,
}

/// 参数校验（§3.6 参数/判据 1/5/8；§7.2 支撑上限）。失败带 E_FIR_* 码。
pub fn validate_spec(spec: &CorrectedSpec) -> Result<ValidatedCorrectedSpec, CorrectedError> {
    let mut warnings: Vec<String> = Vec::new();
    if !spec.fs.is_finite() || spec.fs <= 0.0 {
        return Err(err(
            E_FIR_BAND,
            format!("fs 必须有限且 >0，实际 {}", spec.fs),
        ));
    }
    if !(8_000.0..=192_000.0).contains(&spec.fs) {
        return Err(err(
            E_FIR_BAND,
            format!("fs={} 超出工具支持范围 8000..=192000 Hz", spec.fs),
        ));
    }
    if spec.taps < MIN_TAPS || spec.taps > MAX_TAPS {
        return Err(err(
            E_FIR_TAPS,
            format!("taps 必须在 [{MIN_TAPS}, {MAX_TAPS}]，实际 {}", spec.taps),
        ));
    }
    if !spec.max_mag_error_db.is_finite() || spec.max_mag_error_db <= 0.0 {
        return Err(err(
            E_FIR_BAND,
            format!(
                "max_mag_error_db 必须有限且 >0，实际 {}",
                spec.max_mag_error_db
            ),
        ));
    }
    if !spec.max_phase_error_deg.is_finite() || spec.max_phase_error_deg <= 0.0 {
        return Err(err(
            E_FIR_BAND,
            format!(
                "max_phase_error_deg 必须有限且 >0，实际 {}",
                spec.max_phase_error_deg
            ),
        ));
    }
    if !spec.rms_error_db.is_finite() || spec.rms_error_db <= 0.0 {
        return Err(err(
            E_FIR_BAND,
            format!("rms_error_db 必须有限且 >0，实际 {}", spec.rms_error_db),
        ));
    }
    if !spec.ridge.is_finite() || spec.ridge < 0.0 || spec.ridge > 1e-2 {
        return Err(err(
            E_FIR_BAND,
            format!("ridge 必须有限且 ∈ [0,1e-2]，实际 {}", spec.ridge),
        ));
    }
    if spec.weights != Weighting::Uniform {
        return Err(err(
            E_FIR_FAMILY,
            format!(
                "weights={} 在 fir-toolbox v1.2-draft 中无定义（UNRULED）：本实现只接受 uniform，不猜语义",
                spec.weights.as_str()
            ),
        ));
    }
    if spec.equivalence_claim != EquivalenceClaim::None {
        return Err(err(
            E_FIR_EQUIV_CLAIM,
            format!(
                "corrected/lowdelay 是显式幅相目标族，不声明 {} 等价（E_FIR_EQUIV_CLAIM）；最小相位/线性相位不得当原矩阵等价",
                spec.equivalence_claim.as_str()
            ),
        ));
    }

    let lo = spec.band_hz[0];
    let hi = spec.band_hz[1];
    if !lo.is_finite() || !hi.is_finite() || !(BAND_LO_HZ <= lo && lo < hi) {
        return Err(err(
            E_FIR_BAND,
            format!("band_hz 必须有限且 20 <= lo < hi，实际 [{lo}, {hi}]"),
        ));
    }
    let hi_limit = BAND_HI_HZ.min(NYQUIST_GUARD_FRACTION * spec.fs);
    if hi > hi_limit {
        return Err(err(
            E_FIR_BAND,
            format!("band_hz 上界 {hi} 必须 <= min(20000, 0.499·fs={hi_limit}) Hz（§1.2/§7.2）"),
        ));
    }

    let max_delay_samples = (MAX_DELAY_MS / 1000.0 * spec.fs).floor() as u32;
    if spec.delay_samples > max_delay_samples {
        return Err(err(
            E_FIR_DELAY,
            format!(
                "delay_samples={} 超出 200 ms 上限（{} samples @ {} Hz）",
                spec.delay_samples, max_delay_samples, spec.fs
            ),
        ));
    }
    match spec.mode {
        CorrectedMode::Corrected => {
            if spec.delay_max_samples.is_some() {
                return Err(err(
                    E_FIR_FAMILY,
                    "corrected 模式不接受 delay_max_samples（仅 lowdelay 使用）".to_string(),
                ));
            }
        }
        CorrectedMode::LowDelay => {
            let Some(d_max) = spec.delay_max_samples else {
                return Err(err(
                    E_FIR_DELAY,
                    "lowdelay 必须给 delay_max_samples（§3.6 判据 5）".to_string(),
                ));
            };
            if d_max < 1 {
                return Err(err(
                    E_FIR_DELAY,
                    "lowdelay 的 delay_max_samples 必须 >=1，否则无法报告误差-延迟折中（§3.6 判据 5）"
                        .to_string(),
                ));
            }
            if d_max > max_delay_samples {
                return Err(err(
                    E_FIR_DELAY,
                    format!(
                        "delay_max_samples={d_max} 超出 200 ms 上限（{} samples）",
                        max_delay_samples
                    ),
                ));
            }
            if spec.delay_samples > d_max {
                return Err(err(
                    E_FIR_DELAY,
                    format!(
                        "lowdelay 声明 D={} > delay_max_samples={d_max}（§3.6 判据 5）",
                        spec.delay_samples
                    ),
                ));
            }
        }
    }

    let delay_ms = spec.delay_samples as f64 / spec.fs * 1000.0;
    if !(spec.fs == 48_000.0 || spec.fs == 96_000.0) {
        warnings.push(format!(
            "fs={} 不是 manifest 允许的 48000/96000（§2.2）；设计本身成立但产物需重设计到运行 fs",
            spec.fs
        ));
    }
    if spec.delay_samples as usize >= spec.taps {
        warnings.push(format!(
            "D={} >= taps={}：因果支撑窗口内装不下纯延迟，复目标逼近可能显著受限（报告按实测判据）",
            spec.delay_samples, spec.taps
        ));
    }
    Ok(ValidatedCorrectedSpec {
        spec: spec.clone(),
        band_hz: [lo, hi],
        delay_ms,
        target_delay_max_samples: max_delay_samples,
        warnings,
    })
}

/// 响应测量网格点数（规范 §1.2：N >= max(8192, 8·L)）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnalysisOptions {
    /// 主测量网格点数（20 Hz–min(20 kHz,0.499·fs) 的对数网格）。
    pub mag_points: usize,
}

impl AnalysisOptions {
    /// 规范口径：N = max(8192, 8·L)。
    pub fn normative_for(taps: usize) -> Self {
        Self {
            mag_points: (8 * taps).max(8_192),
        }
    }

    /// 快速自检口径（测试/预检用；正式报告必须 normative）。
    pub fn compact(taps: usize) -> Self {
        Self {
            mag_points: (2 * taps).clamp(1_024, 4_096),
        }
    }
}

/// 设计选项：独立验证网格 + 可选 LS FFT 尺寸覆盖（测试用；None=自动）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesignOptions {
    /// 验证网格。
    pub analysis: AnalysisOptions,
    /// 覆盖 LS FFT 尺寸（必须 2 的幂且 >= 2·taps；None=自动 16·taps 夹到 [4096,65536]）。
    pub ls_fft_size: Option<usize>,
}

impl DesignOptions {
    /// 默认：normative 验证网格，自动 LS FFT。
    pub fn normative(taps: usize) -> Self {
        Self {
            analysis: AnalysisOptions::normative_for(taps),
            ls_fft_size: None,
        }
    }

    /// 快速口径（测试用）。
    pub fn compact(taps: usize) -> Self {
        Self {
            analysis: AnalysisOptions::compact(taps),
            ls_fft_size: None,
        }
    }

    /// 覆盖 LS FFT 尺寸（便于测试限时；不影响验证网格）。
    pub fn with_ls_fft_size(mut self, n_fft: usize) -> Self {
        self.ls_fft_size = Some(n_fft);
        self
    }
}

// ---------------------------------------------------------------------------
// 迭代式 radix-2 Cooley–Tukey FFT（f64，离线设计工具内部实现；无第三方依赖）
// ---------------------------------------------------------------------------

fn is_pow2(n: usize) -> bool {
    n != 0 && (n & (n - 1)) == 0
}

fn next_pow2_at_least(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// 固定长度 radix-2 FFT 计划（bit-reversal 表 + 逐级旋转因子）。
#[derive(Debug)]
struct Fft {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<Vec<(f64, f64)>>,
}

impl Fft {
    fn new(n: usize) -> Result<Self, CorrectedError> {
        if !is_pow2(n) || n < 2 {
            return Err(err(
                E_FIR_SCHEMA,
                format!("FFT 尺寸 {n} 非法：必须是 2 的幂且 >=2"),
            ));
        }
        let bits = n.trailing_zeros();
        let mut rev = vec![0u32; n];
        for (i, r) in rev.iter_mut().enumerate() {
            *r = (i as u32).reverse_bits() >> (32 - bits);
        }
        let mut tw = Vec::with_capacity(bits as usize);
        let mut len = 2usize;
        while len <= n {
            let half = len / 2;
            let mut stage = Vec::with_capacity(half);
            for k in 0..half {
                let ang = -2.0 * PI * (k as f64) / (len as f64);
                stage.push((ang.cos(), ang.sin()));
            }
            tw.push(stage);
            len <<= 1;
        }
        Ok(Self { n, rev, tw })
    }

    fn forward(&self, re: &mut [f64], im: &mut [f64]) {
        debug_assert_eq!(re.len(), self.n);
        debug_assert_eq!(im.len(), self.n);
        for i in 0..self.n {
            let j = self.rev[i] as usize;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2usize;
        for stage in &self.tw {
            let half = len / 2;
            for start in (0..self.n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = stage[k];
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    fn inverse(&self, re: &mut [f64], im: &mut [f64]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.forward(re, im);
        let inv = 1.0 / self.n as f64;
        for v in re.iter_mut() {
            *v *= inv;
        }
        for v in im.iter_mut() {
            *v = -*v * inv;
        }
    }
}

// ---------------------------------------------------------------------------
// 直接响应复算（O(L)，相位递推避免逐点三角函数；§1.2 口径）
// ---------------------------------------------------------------------------

/// 复频响 H(f)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreqResponse {
    /// 实部。
    pub re: f64,
    /// 虚部。
    pub im: f64,
}

impl FreqResponse {
    /// |H|。
    pub fn magnitude(&self) -> f64 {
        self.re.hypot(self.im)
    }

    /// 20·log10(max(|H|,1e-12))（§1.2 下限 −240 dB）。
    pub fn magnitude_db(&self) -> f64 {
        20.0 * self.magnitude().max(MAGNITUDE_FLOOR).log10()
    }

    /// 相位（度，atan2 分支）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re) * 180.0 / PI
    }
}

/// 直接 DTFT 求 H(f)（O(L)；复指数递推 + 每 64 点重锚，独立于 LS 构造）。
pub fn response_at(coeffs: &[f64], fs: f64, f_hz: f64) -> FreqResponse {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return FreqResponse {
            re: f64::NAN,
            im: f64::NAN,
        };
    }
    let (re, im, _) = eval_h_and_gd(coeffs, fs, f_hz);
    FreqResponse { re, im }
}

/// 20·log10|H(f)|（下限 −240 dB）。
pub fn magnitude_db_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).magnitude_db()
}

/// 解析群延迟 GD(f)=Re(G/H)，G=Σ n·h[n]e^{−jωn}（samples；不是有限差分）。
pub fn group_delay_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return f64::NAN;
    }
    let (re, im, gd) = eval_h_and_gd(coeffs, fs, f_hz);
    let den = re * re + im * im;
    if den <= 0.0 || !den.is_finite() {
        return f64::NAN;
    }
    gd
}

fn eval_h_and_gd(coeffs: &[f64], fs: f64, f_hz: f64) -> (f64, f64, f64) {
    let omega = 2.0 * PI * f_hz / fs;
    let step_re = (-omega).cos();
    let step_im = (-omega).sin();
    let mut cur_re = 1.0_f64;
    let mut cur_im = 0.0_f64;
    let mut h_re = 0.0_f64;
    let mut h_im = 0.0_f64;
    let mut b_re = 0.0_f64;
    let mut b_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        h_re += h * cur_re;
        h_im += h * cur_im;
        let nf = n as f64;
        b_re += nf * h * cur_re;
        b_im += nf * h * cur_im;
        let next_re = cur_re * step_re - cur_im * step_im;
        let next_im = cur_re * step_im + cur_im * step_re;
        cur_re = next_re;
        cur_im = next_im;
        if n % 64 == 63 {
            let angle = -omega * (nf + 1.0);
            cur_re = angle.cos();
            cur_im = angle.sin();
        }
    }
    let den = h_re * h_re + h_im * h_im;
    let gd = if den > 0.0 && den.is_finite() {
        (b_re * h_re + b_im * h_im) / den
    } else {
        f64::NAN
    };
    (h_re, h_im, gd)
}

/// 有效支撑（§1.3）：(first, last, effective_taps)，eps 相对 max|h|。
pub fn effective_support(coeffs: &[f64], eps: f64) -> (usize, usize, usize) {
    let mut max_abs = 0.0_f64;
    for &h in coeffs {
        max_abs = max_abs.max(h.abs());
    }
    if coeffs.is_empty() || !max_abs.is_finite() || max_abs == 0.0 {
        return (0, 0, 0);
    }
    let threshold = eps.max(0.0) * max_abs;
    let first = coeffs.iter().position(|h| h.abs() > threshold);
    let last = coeffs.iter().rposition(|h| h.abs() > threshold);
    match (first, last) {
        (Some(fi), Some(la)) => (fi, la, la - fi + 1),
        _ => (0, 0, 0),
    }
}

/// wrap 到 (−180,180] 的度数（相位比较口径 §1.2）。
pub fn wrap_deg(deg: f64) -> f64 {
    if !deg.is_finite() {
        return deg;
    }
    let mut x = deg.rem_euclid(360.0);
    if x > 180.0 {
        x -= 360.0;
    }
    x
}

/// dB -> 幅度（下限 1e-12）。
pub fn db_to_amplitude(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0).max(MAGNITUDE_FLOOR)
}

/// 幅度 -> dB（下限 −240 dB）。
pub fn amplitude_to_db(amp: f64) -> f64 {
    20.0 * amp.max(MAGNITUDE_FLOOR).log10()
}

// ---------------------------------------------------------------------------
// 目标审计与复目标 trace（严格按 §6.6 相位语义）
// ---------------------------------------------------------------------------

/// 目标审计结果（§3.6 判据 1：目标有效；§6.10 族相位接受性）。
#[derive(Clone, Debug, PartialEq)]
pub struct TargetAudit {
    /// 报告用 phase_target 类别（§6.6）。
    pub phase_class: PhaseTargetClass,
    /// 目标是否有显式相位（magnitude-only 降级时为 false）。
    pub phase_present: bool,
    /// 是否显式允许的幅度-only 降级（§3.6 判据 6）。
    pub magnitude_only: bool,
    /// 目标 phase_ref 语义（§6.6）。
    pub phase_ref: PhaseRef,
    /// delay_removed 的 D_target（§6.6）。
    pub target_delay_samples: Option<u32>,
    /// 目标 runtime_rate（Hz）。
    pub target_rate_hz: f64,
    /// 目标规范点序列 sha256（§6.3；用于 stale/版本链）。
    pub target_sha256: String,
    /// 目标网格最小频点（Hz；§6.7 覆盖报告）。
    pub target_min_freq_hz: f64,
    /// 目标网格最大频点（Hz）。
    pub target_max_freq_hz: f64,
    /// 警告（不拒收）。
    pub warnings: Vec<String>,
}

/// 目标校验：族相位接受性（§6.10）、D 一致性（§3.6 判据 1）、目标预算（§6.9 码 28）、
/// magnitude-only 降级（§3.6 判据 6）。T11 的 E_TGT_* 码原样透传。
pub fn validate_target(
    v: &ValidatedCorrectedSpec,
    target: &TargetCurve,
) -> Result<TargetAudit, CorrectedError> {
    let target_rate = target.fs();
    if (target_rate - v.spec.fs).abs() >= 0.5 {
        return Err(err(
            "E_TGT_RATE",
            format!(
                "目标 runtime_rate={} 与设计 fs={} 不一致（§6.3）",
                target_rate, v.spec.fs
            ),
        ));
    }
    let mut warnings: Vec<String> = Vec::new();
    let (phase_class, magnitude_only) = match target
        .phase_target_for_family(v.spec.target_family(), false)
    {
        Ok(class) => {
            if !target.has_phase() {
                // 正常不会发生：requires_phase 族无 phase 会走 Err 分支。
                return Err(err(
                    E_FIR_FAMILY,
                    "corrected/lowdelay 必须有显式相位目标（§3.6 判据 1）".to_string(),
                ));
            }
            (class, false)
        }
        Err(error) => {
            let allow_degrade = error.code == TargetErrorCode::PhaseUnsupported
                && v.spec.allow_magnitude_only
                && !target.has_phase();
            if allow_degrade {
                warnings.push(
                        "目标无相位（phase_ref=none）：allow_magnitude_only=true，按显式线性相位 D 做幅度设计；报告 magnitude_only=true，且不等价于实测复矩阵（§3.6 判据 6）"
                            .to_string(),
                    );
                (PhaseTargetClass::None, true)
            } else {
                return Err(error.into());
            }
        }
    };

    let phase_ref = target.meta().phase_ref;
    let target_delay_samples = target.meta().target_delay_samples;
    if phase_ref == PhaseRef::DelayRemoved {
        // §3.6 判据 1：delay_removed 时 target_delay_samples 必须等于 D（T11 helper 容差 0.01 sample）。
        target.check_delay_consistency(v.spec.delay_samples as f64)?;
    }
    let delay_budget = if v.spec.mode == CorrectedMode::LowDelay {
        v.spec.delay_max_samples.unwrap_or(v.spec.delay_samples) as f64
    } else {
        v.spec.delay_samples as f64
    };
    let budget = TargetBudget {
        max_points: None,
        delay_budget_samples: Some(delay_budget),
        max_taps: Some(v.spec.taps),
    };
    target.check_budget(&budget)?;

    let report = target.report();
    if report.min_freq_hz > v.band_hz[0] || report.max_freq_hz < v.band_hz[1] {
        warnings.push(format!(
            "声明频带 [{}, {}] Hz 超出目标网格 [{}, {}] Hz：带外按 edge_policy 取值，覆盖来源见 T11 导入报告（§6.7）",
            v.band_hz[0], v.band_hz[1], report.min_freq_hz, report.max_freq_hz
        ));
    }
    if magnitude_only {
        warnings.push(format!(
            "magnitude_only=true：{}",
            NOT_EQUIVALENT_MAGNITUDE_ONLY
        ));
    }
    Ok(TargetAudit {
        phase_class,
        phase_present: !magnitude_only && target.has_phase(),
        magnitude_only,
        phase_ref,
        target_delay_samples,
        target_rate_hz: target_rate,
        target_sha256: target.target_sha256().to_string(),
        target_min_freq_hz: report.min_freq_hz,
        target_max_freq_hz: report.max_freq_hz,
        warnings,
    })
}

/// 应用的 D：delay_removed 必须等于 target_delay_samples（§3.6 判据 1），其余取声明 D。
pub fn resolve_applied_delay(
    v: &ValidatedCorrectedSpec,
    audit: &TargetAudit,
) -> Result<u32, CorrectedError> {
    if audit.phase_ref == PhaseRef::DelayRemoved {
        let d_target = audit.target_delay_samples.unwrap_or(0);
        if v.spec.delay_samples != d_target {
            return Err(err(
                "E_TGT_DELAY",
                format!(
                    "delay_removed 目标要求 D_used=target_delay_samples={}，实际声明 D={}（§3.6 判据 1：|D_used−D_target|<=0.01 sample 且两套规则不得混写）",
                    d_target, v.spec.delay_samples
                ),
            ));
        }
        Ok(d_target)
    } else {
        Ok(v.spec.delay_samples)
    }
}

/// 复目标 trace：给定 D 下按 §6.6 生成幅度与总相位（absolute 用给定总相位；
/// delay_removed 用 φ_ex − 360·f·D/fs；magnitude-only 显式线性相位 −360·f·D/fs）。
#[derive(Clone, Copy, Debug)]
pub struct TargetTrace<'a> {
    /// T11 目标曲线（只读）。
    pub target: &'a TargetCurve,
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 本 trace 使用的 D（samples）。
    pub d_samples: u32,
    /// 幅度-only 降级（线性相位）
    pub magnitude_only: bool,
    /// 目标 phase_ref 语义。
    pub phase_ref: PhaseRef,
}

impl<'a> TargetTrace<'a> {
    /// 构造 trace。
    pub fn new(
        v: &ValidatedCorrectedSpec,
        target: &'a TargetCurve,
        audit: &TargetAudit,
        d_samples: u32,
    ) -> Self {
        Self {
            target,
            fs: v.spec.fs,
            d_samples,
            magnitude_only: audit.magnitude_only,
            phase_ref: audit.phase_ref,
        }
    }

    /// 目标幅度（归一化 + reference_gain 之后的最终值，§6.5）。
    pub fn mag_db_at(&self, freq_hz: f64) -> Result<f64, CorrectedError> {
        Ok(self.target.mag_db_at(freq_hz)?)
    }

    /// 目标总相位（§6.6；无相位且非降级时为 None）。
    pub fn phase_total_deg_at(&self, freq_hz: f64) -> Result<Option<f64>, CorrectedError> {
        if self.magnitude_only {
            return Ok(Some(-360.0 * freq_hz * self.d_samples as f64 / self.fs));
        }
        match self.phase_ref {
            PhaseRef::Absolute => Ok(self.target.phase_total_deg_at(freq_hz)?),
            PhaseRef::DelayRemoved => {
                let phase_ex = self.target.phase_ex_deg_at(freq_hz)?;
                Ok(phase_ex.map(|p| p - 360.0 * freq_hz * self.d_samples as f64 / self.fs))
            }
            PhaseRef::None => Ok(None),
        }
    }

    /// 目标复值 T(f) = 10^(mag/20)·exp(j·φ_tot)。
    pub fn complex_at(&self, freq_hz: f64) -> Result<(f64, f64), CorrectedError> {
        let mag_db = self.mag_db_at(freq_hz)?;
        let phase_deg = self
            .phase_total_deg_at(freq_hz)?
            .ok_or_else(|| err(E_FIR_FAMILY, "复目标缺少相位（不应发生）"))?;
        let amp = db_to_amplitude(mag_db);
        let (sin_p, cos_p) = phase_deg.to_radians().sin_cos();
        Ok((amp * cos_p, amp * sin_p))
    }
}

/// 目标群延迟的带内上下界（samples；§1.2 数值口径；因果判据用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetGdBounds {
    /// 带内最小 GD（samples）。
    pub min_samples: f64,
    /// 带内最大 GD（samples）。
    pub max_samples: f64,
    /// 最小 GD 所在频点（Hz）。
    pub min_at_hz: f64,
    /// 网格点数。
    pub points: usize,
}

/// 对目标总相位数值求带内 GD 上下界（中心差分，步长 <= max(0.1 Hz, f*1e-3)，§1.2）。
pub fn target_group_delay_bounds(
    v: &ValidatedCorrectedSpec,
    trace: &TargetTrace<'_>,
) -> Result<TargetGdBounds, CorrectedError> {
    if !trace.phase_total_deg_at(v.band_hz[0])?.is_some() {
        return Err(err(
            E_FIR_FAMILY,
            "目标无相位，无法计算目标 GD（corrected/lowdelay 需要显式相位）".to_string(),
        ));
    }
    // 用相邻带内网格点的相位差估算段 GD（中心差分在带边会落到目标网格外被 hold，不准；
    // 段差分只用带内点，纯延迟目标给出精确 D）。§1.2 允许数值 GD，步长由网格决定。
    let points = 2_048usize;
    let grid = log_frequency_grid(v.band_hz[0], v.band_hz[1], points);
    let mut min_samples = f64::INFINITY;
    let mut max_samples = f64::NEG_INFINITY;
    let mut min_at_hz = v.band_hz[0];
    let mut used = 0usize;
    let mut previous: Option<(f64, f64)> = None;
    for f in grid {
        let Some(phase) = trace.phase_total_deg_at(f)? else {
            previous = None;
            continue;
        };
        if let Some((prev_f, prev_phase)) = previous {
            let df = f - prev_f;
            if df > 0.0 && phase.is_finite() && prev_phase.is_finite() {
                let gd = -(trace.fs / 360.0) * (phase - prev_phase) / df;
                if gd.is_finite() {
                    used += 1;
                    if gd < min_samples {
                        min_samples = gd;
                        min_at_hz = 0.5 * (prev_f + f);
                    }
                    if gd > max_samples {
                        max_samples = gd;
                    }
                }
            }
        }
        previous = Some((f, phase));
    }
    if used == 0 {
        return Err(err(
            E_FIR_FAMILY,
            "目标 GD 数值计算失败（相位在带内不可采样）".to_string(),
        ));
    }
    Ok(TargetGdBounds {
        min_samples,
        max_samples,
        min_at_hz,
        points: used,
    })
}

// ---------------------------------------------------------------------------
// 频域最小二乘（复目标 -> 实系数因果 FIR）
// ---------------------------------------------------------------------------

/// LS 求解报告（可复算）。
#[derive(Clone, Debug, PartialEq)]
pub struct LsReport {
    /// FFT 网格尺寸（2 的幂）。
    pub n_fft: usize,
    /// 带内对称频点数（正/负频成对计入一次的“正频点数”）。
    pub bins_in_band: usize,
    /// 实际使用 ridge。
    pub ridge_used: f64,
    /// CG 迭代次数。
    pub cg_iterations: usize,
    /// 对 (T+ridge·I)h=b 的相对残差。
    pub cg_relative_residual: f64,
    /// 相对残差是否 <= 1e-8（失败不得静默当 PASS）。
    pub converged: bool,
}

/// 选择 LS FFT 尺寸：override 或 16·taps 夹到 [4096,65536]；必须 2 的幂且 >= 2·taps。
fn choose_ls_fft_size(
    v: &ValidatedCorrectedSpec,
    override_size: Option<usize>,
) -> Result<usize, CorrectedError> {
    let taps = v.spec.taps;
    let min_required = (2 * taps).max(2);
    let mut n_fft = match override_size {
        Some(n) => n,
        None => {
            let mut n = taps.saturating_mul(LS_FFT_PER_TAP);
            n = n.max(MIN_LS_FFT).min(MAX_LS_FFT);
            n
        }
    };
    if override_size.is_none() {
        n_fft = next_pow2_at_least(n_fft);
        if n_fft > MAX_LS_FFT {
            n_fft = MAX_LS_FFT;
        }
    }
    if !is_pow2(n_fft) {
        return Err(err(
            E_FIR_BAND,
            format!("ls_fft_size={n_fft} 必须是 2 的幂"),
        ));
    }
    if n_fft < min_required {
        return Err(err(
            E_FIR_BAND,
            format!("ls_fft_size={n_fft} 必须 >= 2·taps={min_required}（circulant 嵌入）"),
        ));
    }
    if n_fft > MAX_LS_FFT {
        return Err(err(
            E_FIR_BUDGET,
            format!("ls_fft_size={n_fft} > {MAX_LS_FFT}（INFEASIBLE：网格超出离线工具上界）"),
        ));
    }
    Ok(n_fft)
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// 共轭梯度：解 (T + ridge·I)x = b（T 由 matvec 给出，x0=0）。
fn conjugate_gradient<F>(
    matvec: &F,
    b: &[f64],
    ridge: f64,
    max_iters: usize,
    tol: f64,
) -> (Vec<f64>, usize, f64)
where
    F: Fn(&[f64]) -> Vec<f64>,
{
    let n = b.len();
    let mut x = vec![0.0_f64; n];
    let mut r = b.to_vec();
    let mut p = r.clone();
    let mut rs = dot(&r, &r);
    let bnorm = rs.sqrt().max(f64::MIN_POSITIVE);
    let mut iterations = 0usize;
    while iterations < max_iters {
        let rnorm = rs.sqrt();
        if !rnorm.is_finite() || rnorm <= tol * bnorm {
            break;
        }
        let mut ap = matvec(&p);
        if ap.len() != n {
            break;
        }
        for i in 0..n {
            ap[i] += ridge * p[i];
        }
        let pap = dot(&p, &ap);
        if !(pap > 0.0) || !pap.is_finite() {
            break;
        }
        let alpha = rs / pap;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        let rs_new = dot(&r, &r);
        if !rs_new.is_finite() {
            break;
        }
        let beta = rs_new / rs;
        for i in 0..n {
            p[i] = r[i] + beta * p[i];
        }
        rs = rs_new;
        iterations += 1;
    }
    let rel = if rs.is_finite() {
        rs.sqrt() / bnorm
    } else {
        f64::INFINITY
    };
    (x, iterations, rel)
}

/// 频域加权 LS：在 [band_lo,band_hi] 的对数对称 FFT 频点上逼近复目标，得到实系数因果 h。
pub fn solve_ls(
    v: &ValidatedCorrectedSpec,
    trace: &TargetTrace<'_>,
    ls_fft_size: Option<usize>,
) -> Result<(Vec<f64>, LsReport), CorrectedError> {
    let taps = v.spec.taps;
    let n_fft = choose_ls_fft_size(v, ls_fft_size)?;
    let fft = Fft::new(n_fft)?;
    let fs = v.spec.fs;
    let lo = v.band_hz[0];
    let hi = v.band_hz[1];
    let mut weights = vec![0.0_f64; n_fft];
    let mut pos_bins: Vec<usize> = Vec::new();
    for k in 1..n_fft / 2 {
        let f = k as f64 * fs / n_fft as f64;
        if f >= lo && f <= hi {
            pos_bins.push(k);
            weights[k] = 1.0;
            weights[n_fft - k] = 1.0;
        }
    }
    if pos_bins.len() < MIN_LS_BINS_PER_TAP * taps {
        return Err(err(
            E_FIR_BUDGET,
            format!(
                "INFEASIBLE: 带内 FFT 频点 {} 个 < 2·taps={}（band=[{},{}] Hz, fs={} Hz, n_fft={}）；网格分辨率不足，不静默降级",
                pos_bins.len(),
                2 * taps,
                lo,
                hi,
                fs,
                n_fft
            ),
        ));
    }
    let mut ure = vec![0.0_f64; n_fft];
    let mut uim = vec![0.0_f64; n_fft];
    for &k in &pos_bins {
        let f = k as f64 * fs / n_fft as f64;
        let (re, im) = trace.complex_at(f)?;
        if !re.is_finite() || !im.is_finite() {
            return Err(err(
                E_FIR_NUMERIC,
                format!("目标在 {f} Hz 处非有限（{re} + {im}j）"),
            ));
        }
        ure[k] = re;
        uim[k] = im;
        let mirror = n_fft - k;
        ure[mirror] = re;
        uim[mirror] = -im;
    }
    fft.inverse(&mut ure, &mut uim);
    let b: Vec<f64> = ure[..taps].to_vec();
    if b.iter().any(|value| !value.is_finite()) {
        return Err(err(
            E_FIR_NUMERIC,
            "LS 右端 b 含非有限值（目标幅度/相位异常）".to_string(),
        ));
    }
    let bnorm = dot(&b, &b).sqrt();
    if !(bnorm > 0.0) || !bnorm.is_finite() {
        return Err(err(
            E_FIR_NUMERIC,
            "LS 右端 b 全零/非有限（目标幅度异常）".to_string(),
        ));
    }
    let matvec = |x: &[f64]| -> Vec<f64> {
        let mut re = vec![0.0_f64; n_fft];
        let mut im = vec![0.0_f64; n_fft];
        re[..taps].copy_from_slice(x);
        fft.forward(&mut re, &mut im);
        for k in 0..n_fft {
            re[k] *= weights[k];
            im[k] *= weights[k];
        }
        fft.inverse(&mut re, &mut im);
        re[..taps].to_vec()
    };
    let (coeffs, cg_iterations, _cg_rel) = conjugate_gradient(
        &matvec,
        &b,
        v.spec.ridge,
        CG_MAX_ITERATIONS,
        CG_RELATIVE_TOLERANCE,
    );
    for &h in &coeffs {
        if !h.is_finite() {
            return Err(err(
                E_FIR_NUMERIC,
                "LS 解含 NaN/Inf（ridge 或网格数值问题）".to_string(),
            ));
        }
    }
    // 以真实残差报告（不复用 CG 内部递推值）。
    let mut residual = b.clone();
    let ah = matvec(&coeffs);
    for i in 0..taps {
        residual[i] -= ah[i] + v.spec.ridge * coeffs[i];
    }
    let rel = dot(&residual, &residual).sqrt() / bnorm;
    let converged = rel.is_finite() && rel <= 1e-8;
    Ok((
        coeffs,
        LsReport {
            n_fft,
            bins_in_band: pos_bins.len(),
            ridge_used: v.spec.ridge,
            cg_iterations,
            cg_relative_residual: rel,
            converged,
        },
    ))
}

// ---------------------------------------------------------------------------
// 测量网格与报告结构
// ---------------------------------------------------------------------------

/// 归一化参考频点：1 kHz 在带内则取 1 kHz，否则取带几何中心（§1.5/§2.2）。
pub fn reference_hz(v: &ValidatedCorrectedSpec) -> f64 {
    if v.band_hz[0] <= 1_000.0 && 1_000.0 <= v.band_hz[1] {
        1_000.0
    } else {
        (v.band_hz[0] * v.band_hz[1]).sqrt()
    }
}

/// 测量网格上界：min(20 kHz, 0.499·fs)（§1.2/§2.4）。
pub fn measurement_hi_hz(fs: f64) -> f64 {
    BAND_HI_HZ.min(NYQUIST_GUARD_FRACTION * fs).max(BAND_LO_HZ)
}

/// 规范测量网格：20 Hz–min(20 kHz,0.499·fs) 对数网格 N 点 + 带边/参考锚点（排序去重）。
pub fn measurement_grid(v: &ValidatedCorrectedSpec, points: usize) -> Vec<f64> {
    let hi = measurement_hi_hz(v.spec.fs);
    let n = points.max(2);
    let mut grid = log_frequency_grid(BAND_LO_HZ, hi, n);
    for anchor in [v.band_hz[0], v.band_hz[1], reference_hz(v), 1_000.0] {
        if anchor.is_finite() && anchor >= BAND_LO_HZ && anchor <= hi {
            grid.push(anchor);
        }
    }
    grid.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    grid.dedup_by(|a, b| (*a - *b).abs() <= 1e-9 * b.abs().max(1.0));
    grid
}

/// 频带幅度统计（dB）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandStats {
    /// 频带下界（Hz）。
    pub lo_hz: f64,
    /// 频带上界（Hz）。
    pub hi_hz: f64,
    /// 最小幅度（dB）。
    pub min_db: f64,
    /// 最大幅度（dB）。
    pub max_db: f64,
    /// 参与统计的网格点数。
    pub points: usize,
}

impl BandStats {
    /// max−min（dB）。
    pub fn ripple_db(&self) -> f64 {
        self.max_db - self.min_db
    }
}

/// 相位误差统计（deg；wrap 到 (−180,180] 后取绝对值）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhaseStats {
    /// 最大 |Δφ|（deg）。
    pub max_abs_error_deg: f64,
    /// 平均 |Δφ|（deg）。
    pub mean_abs_error_deg: f64,
    /// 参与统计的网格点数。
    pub points: usize,
}

/// 1/3 oct 群延迟表条目（§3.6 判据 4 要求报 1/3 oct 表）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupDelayPoint {
    /// 频点（Hz；1/3 oct 标准中心频率）。
    pub freq_hz: f64,
    /// 该频点 GD（samples；解析 Re(G/H)）。
    pub gd_samples: f64,
}

/// 群延迟统计（samples；§3.6 判据 4）。
#[derive(Clone, Debug, PartialEq)]
pub struct GroupDelayStats {
    /// 带内最小 GD（samples）。
    pub min_samples: f64,
    /// 带内最大 GD（samples）。
    pub max_samples: f64,
    /// 带内均值（samples）。
    pub mean_samples: f64,
    /// 1 kHz 处 GD（samples；§2.2 字段）。
    pub at_1khz_samples: f64,
    /// 实测事实类别（带内起伏 <= 0.01 sample 记 Constant；纯延迟目标可达）。
    pub class: GroupDelayClass,
    /// §3.6 判据 4 的族声明类别（corrected/lowdelay 不假设常数 GD，声明 frequency_varying）。
    pub declared_class: GroupDelayClass,
    /// 1/3 oct 表（带内标准中心频率，实测；不足 3 点时补参考频点）。
    pub table_1_3_oct: Vec<GroupDelayPoint>,
}

/// 振铃统计（§1.4；corrected 只报告，不设上限）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingingStats {
    /// 排除窗 W=ceil(0.000333·fs)（samples）。
    pub exclusion_samples: usize,
    /// 主峰位置。
    pub peak_index: usize,
    /// 主峰 |h|。
    pub peak_abs: f64,
    /// 前振铃相对峰值（dB；支撑过短为 None=n/a）。
    pub pre_db: Option<f64>,
    /// 后振铃相对峰值（dB）。
    pub post_db: Option<f64>,
    /// 前振铃能量比（dB）。
    pub pre_energy_db: Option<f64>,
    /// 后振铃能量比（dB）。
    pub post_energy_db: Option<f64>,
}

/// 端点（§2.4 Endpoint 联合类型）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Endpoint {
    /// 已测量。
    Measured {
        /// 频点（Hz）。
        freq_hz: f64,
        /// 幅度（dB）。
        mag_db: f64,
        /// 相位（deg，atan2 分支）。
        phase_deg: f64,
    },
    /// 该 fs 下不可测。
    NotApplicableAtFs {
        /// 频点（Hz）。
        freq_hz: f64,
        /// 原因。
        reason: &'static str,
    },
}

impl Endpoint {
    /// 是否 measured。
    pub fn is_measured(&self) -> bool {
        matches!(self, Endpoint::Measured { .. })
    }

    /// status 字面值（§2.4）。
    pub fn status_str(&self) -> &'static str {
        match self {
            Endpoint::Measured { .. } => "measured",
            Endpoint::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }
}

/// 20 Hz / 20 kHz 端点报告（§2.4）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Endpoints {
    /// 20 Hz。
    pub f20: Endpoint,
    /// 20 kHz。
    pub f20k: Endpoint,
}

/// 参考归一化结果（§1.5/§2.2 规则 4）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（Hz）。
    pub hz: f64,
    /// 目标幅度（dB）。
    pub target_gain_db: f64,
    /// 实测幅度（dB）。
    pub measured_gain_db: f64,
    /// 写进 manifest 的声明增益（本设计取实测值，绝不偷偷加整体增益；§1.5）。
    pub declared_gain_db: f64,
    /// |declared−measured| <= 0.01 dB。
    pub tolerance_ok: bool,
}

/// 带外行为报告（§3.6 判据 7；不要求满足目标，但必须报告）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutOfBandStats {
    /// 观测下界（Hz）。
    pub lo_hz: f64,
    /// 观测上界（Hz）。
    pub hi_hz: f64,
    /// 最小幅度（dB；points=0 时为 NaN）。
    pub min_db: f64,
    /// 最大幅度（dB）。
    pub max_db: f64,
    /// 观测点数。
    pub points: usize,
}

/// 逐条判据结果（§3.6 判据 1–8）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorrectedChecks {
    /// 系数有限且 max|h|>0（§2.2 规则 3）。
    pub finite: bool,
    /// taps = len(h)（§2.2 规则 1）。
    pub taps: bool,
    /// D 整数 >=0 且 delay_ms 一致（§2.2 规则 2）。
    pub delay: bool,
    /// 目标有效且相位要求满足（§3.6 判据 1 / §6.10）。
    pub phase_target_ok: bool,
    /// 目标在声明 D 下因果（§3.6 判据 3）。
    pub causal_target: bool,
    /// delay_removed 时 D_used == D_target（§3.6 判据 1）。
    pub delay_consistency: bool,
    /// max 幅度误差 <= 门（§3.6 判据 2）。
    pub magnitude_error: bool,
    /// max 相位误差 <= 门（§3.6 判据 2；magnitude-only 不适用=true）。
    pub phase_error: bool,
    /// RMS 复误差 <= 门（§3.6 判据 2 / A1）。
    pub rms_error: bool,
    /// lowdelay D <= delay_max_samples（§3.6 判据 5）。
    pub lowdelay_budget: bool,
    /// 不冒充 linear/min 等价（§3.6 判据 6 / §3.8）。
    pub non_equivalence: bool,
    /// LS 求解收敛（A3；不收敛不得当 PASS）。
    pub ls_converged: bool,
}

impl CorrectedChecks {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.finite
            && self.taps
            && self.delay
            && self.phase_target_ok
            && self.causal_target
            && self.delay_consistency
            && self.magnitude_error
            && self.phase_error
            && self.rms_error
            && self.lowdelay_budget
            && self.non_equivalence
            && self.ls_converged
    }
}

/// 独立响应验证报告（T55 报告器的输入；不含 WAV/manifest 文件）。
#[derive(Clone, Debug, PartialEq)]
pub struct CorrectedVerification {
    /// 采样率。
    pub fs: f64,
    /// taps。
    pub taps: usize,
    /// 声明的 D（samples）。
    pub delay_samples: u32,
    /// D/fs·1000。
    pub delay_ms: f64,
    /// 测量网格点数（§1.2 的 N）。
    pub grid_points: usize,
    /// 带内幅度统计（非误差；见 max_mag_error_db）。
    pub band: BandStats,
    /// 归一化 RMS 复误差（dB；magnitude-only 时为幅度 dB RMS，A1）。
    pub rms_error_db: f64,
    /// RMS 门。
    pub rms_error_limit_db: f64,
    /// 带内最大幅度误差（dB）。
    pub max_mag_error_db: f64,
    /// 幅度门。
    pub max_mag_error_limit_db: f64,
    /// 带内最大相位误差（deg；magnitude-only 或目标无相位为 None）。
    pub max_phase_error_deg: Option<f64>,
    /// 相位门。
    pub max_phase_error_limit_deg: f64,
    /// 相位误差统计（无相位为 None）。
    pub phase: Option<PhaseStats>,
    /// 参考归一化。
    pub reference: ReferenceStats,
    /// 群延迟统计。
    pub group_delay: GroupDelayStats,
    /// 振铃。
    pub ringing: RingingStats,
    /// 20/20k 端点。
    pub endpoints: Endpoints,
    /// 带外行为。
    pub out_of_band: OutOfBandStats,
    /// 目标 GD 上下界（无相位为 None）。
    pub target_gd: Option<TargetGdBounds>,
    /// 逐条判据。
    pub checks: CorrectedChecks,
    /// 未通过判据的人类可读原因（空=全过）。
    pub failures: Vec<String>,
    /// 幅度-only 降级标志（§3.6 判据 6）。
    pub magnitude_only: bool,
}

impl CorrectedVerification {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }
}

/// 低延迟误差-延迟折中条目（§3.6 判据 5）。
#[derive(Clone, Debug, PartialEq)]
pub struct DelayTradeoffEntry {
    /// 候选 D（samples）。
    pub d_samples: u32,
    /// 是否为最终应用的 D。
    pub applied: bool,
    /// 是否在 delay_max 内（诊断条目可能越界，但本实现候选均 <= dmax）。
    pub within_delay_max: bool,
    /// 该 D 的带内最大幅度误差（dB）。
    pub max_mag_error_db: f64,
    /// 该 D 的带内最大相位误差（deg；magnitude-only 为 None）。
    pub max_phase_error_deg: Option<f64>,
    /// 该 D 的归一化 RMS 复误差（dB）。
    pub rms_error_db: f64,
    /// 是否满足全部误差门。
    pub meets_limits: bool,
}

/// 预算事实（§7.2；准入判定归 T56，本模块只报事实）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BudgetFacts {
    /// taps。
    pub taps: usize,
    /// regular|extended|offline_only（按 taps 上限分类）。
    pub profile: &'static str,
    /// 常规 profile 上限。
    pub regular_cap: usize,
    /// extended profile 上限。
    pub extended_cap: usize,
    /// f32 系数内存（bytes/路由）。
    pub memory_bytes_f32: usize,
    /// 声明 D。
    pub delay_samples: u32,
    /// D/fs·1000。
    pub delay_ms: f64,
    /// 是否在常规 profile 上限内。
    pub fits_regular: bool,
    /// 是否在 extended profile 上限内。
    pub fits_extended: bool,
    /// 是否已判定 15/25/40 ms 整链预算（本模块 = false，归 T56）。
    pub latency_budget_evaluated: bool,
}

/// 等价性诊断报告（§3.6 判据 6/§3.8；**不作等价声明**）。
#[derive(Clone, Debug, PartialEq)]
pub struct EquivalenceReport {
    /// 声明值（唯一合法 none）。
    pub claim: EquivalenceClaim,
    /// 事实诊断：h 是否线性相位（对称 + 带内常数 GD）。
    pub linear_phase_equivalent: bool,
    /// 对称性误差 max|h−flip(h)|。
    pub symmetry_max_abs_error: f64,
    /// 带内 GD 起伏（samples）。
    pub gd_spread_samples: f64,
    /// 事实诊断：与最小相位的 Hilbert 一致性（deg；仅诊断，不是等价声明）。
    pub minimum_phase_consistency_deg: Option<f64>,
    /// **恒 false**：正确实现从不声明最小相位等价于原矩阵。
    pub minimum_phase_equivalent: bool,
    /// **恒 false**：显式幅相目标不等于实测复矩阵。
    pub measured_complex_matrix_equivalent: bool,
    /// 非等价声明（§3.6 判据 6 必填文字）。
    pub note: String,
}

/// corrected/lowdelay 设计产物（tspi-fir/v1 的系数/报告源；WAV/manifest 归 T55）。
#[derive(Clone, Debug, PartialEq)]
pub struct CorrectedDesign {
    /// 原参数。
    pub spec: CorrectedSpec,
    /// f64 冲激响应（len=taps，因果）。
    pub coeffs: Vec<f64>,
    /// 家族 id（corrected|lowdelay）。
    pub family: &'static str,
    /// 目标规范点哈希（§6.3，stale/版本链）。
    pub target_sha256: String,
    /// phase_target 类别（§6.6）。
    pub target_phase_class: PhaseTargetClass,
    /// 幅度-only 降级标志（§3.6 判据 6）。
    pub magnitude_only: bool,
    /// 非等价声明文字。
    pub not_equivalent_statement: &'static str,
    /// 实际应用 D（=声明 D，或 delay_removed 的 D_target）。
    pub applied_delay_samples: u32,
    /// 独立验证报告。
    pub verification: CorrectedVerification,
    /// lowdelay 误差-延迟折中扫描（corrected 为单条 applied 条目）。
    pub delay_scan: Vec<DelayTradeoffEntry>,
    /// LS 求解报告。
    pub ls: LsReport,
    /// 等价性诊断。
    pub equivalence: EquivalenceReport,
    /// 目标审计。
    pub target_audit: TargetAudit,
    /// 预算事实。
    pub budget: BudgetFacts,
    /// max|h|。
    pub peak_abs: f64,
    /// Σh（DC 直流和，事实字段）。
    pub sum_coeffs: f64,
    /// 有效支撑 (first,last,effective_taps)（§1.3）。
    pub effective_support: (usize, usize, usize),
    /// onset latency = first/fs·1000（ms；§1.3 与 D 分开报告）。
    pub onset_ms: f64,
    /// 警告（不拒收）。
    pub warnings: Vec<String>,
}

impl CorrectedDesign {
    /// taps = len(h)。
    pub fn taps(&self) -> usize {
        self.coeffs.len()
    }

    /// 全部判据通过（含 lowdelay 应用条目满足门）。
    pub fn accepted(&self) -> bool {
        self.verification.passed()
            && self
                .delay_scan
                .iter()
                .find(|entry| entry.applied)
                .map(|entry| entry.meets_limits)
                .unwrap_or(false)
    }

    /// f32 导出（WAV/运行器用；量化误差复算归 T55/T22）。
    pub fn coeffs_f32(&self) -> Vec<f32> {
        self.coeffs.iter().map(|&h| h as f32).collect()
    }

    /// 延迟（ms，6 位小数格式由上层负责）。
    pub fn delay_ms(&self) -> f64 {
        self.applied_delay_samples as f64 / self.spec.fs * 1000.0
    }

    /// 人类可读摘要（证据日志用；键=值 单行）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "family={} fs={} taps={} D={} delay_ms={:.6} target_sha256={}",
            self.family,
            self.spec.fs,
            self.taps(),
            self.applied_delay_samples,
            self.delay_ms(),
            &self.target_sha256.chars().take(16).collect::<String>()
        ));
        lines.push(format!(
            "band=[{},{}] grid_points={} magnitude_only={} phase_class={}",
            self.verification.band.lo_hz,
            self.verification.band.hi_hz,
            self.verification.grid_points,
            self.magnitude_only,
            self.target_phase_class.as_str()
        ));
        lines.push(format!(
            "max_mag_error={:.6} dB (limit {:.6}) phase_error={} (limit {:.6}) rms_error={:.6} dB (limit {:.6})",
            self.verification.max_mag_error_db,
            self.verification.max_mag_error_limit_db,
            match self.verification.max_phase_error_deg {
                Some(value) => format!("{value:.6} deg"),
                None => "n/a".to_string(),
            },
            self.verification.max_phase_error_limit_deg,
            self.verification.rms_error_db,
            self.verification.rms_error_limit_db
        ));
        lines.push(format!(
            "reference hz={} target={:.6} dB measured={:.6} dB declared={:.6} dB tolerance_ok={}",
            self.verification.reference.hz,
            self.verification.reference.target_gain_db,
            self.verification.reference.measured_gain_db,
            self.verification.reference.declared_gain_db,
            self.verification.reference.tolerance_ok
        ));
        lines.push(format!(
            "group_delay min={:.6} max={:.6} mean={:.6} at_1khz={:.6} class={} declared_class={}",
            self.verification.group_delay.min_samples,
            self.verification.group_delay.max_samples,
            self.verification.group_delay.mean_samples,
            self.verification.group_delay.at_1khz_samples,
            self.verification.group_delay.class.as_str(),
            self.verification.group_delay.declared_class.as_str()
        ));
        for point in &self.verification.group_delay.table_1_3_oct {
            lines.push(format!(
                "gd_1_3oct hz={:.1} gd_samples={:.6}",
                point.freq_hz, point.gd_samples
            ));
        }
        lines.push(format!(
            "ringing exclusion={} peak_index={} pre={} post={} pre_energy={} post_energy={}",
            self.verification.ringing.exclusion_samples,
            self.verification.ringing.peak_index,
            format_opt_db(self.verification.ringing.pre_db),
            format_opt_db(self.verification.ringing.post_db),
            format_opt_db(self.verification.ringing.pre_energy_db),
            format_opt_db(self.verification.ringing.post_energy_db)
        ));
        lines.push(format!(
            "endpoints f20={} f20k={}",
            endpoint_summary(&self.verification.endpoints.f20),
            endpoint_summary(&self.verification.endpoints.f20k)
        ));
        lines.push(format!(
            "out_of_band lo={} hi={} min={:.6} dB max={:.6} dB points={}",
            self.verification.out_of_band.lo_hz,
            self.verification.out_of_band.hi_hz,
            self.verification.out_of_band.min_db,
            self.verification.out_of_band.max_db,
            self.verification.out_of_band.points
        ));
        if let Some(gd) = self.verification.target_gd {
            lines.push(format!(
                "target_gd min={:.6} at {:.3} Hz max={:.6} points={}",
                gd.min_samples, gd.min_at_hz, gd.max_samples, gd.points
            ));
        }
        lines.push(format!(
            "ls n_fft={} bins_in_band={} ridge={:.1e} cg_iterations={} cg_relative_residual={:.3e} converged={}",
            self.ls.n_fft,
            self.ls.bins_in_band,
            self.ls.ridge_used,
            self.ls.cg_iterations,
            self.ls.cg_relative_residual,
            self.ls.converged
        ));
        lines.push(format!(
            "checks passed={} {}",
            self.verification.passed(),
            checks_summary(&self.verification.checks)
        ));
        lines.push(format!(
            "equivalence claim={} linear_phase={} min_phase_consistency={} min_phase_equivalent=false measured_matrix_equivalent=false",
            self.equivalence.claim.as_str(),
            self.equivalence.linear_phase_equivalent,
            format_opt_deg(self.equivalence.minimum_phase_consistency_deg)
        ));
        lines.push(format!("not_equivalent={}", self.not_equivalent_statement));
        lines.push(format!(
            "budget profile={} memory_bytes_f32={} fits_regular={} fits_extended={} latency_budget_evaluated={}",
            self.budget.profile,
            self.budget.memory_bytes_f32,
            self.budget.fits_regular,
            self.budget.fits_extended,
            self.budget.latency_budget_evaluated
        ));
        lines.push(format!(
            "support first={} last={} effective_taps={} onset_ms={:.6} peak_abs={:.6} sum={:.6}",
            self.effective_support.0,
            self.effective_support.1,
            self.effective_support.2,
            self.onset_ms,
            self.peak_abs,
            self.sum_coeffs
        ));
        for entry in &self.delay_scan {
            lines.push(format!(
                "delay_scan D={} applied={} within_delay_max={} max_mag_error={:.6} phase_error={} rms_error={:.6} meets_limits={}",
                entry.d_samples,
                entry.applied,
                entry.within_delay_max,
                entry.max_mag_error_db,
                match entry.max_phase_error_deg {
                    Some(value) => format!("{value:.6} deg"),
                    None => "n/a".to_string(),
                },
                entry.rms_error_db,
                entry.meets_limits
            ));
        }
        for failure in &self.verification.failures {
            lines.push(format!("failure {failure}"));
        }
        lines.push(format!("warnings {}", self.warnings.len()));
        for warning in &self.warnings {
            lines.push(format!("warning {warning}"));
        }
        lines
    }
}

fn format_opt_db(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.6} dB"),
        None => "n/a".to_string(),
    }
}

fn format_opt_deg(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.6} deg"),
        None => "n/a".to_string(),
    }
}

fn endpoint_summary(endpoint: &Endpoint) -> String {
    match endpoint {
        Endpoint::Measured {
            freq_hz,
            mag_db,
            phase_deg,
        } => format!("measured({freq_hz} Hz, {mag_db:.6} dB, {phase_deg:.6} deg)"),
        Endpoint::NotApplicableAtFs { freq_hz, reason } => {
            format!("not_applicable_at_fs({freq_hz} Hz: {reason})")
        }
    }
}

fn checks_summary(checks: &CorrectedChecks) -> String {
    format!(
        "finite={} taps={} delay={} phase_target_ok={} causal_target={} delay_consistency={} magnitude_error={} phase_error={} rms_error={} lowdelay_budget={} non_equivalence={} ls_converged={}",
        checks.finite,
        checks.taps,
        checks.delay,
        checks.phase_target_ok,
        checks.causal_target,
        checks.delay_consistency,
        checks.magnitude_error,
        checks.phase_error,
        checks.rms_error,
        checks.lowdelay_budget,
        checks.non_equivalence,
        checks.ls_converged
    )
}

// ---------------------------------------------------------------------------
// 独立响应验证
// ---------------------------------------------------------------------------

fn response_and_gd(coeffs: &[f64], fs: f64, f_hz: f64) -> (FreqResponse, f64) {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return (
            FreqResponse {
                re: f64::NAN,
                im: f64::NAN,
            },
            f64::NAN,
        );
    }
    let (re, im, gd) = eval_h_and_gd(coeffs, fs, f_hz);
    (FreqResponse { re, im }, gd)
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    f_hz: f64,
    resp: FreqResponse,
    gd_samples: f64,
    target_mag_db: f64,
    target_phase_deg: Option<f64>,
}

fn in_band(v: &ValidatedCorrectedSpec, f_hz: f64) -> bool {
    let eps = 1e-9 * v.spec.fs.max(1.0);
    f_hz >= v.band_hz[0] - eps && f_hz <= v.band_hz[1] + eps
}

fn collect_samples(
    v: &ValidatedCorrectedSpec,
    trace: &TargetTrace<'_>,
    coeffs: &[f64],
    analysis: &AnalysisOptions,
) -> Result<Vec<Sample>, CorrectedError> {
    let grid = measurement_grid(v, analysis.mag_points);
    let mut samples = Vec::with_capacity(grid.len());
    for f_hz in grid {
        let (resp, gd_samples) = response_and_gd(coeffs, v.spec.fs, f_hz);
        let target_mag_db = trace.mag_db_at(f_hz)?;
        let target_phase_deg = trace.phase_total_deg_at(f_hz)?;
        samples.push(Sample {
            f_hz,
            resp,
            gd_samples,
            target_mag_db,
            target_phase_deg,
        });
    }
    Ok(samples)
}

fn band_metrics(
    v: &ValidatedCorrectedSpec,
    audit: &TargetAudit,
    samples: &[Sample],
) -> Result<(f64, Option<f64>, f64), CorrectedError> {
    let band: Vec<&Sample> = samples
        .iter()
        .filter(|sample| in_band(v, sample.f_hz))
        .collect();
    if band.is_empty() {
        return Err(err(
            E_FIR_NUMERIC,
            "带内测量样本为空（测量网格异常）".to_string(),
        ));
    }
    let mut max_mag = 0.0_f64;
    for sample in &band {
        let err_db = (sample.resp.magnitude_db() - sample.target_mag_db).abs();
        if !err_db.is_finite() {
            return Err(err(E_FIR_NUMERIC, format!("{} Hz 幅度非有限", sample.f_hz)));
        }
        max_mag = max_mag.max(err_db);
    }
    let phase = if audit.phase_present {
        let mut max_phase = 0.0_f64;
        let mut count = 0usize;
        let mut sum_abs = 0.0_f64;
        for sample in &band {
            let Some(target_phase) = sample.target_phase_deg else {
                continue;
            };
            let diff = wrap_deg(sample.resp.phase_deg() - target_phase);
            if !diff.is_finite() {
                return Err(err(E_FIR_NUMERIC, format!("{} Hz 相位非有限", sample.f_hz)));
            }
            max_phase = max_phase.max(diff.abs());
            sum_abs += diff.abs();
            count += 1;
        }
        if count == 0 {
            return Err(err(
                E_FIR_NUMERIC,
                "带内相位目标样本为空（§3.6 判据 2）".to_string(),
            ));
        }
        Some((max_phase, sum_abs / count as f64))
    } else {
        None
    };
    let rms_db = if audit.phase_present {
        let mut num = 0.0_f64;
        let mut den = 0.0_f64;
        for sample in &band {
            let target_phase = sample.target_phase_deg.unwrap_or(0.0);
            let (sin_p, cos_p) = target_phase.to_radians().sin_cos();
            let amp = db_to_amplitude(sample.target_mag_db);
            let tre = amp * cos_p;
            let tim = amp * sin_p;
            let dre = sample.resp.re - tre;
            let dim = sample.resp.im - tim;
            num += dre * dre + dim * dim;
            den += tre * tre + tim * tim;
        }
        let ratio = (num / den.max(f64::MIN_POSITIVE)).sqrt();
        20.0 * ratio.max(MAGNITUDE_FLOOR).log10()
    } else {
        let mut sum = 0.0_f64;
        for sample in &band {
            let err = sample.resp.magnitude_db() - sample.target_mag_db;
            sum += err * err;
        }
        (sum / band.len() as f64).sqrt()
    };
    if !rms_db.is_finite() {
        return Err(err(E_FIR_NUMERIC, "RMS 误差非有限".to_string()));
    }
    Ok((max_mag, phase.map(|(max, _mean)| max), rms_db))
}

fn band_phase_stats(audit: &TargetAudit, samples: &[Sample]) -> Option<PhaseStats> {
    if !audit.phase_present {
        return None;
    }
    let mut max_abs = 0.0_f64;
    let mut sum_abs = 0.0_f64;
    let mut count = 0usize;
    for sample in samples {
        let Some(target_phase) = sample.target_phase_deg else {
            continue;
        };
        let diff = wrap_deg(sample.resp.phase_deg() - target_phase).abs();
        if diff.is_finite() {
            max_abs = max_abs.max(diff);
            sum_abs += diff;
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some(PhaseStats {
            max_abs_error_deg: max_abs,
            mean_abs_error_deg: sum_abs / count as f64,
            points: count,
        })
    }
}

/// 振铃统计（§1.4；零填充线性卷积系数上测）。
pub fn ringing_stats(coeffs: &[f64], fs: f64) -> Result<RingingStats, CorrectedError> {
    let mut peak_abs = 0.0_f64;
    let mut peak_index = 0usize;
    for (index, &h) in coeffs.iter().enumerate() {
        if h.abs() > peak_abs {
            peak_abs = h.abs();
            peak_index = index;
        }
    }
    if !peak_abs.is_finite() || peak_abs <= 0.0 {
        return Err(err(
            E_FIR_NUMERIC,
            "振铃统计失败：max|h| 非有限或为 0".to_string(),
        ));
    }
    let exclusion = (RINGING_WINDOW_SEC * fs).ceil() as usize;
    let total_energy: f64 = coeffs.iter().map(|h| h * h).sum();
    if total_energy <= 0.0 || !total_energy.is_finite() {
        return Err(err(E_FIR_NUMERIC, "总能量非有限/为 0".to_string()));
    }
    let mut pre_peak = 0.0_f64;
    let mut pre_energy = 0.0_f64;
    let mut pre_available = false;
    if peak_index >= exclusion + 1 {
        pre_available = true;
        for &h in &coeffs[..peak_index - exclusion] {
            pre_peak = pre_peak.max(h.abs());
            pre_energy += h * h;
        }
    }
    let mut post_peak = 0.0_f64;
    let mut post_energy = 0.0_f64;
    let mut post_available = false;
    if peak_index + exclusion + 1 < coeffs.len() {
        post_available = true;
        for &h in &coeffs[peak_index + exclusion + 1..] {
            post_peak = post_peak.max(h.abs());
            post_energy += h * h;
        }
    }
    Ok(RingingStats {
        exclusion_samples: exclusion,
        peak_index,
        peak_abs,
        pre_db: pre_available.then(|| amplitude_to_db(pre_peak / peak_abs)),
        post_db: post_available.then(|| amplitude_to_db(post_peak / peak_abs)),
        pre_energy_db: pre_available.then(|| 10.0 * (pre_energy / total_energy).max(1e-24).log10()),
        post_energy_db: post_available
            .then(|| 10.0 * (post_energy / total_energy).max(1e-24).log10()),
    })
}

fn endpoint_at(coeffs: &[f64], fs: f64, freq_hz: f64) -> Endpoint {
    if freq_hz > 0.0 && freq_hz < 0.5 * fs {
        let resp = response_at(coeffs, fs, freq_hz);
        Endpoint::Measured {
            freq_hz,
            mag_db: resp.magnitude_db(),
            phase_deg: resp.phase_deg(),
        }
    } else {
        Endpoint::NotApplicableAtFs {
            freq_hz,
            reason: "0.5·fs <= 20 kHz：该 fs 下不可测（§2.4）",
        }
    }
}

fn symmetry_max_abs_error(coeffs: &[f64]) -> f64 {
    if coeffs.is_empty() {
        return f64::NAN;
    }
    let l = coeffs.len();
    let mut max_err = 0.0_f64;
    for n in 0..l {
        let err = (coeffs[n] - coeffs[l - 1 - n]).abs();
        max_err = max_err.max(err);
    }
    max_err
}

fn verify_inner(
    v: &ValidatedCorrectedSpec,
    trace: &TargetTrace<'_>,
    audit: &TargetAudit,
    coeffs: &[f64],
    analysis: &AnalysisOptions,
    ls_converged: bool,
) -> Result<CorrectedVerification, CorrectedError> {
    let fs = v.spec.fs;
    let taps = v.spec.taps;
    if coeffs.len() != taps {
        return Err(err(
            E_FIR_TAPS,
            format!(
                "系数长度 {} != 声明 taps {taps}（§2.2 规则 1）",
                coeffs.len()
            ),
        ));
    }
    let mut peak_abs = 0.0_f64;
    for &h in coeffs {
        if !h.is_finite() {
            return Err(err(
                E_FIR_NUMERIC,
                "系数含 NaN/Inf（§2.2 规则 3）".to_string(),
            ));
        }
        peak_abs = peak_abs.max(h.abs());
    }
    if !(peak_abs > 0.0) || !peak_abs.is_finite() {
        return Err(err(E_FIR_NUMERIC, "max|h|=0 或非有限".to_string()));
    }

    let samples = collect_samples(v, trace, coeffs, analysis)?;
    let (max_mag, max_phase, rms_db) = band_metrics(v, audit, &samples)?;
    let band_values: Vec<&Sample> = samples
        .iter()
        .filter(|sample| in_band(v, sample.f_hz))
        .collect();
    let mut min_db = f64::INFINITY;
    let mut max_db = f64::NEG_INFINITY;
    for sample in &band_values {
        let db = sample.resp.magnitude_db();
        min_db = min_db.min(db);
        max_db = max_db.max(db);
    }
    let band = BandStats {
        lo_hz: v.band_hz[0],
        hi_hz: v.band_hz[1],
        min_db,
        max_db,
        points: band_values.len(),
    };

    let phase = band_phase_stats(audit, &samples);
    if audit.phase_present && phase.is_none() {
        return Err(err(
            E_FIR_NUMERIC,
            "相位误差统计为空（§3.6 判据 2）".to_string(),
        ));
    }

    let mut gd_min = f64::INFINITY;
    let mut gd_max = f64::NEG_INFINITY;
    let mut gd_sum = 0.0_f64;
    let mut gd_count = 0usize;
    for sample in &band_values {
        if sample.gd_samples.is_finite() {
            gd_min = gd_min.min(sample.gd_samples);
            gd_max = gd_max.max(sample.gd_samples);
            gd_sum += sample.gd_samples;
            gd_count += 1;
        }
    }
    if gd_count == 0 {
        return Err(err(E_FIR_NUMERIC, "带内 GD 全非有限".to_string()));
    }
    let gd_mean = gd_sum / gd_count as f64;
    let gd_spread = gd_max - gd_min;
    let at_1khz = group_delay_at(coeffs, fs, 1_000.0);
    let mut table_1_3_oct: Vec<GroupDelayPoint> = Vec::new();
    for f_hz in [
        31.5_f64, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
    ] {
        if f_hz < v.band_hz[0] || f_hz > v.band_hz[1] {
            continue;
        }
        let gd = group_delay_at(coeffs, fs, f_hz);
        if gd.is_finite() {
            table_1_3_oct.push(GroupDelayPoint {
                freq_hz: f_hz,
                gd_samples: gd,
            });
        }
    }
    if table_1_3_oct.is_empty() {
        let fallback_hz = reference_hz(v);
        table_1_3_oct.push(GroupDelayPoint {
            freq_hz: fallback_hz,
            gd_samples: group_delay_at(coeffs, fs, fallback_hz),
        });
    }
    let group_delay = GroupDelayStats {
        min_samples: gd_min,
        max_samples: gd_max,
        mean_samples: gd_mean,
        at_1khz_samples: at_1khz,
        class: if gd_spread <= 0.01 {
            GroupDelayClass::Constant
        } else {
            GroupDelayClass::FrequencyVarying
        },
        declared_class: GroupDelayClass::FrequencyVarying,
        table_1_3_oct,
    };

    let ringing = ringing_stats(coeffs, fs)?;
    let endpoints = Endpoints {
        f20: endpoint_at(coeffs, fs, BAND_LO_HZ),
        f20k: endpoint_at(coeffs, fs, BAND_HI_HZ),
    };
    let hi_lim = measurement_hi_hz(fs);
    let mut out_min = f64::INFINITY;
    let mut out_max = f64::NEG_INFINITY;
    let mut out_points = 0usize;
    for sample in &samples {
        if in_band(v, sample.f_hz) {
            continue;
        }
        let db = sample.resp.magnitude_db();
        out_min = out_min.min(db);
        out_max = out_max.max(db);
        out_points += 1;
    }

    let ref_hz = reference_hz(v);
    let ref_response = response_at(coeffs, fs, ref_hz);
    let measured_gain_db = ref_response.magnitude_db();
    let target_gain_db = trace.mag_db_at(ref_hz)?;
    let declared_gain_db = measured_gain_db;
    let reference = ReferenceStats {
        hz: ref_hz,
        target_gain_db,
        measured_gain_db,
        declared_gain_db,
        tolerance_ok: (declared_gain_db - measured_gain_db).abs() <= REFERENCE_TOL_DB,
    };

    let target_gd = if audit.phase_present {
        Some(target_group_delay_bounds(v, trace)?)
    } else {
        None
    };

    let delay_ok = (v.delay_ms - trace.d_samples as f64 / fs * 1000.0).abs() <= 1e-9;
    let target_phase_ok =
        audit.phase_present || (audit.magnitude_only && v.spec.allow_magnitude_only);
    let causal_target = target_gd
        .map(|gd| gd.min_samples >= -CAUSALITY_TOLERANCE_SAMPLES)
        .unwrap_or(true);
    let delay_consistency = if audit.phase_ref == PhaseRef::DelayRemoved {
        audit
            .target_delay_samples
            .map(|d| d == trace.d_samples)
            .unwrap_or(false)
    } else {
        true
    };
    let eps = 1e-9;
    let magnitude_error = max_mag <= v.spec.max_mag_error_db + eps;
    let phase_error = match max_phase {
        Some(value) => value <= v.spec.max_phase_error_deg + eps,
        None => true,
    };
    let rms_error = rms_db <= v.spec.rms_error_db + eps;
    let lowdelay_budget = v.spec.mode != CorrectedMode::LowDelay
        || v.spec
            .delay_max_samples
            .map(|d_max| trace.d_samples <= d_max)
            .unwrap_or(false);
    let non_equivalence = v.spec.equivalence_claim == EquivalenceClaim::None;
    let finite = peak_abs.is_finite() && peak_abs > 0.0;

    let checks = CorrectedChecks {
        finite,
        taps: coeffs.len() == taps,
        delay: delay_ok,
        phase_target_ok: target_phase_ok,
        causal_target,
        delay_consistency,
        magnitude_error,
        phase_error,
        rms_error,
        lowdelay_budget,
        non_equivalence,
        ls_converged,
    };
    let mut failures = Vec::new();
    if !checks.finite {
        failures.push("finite: max|h| 非有限或为 0".to_string());
    }
    if !checks.taps {
        failures.push("taps: 系数长度 != 声明 taps".to_string());
    }
    if !checks.delay {
        failures.push("delay: D/delay_ms 不一致".to_string());
    }
    if !checks.phase_target_ok {
        failures.push("phase_target: 目标是幅度-only 且未显式允许（§3.6 判据 1/6）".to_string());
    }
    if !checks.causal_target {
        if let Some(gd) = target_gd {
            failures.push(format!(
                "causal_target: 目标带内 min GD={:.6} samples @ {:.3} Hz < -{CAUSALITY_TOLERANCE_SAMPLES}（§3.6 判据 3）",
                gd.min_samples, gd.min_at_hz
            ));
        }
    }
    if !checks.delay_consistency {
        failures
            .push("delay_consistency: D_used != target_delay_samples（§3.6 判据 1）".to_string());
    }
    if !checks.magnitude_error {
        failures.push(format!(
            "magnitude_error: max={max_mag:.6} dB > {:.6} dB（§3.6 判据 2）",
            v.spec.max_mag_error_db
        ));
    }
    if !checks.phase_error {
        failures.push(format!(
            "phase_error: max={:.6} deg > {:.6} deg（§3.6 判据 2）",
            max_phase.unwrap_or(f64::NAN),
            v.spec.max_phase_error_deg
        ));
    }
    if !checks.rms_error {
        failures.push(format!(
            "rms_error: {rms_db:.6} dB > {:.6} dB（§3.6 判据 2 / A1）",
            v.spec.rms_error_db
        ));
    }
    if !checks.lowdelay_budget {
        failures.push("lowdelay_budget: D > delay_max_samples（§3.6 判据 5）".to_string());
    }
    if !checks.non_equivalence {
        failures.push("non_equivalence: 声明了 linear/min 等价（§3.6 判据 6/§3.8）".to_string());
    }
    if !checks.ls_converged {
        failures.push("ls_converged: LS CG 未收敛（A3；不得当 PASS）".to_string());
    }

    Ok(CorrectedVerification {
        fs,
        taps,
        delay_samples: trace.d_samples,
        delay_ms: trace.d_samples as f64 / fs * 1000.0,
        grid_points: samples.len(),
        band,
        rms_error_db: rms_db,
        rms_error_limit_db: v.spec.rms_error_db,
        max_mag_error_db: max_mag,
        max_mag_error_limit_db: v.spec.max_mag_error_db,
        max_phase_error_deg: max_phase,
        max_phase_error_limit_deg: v.spec.max_phase_error_deg,
        phase,
        reference,
        group_delay,
        ringing,
        endpoints,
        out_of_band: OutOfBandStats {
            lo_hz: BAND_LO_HZ,
            hi_hz: hi_lim,
            min_db: if out_points == 0 { f64::NAN } else { out_min },
            max_db: if out_points == 0 { f64::NAN } else { out_max },
            points: out_points,
        },
        target_gd,
        checks,
        failures,
        magnitude_only: audit.magnitude_only,
    })
}

/// 独立验证任意系数（不复用 LS 中间量；min-phase 替代反例测试用）。
pub fn verify(
    spec: &CorrectedSpec,
    target: &TargetCurve,
    coeffs: &[f64],
) -> Result<CorrectedVerification, CorrectedError> {
    verify_with(
        spec,
        target,
        coeffs,
        &AnalysisOptions::normative_for(spec.taps),
    )
}

/// 独立验证任意系数（自定义网格）。
pub fn verify_with(
    spec: &CorrectedSpec,
    target: &TargetCurve,
    coeffs: &[f64],
    analysis: &AnalysisOptions,
) -> Result<CorrectedVerification, CorrectedError> {
    let v = validate_spec(spec)?;
    let audit = validate_target(&v, target)?;
    let applied = resolve_applied_delay(&v, &audit)?;
    let trace = TargetTrace::new(&v, target, &audit, applied);
    verify_inner(&v, &trace, &audit, coeffs, analysis, true)
}

// ---------------------------------------------------------------------------
// 等价性诊断（不声明等价；最小相位只能作反例/诊断）
// ---------------------------------------------------------------------------

/// 最小相位一致性（§3.2 判据 2 的 Hilbert 一致性口径，诊断用）：
/// 对 h 补零到 n_fft，实测相位与由 log|H| 实倒谱得到的 min-phase 相位之差的最大 |wrap|（deg）。
pub fn min_phase_consistency_deg(
    coeffs: &[f64],
    fs: f64,
    n_fft: usize,
    lo_hz: f64,
    hi_hz: f64,
) -> Result<f64, CorrectedError> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err(err(E_FIR_BAND, format!("fs 非法：{fs}")));
    }
    if !is_pow2(n_fft) || n_fft < 1_024 {
        return Err(err(
            E_FIR_BAND,
            format!("n_fft={n_fft} 必须是 >=1024 的 2 的幂"),
        ));
    }
    if coeffs.is_empty() || coeffs.len() > n_fft {
        return Err(err(
            E_FIR_TAPS,
            format!("coeffs 长度 {} 必须在 1..={n_fft}", coeffs.len()),
        ));
    }
    if !(lo_hz.is_finite() && hi_hz.is_finite() && 0.0 < lo_hz && lo_hz < hi_hz) {
        return Err(err(E_FIR_BAND, format!("诊断频带非法：[{lo_hz}, {hi_hz}]")));
    }
    let fft = Fft::new(n_fft)?;
    let mut re = vec![0.0_f64; n_fft];
    let mut im = vec![0.0_f64; n_fft];
    re[..coeffs.len()].copy_from_slice(coeffs);
    fft.forward(&mut re, &mut im);
    let mut logmag = vec![0.0_f64; n_fft];
    for k in 0..n_fft {
        let amp = re[k].hypot(im[k]).max(1e-6); // −120 dB floor（§3.2 mag_floor）
        logmag[k] = amp.ln();
    }
    let mut cep_re = logmag;
    let mut cep_im = vec![0.0_f64; n_fft];
    fft.inverse(&mut cep_re, &mut cep_im);
    let mut folded = vec![0.0_f64; n_fft];
    folded[0] = cep_re[0];
    for k in 1..n_fft / 2 {
        folded[k] = 2.0 * cep_re[k];
    }
    folded[n_fft / 2] = cep_re[n_fft / 2];
    let mut logh_re = folded;
    let mut logh_im = vec![0.0_f64; n_fft];
    fft.forward(&mut logh_re, &mut logh_im);
    let k_lo = ((lo_hz / fs) * n_fft as f64).ceil().max(1.0) as usize;
    let k_hi = ((hi_hz / fs) * n_fft as f64)
        .floor()
        .min((n_fft / 2) as f64) as usize;
    if k_hi < k_lo {
        return Err(err(
            E_FIR_BAND,
            format!("诊断频带 [{lo_hz},{hi_hz}] 在 n_fft={n_fft} 下无频点"),
        ));
    }
    let mut max_diff = 0.0_f64;
    for k in k_lo..=k_hi {
        let meas = im[k].atan2(re[k]).to_degrees();
        let min_phase = logh_im[k].atan2(logh_re[k]).to_degrees();
        max_diff = max_diff.max(wrap_deg(meas - min_phase).abs());
    }
    Ok(max_diff)
}

fn interpolate_table_log_f(freqs_hz: &[f64], values: &[f64], f_hz: f64) -> f64 {
    if freqs_hz.is_empty() {
        return f64::NAN;
    }
    if f_hz <= freqs_hz[0] {
        return values[0];
    }
    if f_hz >= freqs_hz[freqs_hz.len() - 1] {
        return values[values.len() - 1];
    }
    let mut lo = 0usize;
    let mut hi = freqs_hz.len() - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if freqs_hz[mid] <= f_hz {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let f1 = freqs_hz[lo];
    let f2 = freqs_hz[hi];
    let t = (f_hz.ln() - f1.ln()) / (f2.ln() - f1.ln());
    values[lo] + t * (values[hi] - values[lo])
}

/// 由幅度表做同态最小相位 FIR（**诊断/oracle 专用，不是本家族设计输出**）。
///
/// 用途：测试中构造“最小相位替代”，证明它与显式复目标不等价（相位门失败）。
pub fn min_phase_from_magnitude_table(
    freqs_hz: &[f64],
    mag_db: &[f64],
    fs: f64,
    taps: usize,
    n_fft: usize,
) -> Result<Vec<f64>, CorrectedError> {
    if freqs_hz.len() < 2 || freqs_hz.len() != mag_db.len() {
        return Err(err(
            E_FIR_BAND,
            "幅度表必须 >=2 点且 freqs/mag 等长".to_string(),
        ));
    }
    for window in freqs_hz.windows(2) {
        if !(window[0] > 0.0 && window[1] > window[0]) {
            return Err(err(E_FIR_BAND, "幅度表频点必须正且严格递增".to_string()));
        }
    }
    if mag_db.iter().any(|value| !value.is_finite()) {
        return Err(err(E_FIR_BAND, "幅度表含 NaN/Inf".to_string()));
    }
    if !fs.is_finite() || fs <= 0.0 {
        return Err(err(E_FIR_BAND, format!("fs 非法：{fs}")));
    }
    if taps == 0 || taps > n_fft {
        return Err(err(E_FIR_TAPS, format!("taps={taps} 必须在 1..={n_fft}")));
    }
    if !is_pow2(n_fft) || n_fft < 1_024 {
        return Err(err(
            E_FIR_BAND,
            format!("n_fft={n_fft} 必须是 >=1024 的 2 的幂"),
        ));
    }
    let fft = Fft::new(n_fft)?;
    let mut logmag = vec![0.0_f64; n_fft];
    for k in 0..=n_fft / 2 {
        let f = k as f64 * fs / n_fft as f64;
        let db = interpolate_table_log_f(freqs_hz, mag_db, f).max(-120.0);
        logmag[k] = db / 20.0 * std::f64::consts::LN_10;
    }
    for k in n_fft / 2 + 1..n_fft {
        logmag[k] = logmag[n_fft - k];
    }
    let mut cep_re = logmag;
    let mut cep_im = vec![0.0_f64; n_fft];
    fft.inverse(&mut cep_re, &mut cep_im);
    let mut folded = vec![0.0_f64; n_fft];
    folded[0] = cep_re[0];
    for k in 1..n_fft / 2 {
        folded[k] = 2.0 * cep_re[k];
    }
    folded[n_fft / 2] = cep_re[n_fft / 2];
    let mut logh_re = folded;
    let mut logh_im = vec![0.0_f64; n_fft];
    fft.forward(&mut logh_re, &mut logh_im);
    let mut h_re = vec![0.0_f64; n_fft];
    let mut h_im = vec![0.0_f64; n_fft];
    for k in 0..n_fft {
        let amp = logh_re[k].exp();
        h_re[k] = amp * logh_im[k].cos();
        h_im[k] = amp * logh_im[k].sin();
    }
    fft.inverse(&mut h_re, &mut h_im);
    let coeffs: Vec<f64> = h_re[..taps].to_vec();
    if coeffs.iter().any(|value| !value.is_finite()) {
        return Err(err(E_FIR_NUMERIC, "最小相位诊断系数非有限".to_string()));
    }
    Ok(coeffs)
}

fn build_equivalence(
    v: &ValidatedCorrectedSpec,
    coeffs: &[f64],
    verification: &CorrectedVerification,
) -> Result<EquivalenceReport, CorrectedError> {
    let mut peak_abs = 0.0_f64;
    for &h in coeffs {
        peak_abs = peak_abs.max(h.abs());
    }
    let symmetry_err = symmetry_max_abs_error(coeffs);
    let gd_spread = verification.group_delay.max_samples - verification.group_delay.min_samples;
    let linear_phase_equivalent = symmetry_err.is_finite()
        && symmetry_err <= 1e-7 * peak_abs.max(f64::MIN_POSITIVE)
        && gd_spread <= 0.01;
    let n_fft =
        next_pow2_at_least((8 * coeffs.len()).max(MIN_PHASE_FFT_MIN)).min(MIN_PHASE_FFT_MAX);
    let hi = (0.45 * v.spec.fs).min(BAND_HI_HZ).max(BAND_LO_HZ + 1.0);
    let consistency = min_phase_consistency_deg(coeffs, v.spec.fs, n_fft, BAND_LO_HZ, hi)?;
    let note = if verification.magnitude_only {
        NOT_EQUIVALENT_MAGNITUDE_ONLY.to_string()
    } else {
        NOT_EQUIVALENT_EXPLICIT_COMPLEX.to_string()
    };
    Ok(EquivalenceReport {
        claim: v.spec.equivalence_claim,
        linear_phase_equivalent,
        symmetry_max_abs_error: symmetry_err,
        gd_spread_samples: gd_spread,
        minimum_phase_consistency_deg: Some(consistency),
        minimum_phase_equivalent: false,
        measured_complex_matrix_equivalent: false,
        note,
    })
}

// ---------------------------------------------------------------------------
// 预算事实
// ---------------------------------------------------------------------------

fn budget_facts_inner(v: &ValidatedCorrectedSpec) -> BudgetFacts {
    let taps = v.spec.taps;
    let (profile, fits_regular, fits_extended) = if taps <= REGULAR_PROFILE_TAPS_CAP {
        ("regular", true, true)
    } else if taps <= EXTENDED_PROFILE_TAPS_CAP {
        ("extended", false, true)
    } else {
        ("offline_only", false, false)
    };
    BudgetFacts {
        taps,
        profile,
        regular_cap: REGULAR_PROFILE_TAPS_CAP,
        extended_cap: EXTENDED_PROFILE_TAPS_CAP,
        memory_bytes_f32: 4 * taps,
        delay_samples: v.spec.delay_samples,
        delay_ms: v.delay_ms,
        fits_regular,
        fits_extended,
        latency_budget_evaluated: false,
    }
}

/// 预算事实（§7.2；准入判定归 T56）。
pub fn budget_facts(spec: &CorrectedSpec) -> Result<BudgetFacts, CorrectedError> {
    let v = validate_spec(spec)?;
    Ok(budget_facts_inner(&v))
}

// ---------------------------------------------------------------------------
// 误差-延迟折中扫描（§3.6 判据 5）
// ---------------------------------------------------------------------------

fn scan_depends_on_d(audit: &TargetAudit) -> bool {
    audit.magnitude_only || audit.phase_ref == PhaseRef::DelayRemoved
}

fn delay_scan_candidates(v: &ValidatedCorrectedSpec, applied: u32) -> Vec<u32> {
    let d_max = v.spec.delay_max_samples.unwrap_or(applied).max(applied);
    let mut candidates = vec![applied, 0, d_max / 4, d_max / 2, d_max];
    candidates.retain(|d| *d <= d_max);
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn tradeoff_entry_from_verification(
    d_samples: u32,
    applied: bool,
    within_delay_max: bool,
    verification: &CorrectedVerification,
) -> DelayTradeoffEntry {
    DelayTradeoffEntry {
        d_samples,
        applied,
        within_delay_max,
        max_mag_error_db: verification.max_mag_error_db,
        max_phase_error_deg: verification.max_phase_error_deg,
        rms_error_db: verification.rms_error_db,
        meets_limits: verification.passed(),
    }
}

fn build_delay_scan(
    v: &ValidatedCorrectedSpec,
    audit: &TargetAudit,
    target: &TargetCurve,
    verification: &CorrectedVerification,
    applied: u32,
    options: &DesignOptions,
) -> Result<Vec<DelayTradeoffEntry>, CorrectedError> {
    if v.spec.mode == CorrectedMode::Corrected {
        return Ok(vec![tradeoff_entry_from_verification(
            applied,
            true,
            true,
            verification,
        )]);
    }
    let d_max = v.spec.delay_max_samples.unwrap_or(applied);
    let candidates = delay_scan_candidates(v, applied);
    let depends = scan_depends_on_d(audit);
    let mut entries: Vec<DelayTradeoffEntry> = Vec::new();
    for d in candidates {
        let within = d <= d_max;
        if d == applied {
            entries.push(tradeoff_entry_from_verification(
                d,
                true,
                within,
                verification,
            ));
            continue;
        }
        if !depends {
            // absolute 相位：总线相位目标不随 D 改变，LS 解相同；D 只报告（§3.6 判据 1）。
            entries.push(tradeoff_entry_from_verification(
                d,
                false,
                within,
                verification,
            ));
            continue;
        }
        let trace = TargetTrace::new(v, target, audit, d);
        let gd = if audit.phase_present {
            Some(target_group_delay_bounds(v, &trace)?)
        } else {
            None
        };
        let causal = gd
            .map(|bounds| bounds.min_samples >= -CAUSALITY_TOLERANCE_SAMPLES)
            .unwrap_or(true);
        let (coeffs, ls) = solve_ls(v, &trace, options.ls_fft_size)?;
        let samples = collect_samples(v, &trace, &coeffs, &options.analysis)?;
        let (max_mag, max_phase, rms_db) = band_metrics(v, audit, &samples)?;
        let meets = ls.converged
            && causal
            && max_mag <= v.spec.max_mag_error_db + 1e-9
            && max_phase
                .map(|value| value <= v.spec.max_phase_error_deg + 1e-9)
                .unwrap_or(true)
            && rms_db <= v.spec.rms_error_db + 1e-9;
        entries.push(DelayTradeoffEntry {
            d_samples: d,
            applied: false,
            within_delay_max: within,
            max_mag_error_db: max_mag,
            max_phase_error_deg: max_phase,
            rms_error_db: rms_db,
            meets_limits: meets,
        });
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// 设计入口
// ---------------------------------------------------------------------------

/// 设计（规范测量网格）：corrected/lowdelay。
pub fn design(
    spec: &CorrectedSpec,
    target: &TargetCurve,
) -> Result<CorrectedDesign, CorrectedError> {
    design_with(spec, target, &DesignOptions::normative(spec.taps))
}

/// 设计（自定义验证网格/LS FFT）。
pub fn design_with(
    spec: &CorrectedSpec,
    target: &TargetCurve,
    options: &DesignOptions,
) -> Result<CorrectedDesign, CorrectedError> {
    let v = validate_spec(spec)?;
    let audit = validate_target(&v, target)?;
    let applied = resolve_applied_delay(&v, &audit)?;
    let trace = TargetTrace::new(&v, target, &audit, applied);

    // §3.6 判据 3：目标在声明 D 下必须因果（min GD >= -0.01 sample），否则 E_FIR_NONCAUSAL。
    if audit.phase_present {
        let gd = target_group_delay_bounds(&v, &trace)?;
        if !gd.min_samples.is_finite() || gd.min_samples < -CAUSALITY_TOLERANCE_SAMPLES {
            return Err(err(
                E_FIR_NONCAUSAL,
                format!(
                    "目标在声明 D={} samples 下不可因果：带内 min GD={:.6} samples @ {:.3} Hz < -{}（§3.6 判据 3；不猜、不截、不 silently shift D）",
                    applied, gd.min_samples, gd.min_at_hz, CAUSALITY_TOLERANCE_SAMPLES
                ),
            ));
        }
    }

    let (coeffs, ls) = solve_ls(&v, &trace, options.ls_fft_size)?;
    let verification = verify_inner(&v, &trace, &audit, &coeffs, &options.analysis, ls.converged)?;
    let delay_scan = build_delay_scan(&v, &audit, target, &verification, applied, options)?;
    let equivalence = build_equivalence(&v, &coeffs, &verification)?;
    let budget = budget_facts_inner(&v);

    let mut peak_abs = 0.0_f64;
    for &h in &coeffs {
        peak_abs = peak_abs.max(h.abs());
    }
    let sum_coeffs: f64 = coeffs.iter().sum();
    let (first, last, _effective) = effective_support(&coeffs, EFFECTIVE_SUPPORT_EPS);

    let mut warnings = v.warnings.clone();
    warnings.extend(audit.warnings.clone());
    if v.spec.mode == CorrectedMode::LowDelay && !scan_depends_on_d(&audit) {
        warnings.push(
            "lowdelay + phase_ref=absolute：总相位目标不随 D 变化（D 仅报告，§3.6 判据 1），误差-延迟扫描条目为同一设计的重复度量"
                .to_string(),
        );
    }
    if !ls.converged {
        warnings.push(format!(
            "LS CG 未收敛：相对残差 {:.3e} > 1e-8（A3：accepted=false，不静默当 PASS）",
            ls.cg_relative_residual
        ));
    }
    if (v.spec.ridge - DEFAULT_RIDGE).abs() > 0.0 {
        warnings.push(format!(
            "使用非默认 ridge={}（默认 {DEFAULT_RIDGE}）；设计对目标幅度可能有微小有偏",
            v.spec.ridge
        ));
    }
    if let Some(gd) = verification.target_gd {
        if gd.min_samples < 0.0 {
            warnings.push(format!(
                "目标带内 min GD={:.6} samples < 0 但在因果容差 {CAUSALITY_TOLERANCE_SAMPLES} 内；报告如实保留",
                gd.min_samples
            ));
        }
    }

    let not_equivalent_statement = if audit.magnitude_only {
        NOT_EQUIVALENT_MAGNITUDE_ONLY
    } else {
        NOT_EQUIVALENT_EXPLICIT_COMPLEX
    };
    Ok(CorrectedDesign {
        spec: v.spec.clone(),
        coeffs,
        family: v.spec.family_id(),
        target_sha256: audit.target_sha256.clone(),
        target_phase_class: audit.phase_class,
        magnitude_only: audit.magnitude_only,
        not_equivalent_statement,
        applied_delay_samples: applied,
        verification,
        delay_scan,
        ls,
        equivalence,
        target_audit: audit,
        budget,
        peak_abs,
        sum_coeffs,
        effective_support: (first, last, _effective),
        onset_ms: first as f64 / v.spec.fs * 1000.0,
        warnings,
    })
}

/// 严格设计：未达 §3.6 判据即 E_FIR_BUDGET/INFEASIBLE（报告 best achieved，不静默放行）。
pub fn design_strict(
    spec: &CorrectedSpec,
    target: &TargetCurve,
) -> Result<CorrectedDesign, CorrectedError> {
    design_strict_with(spec, target, &DesignOptions::normative(spec.taps))
}

/// 严格设计（自定义网格）。
pub fn design_strict_with(
    spec: &CorrectedSpec,
    target: &TargetCurve,
    options: &DesignOptions,
) -> Result<CorrectedDesign, CorrectedError> {
    let design = design_with(spec, target, options)?;
    if design.accepted() {
        return Ok(design);
    }
    let applied_entry = design
        .delay_scan
        .iter()
        .find(|entry| entry.applied)
        .cloned();
    let infeasible_reason = if spec.mode == CorrectedMode::LowDelay {
        "INFEASIBLE: lowdelay 在 D<=delay_max_samples 内未达 §3.6 判据 5 的误差门（报告 best achieved，不静默放行）"
    } else {
        "INFEASIBLE: corrected 未达 §3.6 判据 2 的误差门（报告 best achieved，不静默放行）"
    };
    Err(err(
        E_FIR_BUDGET,
        format!(
            "{infeasible_reason}；failures=[{}]；applied_delay_scan={}",
            design.verification.failures.join("; "),
            match applied_entry {
                Some(entry) => format!(
                    "D={} max_mag_error={:.6} dB phase_error={} rms_error={:.6} dB meets_limits={}",
                    entry.d_samples,
                    entry.max_mag_error_db,
                    match entry.max_phase_error_deg {
                        Some(value) => format!("{value:.6} deg"),
                        None => "n/a".to_string(),
                    },
                    entry.rms_error_db,
                    entry.meets_limits
                ),
                None => "none".to_string(),
            }
        ),
    ))
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T37/corrected_cases.rs 通过 include! 编进 lib 单测；
// 不依赖 integrator 独占的 public_registration.rs，见 tests/T37/README.md）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod corrected_tests {
    use super::*;
    include!("../tests/T37/corrected_cases.rs");
}
