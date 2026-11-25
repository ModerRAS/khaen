//! [NONRT] NOS（直通/旁路）族设计与旁路计划（WBS T38）—— T38 写域。
//!
//! 本模块**只**实现 NOS 家族：每路输出目标 FIR 的 delta（h=[1.0]，L=1，D=0）+ **显式旁路**声明，
//! 以及两条不可让步的边界：
//!
//! 1. **后置输出保护不被旁路**：limiter/clip/ramp 仍在图的其他节点执行（§5.1 规范链序
//!    …→ 输出目标 FIR → gain → delay → polarity → 保护/limiter → scatter）；NOS 只旁路每路
//!    **输出目标 FIR** 这一级，不允许借 bypass 绕过保护（DEC §3「NOS 不是绕保护」；
//!    compat-matrix P03/P06：限幅不得被 bypass/identity 回退绕过）。
//! 2. **无重建 DAC 复刻承诺**：NOS 只表示本软件旁路，**不改变/不模拟 MOTU DAC 内部重建滤波**；
//!    本模块不实现、不复刻任何专有 DAC 的重建滤波器（WBS T38 行；spec §0.2 非目标）。
//!
//! 权威契约：
//! - rt-stack/spec/fir-toolbox.md v1.2-draft §3.7（nos 参数/判据/拒收）、§3.8（nos 恒等检查）、
//!   §2.2（khaen-fir/v1 manifest 字段）、§2.4（20 Hz/20 kHz Endpoint 联合类型）、§2.5（E_FIR_* 失败码，
//!   含 E_FIR_NOS）、§1.2（响应测量程序）、§1.3（支撑/延迟）、§1.4（振铃排除窗口径）、
//!   §1.5（参考归一化）、§5.1（P_post 规范链序）、§6.10（nos 目标可选、仅记录不参与设计）、
//!   §7.2（运行准入 4096/8192/16384）、§9 V18（h=[1.0]、L=1、D=0、|H|=1、phase=0）；
//! - scratch/lead/planner/DSP-HOST-DECISION.md §3：NOS 工具箱独立可验、不复刻专有 DAC、NOS 不是绕保护；
//! - scratch/lead/planner/KHAEN-RT-STACK-WBS.md T38 行：NOS=delta/明确旁路，仍保后置输出保护，
//!   记录无重建 DAC 复刻承诺；
//! - rt-stack/spec/compat-matrix.md P03/P06/P07：限幅不得被 bypass/identity 回退绕过、回退保最终保护。
//!
//! 能力：
//! - 参数：bypass=true、taps=1；构造器 NosSpec::delta(fs) / NosSpec::reference_preset_48k()；
//!   任何 taps != 1、任何非显式 bypass、任何频带/窗参数 → E_FIR_NOS（§3.7 拒收）；
//!   任何 declared_delay_samples != 0 → E_FIR_DELAY（§2.2 规则 nos=0；整数/分数 delay 归 T40，不在此冒充）。
//! - 构造：h=[1.0]（n=0 的 delta）、D=0、delay_ms=0、reference DC 0 dB、response 全 0 dB/0°；
//!   同一输入同一版本两次 design 系数逐位一致（无随机数/时间戳）。
//! - 独立验证：直接 DTFT 复算任意频点（不复用「h=[1] 所以一定 1」的假定）、20 Hz/20 kHz Endpoint、
//!   振铃 n/a（L=1，§1.4 支撑过短）、参考归一化 ±0.01 dB、taps/预算分类。
//! - 旁路计划：plan_bypass[_with_chain] 返回 NosBypassPlan，显式记录「只旁路 OutputStage::TargetFir，
//!   OutputStage::Protection 不被旁路」；缺 limiter/clip/ramp 任一、目标 FIR/保护/scatter 顺序错、
//!   重复级 → E_FIR_NOS（bypass 不得绕保护）。
//! - 记录：DacReplicationRecord（claim=none + 无复刻声明）、NosManifestDeclaration
//!   （family=nos / filtering=none / protection_bypassed=false）、report_text()（含两条必须声明）。
//! - 可选目标：with_recorded_target 只记录（label/has_magnitude/has_phase），
//!   target_participated_in_design 恒 false；设计系数逐位不随 target 变化（§6.10）。
//!
//! 登记（契约文字差异，不私自改 schema）：
//! §3.7 要求 manifest 写 filtering: none，但 §2.2 的 khaen-fir/v1 规范字段表未列 filtering，
//! 且 §2.2 规则「未知字段：x_ 前缀允许，其它未知字段拒绝」。本模块提供 filtering 值与
//! FILTERING_FIELD_NOTE；T55/Lead 落盘时须裁决写 filtering（先补 §2.2 字段）还是 x_filtering，
//! 本模块不擅自扩张 schema。同理「无重建 DAC 复刻承诺」属**报告/design 段**声明，不新增顶层未知字段。
//!
//! 明确非目标（不得由本模块冒充）：
//! - 不写 WAV/manifest 文件（T55）、不做 target 文件导入（T11）、不做 integer/fractional delay 与
//!   polarity 设计（T40）、不实现 limiter/clip/ramp 本身（T21/T23 图节点）、不做 P/G 融合（T12）、
//!   不做 profile 资源准入（T56）；
//! - 不碰板端/ALSA/服务/RT（本文件零 RT 路径、零板端动作、零 git 操作）。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次 design 系数逐位一致（无随机数/时间戳）。
//!
//! 私有测试：tests/T38/nos_cases.rs 由本文件末尾 #[cfg(test)] mod nos_tests 通过 include!
//! 编译执行（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，不改 public_registration.rs）。

use std::f64::consts::PI;
use std::fmt;

// ---------------------------------------------------------------------------
// 常量（契约来源见各行注释）
// ---------------------------------------------------------------------------

/// 家族标识（§1.7 / §2.2 family 字段）。
pub const FAMILY: &str = "nos";

/// NOS 支撑长度：§3.7「taps 必须=1」。
pub const NOS_TAPS: usize = 1;

/// NOS 唯一非零系数（delta at n=0，§3.7/§9 V18 h=[1.0]）。
pub const NOS_COEFFICIENT: f64 = 1.0;

/// 体延迟 D（samples）：§2.2 规则 nos=0、§9 V18 D=0。
pub const NOS_DELAY_SAMPLES: f64 = 0.0;

/// 恒等容差：§3.7「|h[0]−1| <= 1e-12」。
pub const IDENTITY_TOLERANCE: f64 = 1e-12;

/// 参考归一化容差（§1.5：|measured_gain_db − gain_db| <= 0.01 dB）。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;

/// |H| 下限（§1.2：截到 1e-12 = −240 dB，避免 log0）。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;

/// 测量网格下界（Hz，§1.2）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;

/// 20 kHz 端点频点（Hz，§2.4）。
pub const ENDPOINT_HI_HZ: f64 = 20_000.0;

/// Nyquist 护栏比例 0.499·fs（§1.2 测量网格上界 / §2.4 端点可测判据）。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;

/// 常规 profile 每路由有效支撑（§7.2）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;

/// extended profile 每路由有效支撑（§7.2）。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;

/// 每个 f32 系数字节数（§7.2 内存估算 4·taps bytes/路由）。
pub const F32_BYTES: usize = 4;

/// 振铃排除窗比例（§1.4：W=ceil(0.000333·fs)，48k=16、96k=32）。
pub const RINGING_EXCLUSION_SECONDS: f64 = 0.000_333;

/// 设计模块工程采样率范围下限（Hz；典型 48k/96k，8k 为语音域下限）。
pub const FS_ENGINEERING_MIN_HZ: f64 = 8_000.0;

/// 设计模块工程采样率范围上限（Hz）。
pub const FS_ENGINEERING_MAX_HZ: f64 = 384_000.0;

/// manifest filtering 字段值（§3.7「manifest 写 filtering: none」）。
pub const FILTERING_NONE: &str = "none";

/// 无重建 DAC 复刻承诺：报告必须写出的原句（§3.7）。
pub const NO_DAC_REPLICATION_STATEMENT: &str = "本软件旁路，不改变/不模拟 MOTU DAC 内部重建滤波";

/// 无重建 DAC 复刻承诺的补充句（WBS T38「记录无重建 DAC 复刻承诺」）。
pub const NO_DAC_REPLICATION_COMMITMENT: &str =
    "不承诺复刻任何专有 DAC 的重建滤波器；NOS 只声明本软件旁路";

/// 后置输出保护不被旁路的声明（§3.7/§5.1；DEC §3「NOS 不是绕保护」）。
pub const PROTECTION_NOT_BYPASSED_STATEMENT: &str =
    "NOS 只旁路每路输出目标 FIR；后置输出保护（limiter/clip/ramp）仍在图的其他节点执行，不被 bypass 绕过";

/// DAC 复刻声明值（报告/design 段，非顶层 manifest 字段）。
pub const DAC_REPLICATION_NONE: &str = "none";

/// §2.2 与 §3.7 的字段差异登记（见模块头「登记」）。
pub const FILTERING_FIELD_NOTE: &str =
    "§3.7 要求 manifest 写 filtering=none，但 §2.2 规范字段表未列 filtering 且会拒收未知非 x_ 字段；T55/Lead 须裁决 filtering 或 x_filtering，本模块不擅自扩张 schema";

/// 旁路作用范围（显式，不允许隐含）。
pub const BYPASS_SCOPE: &str = "p_post.output_target_fir";

// ---------------------------------------------------------------------------
// Endpoint / 预算 / delta 旁路 / DAC 复刻记录
// ---------------------------------------------------------------------------

/// 端点报告联合类型（§2.4；禁止裸字符串/省略）。
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    /// 可测端点。
    Measured {
        /// 频点（Hz）。
        freq_hz: f64,
        /// |H|（dB）。
        mag_db: f64,
        /// 相位（度，wrap 到 (−180,180]）。
        phase_deg: f64,
    },
    /// 在运行 fs 下不可测（如 20 kHz > 0.499·fs）。
    NotApplicableAtFs {
        /// 原因。
        reason: String,
    },
}

impl Endpoint {
    /// status 字段字符串（measured / not_applicable_at_fs）。
    pub fn status(&self) -> &'static str {
        match self {
            Endpoint::Measured { .. } => "measured",
            Endpoint::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }

    /// 可测时返回 (freq_hz, mag_db, phase_deg)。
    pub fn measured(&self) -> Option<(f64, f64, f64)> {
        match self {
            Endpoint::Measured {
                freq_hz,
                mag_db,
                phase_deg,
            } => Some((*freq_hz, *mag_db, *phase_deg)),
            Endpoint::NotApplicableAtFs { .. } => None,
        }
    }
}

/// 20 Hz / 20 kHz 端点（§2.4；T55 response.endpoints 的字段来源）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosEndpoints {
    /// 20 Hz 端点。
    pub f20: Endpoint,
    /// 20 kHz 端点（fs 下不可测时为 not_applicable_at_fs）。
    pub f20k: Endpoint,
}

/// taps 运行准入分类（§7.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NosProfile {
    /// 常规 profile：每路由 <= 4096 taps。
    Regular,
}

impl NosProfile {
    /// schema 字符串。
    pub fn schema_name(self) -> &'static str {
        match self {
            NosProfile::Regular => "regular",
        }
    }
}

/// NOS 的 taps/内存预算（§7.2）：delta 恒为 1 tap = 4 bytes f32，永远进常规 profile。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NosBudget {
    /// profile 分类。
    pub profile: NosProfile,
    /// 该 profile 的每路由 taps 上限。
    pub route_taps_cap: usize,
    /// 每路由 f32 系数内存 4·taps bytes（§7.2）。
    pub ram_bytes_f32: usize,
    /// 是否可进常规 profile（<=4096）。
    pub fits_regular_profile: bool,
    /// 是否可进 extended profile（<=16384）。
    pub fits_extended_profile: bool,
}

impl NosBudget {
    /// delta 的预算（1 tap）：regular、4 bytes。
    pub fn identity() -> Self {
        NosBudget {
            profile: NosProfile::Regular,
            route_taps_cap: REGULAR_PROFILE_TAPS_CAP,
            ram_bytes_f32: F32_BYTES * NOS_TAPS,
            fits_regular_profile: true,
            fits_extended_profile: true,
        }
    }
}

/// 旁路替换物：显式 delta（**不是**任何 DAC 重建滤波或其它近似）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BypassReplacement {
    /// h=[1.0]：n=0 的 delta（直通）。
    Delta,
}

impl BypassReplacement {
    /// schema 字符串。
    pub fn schema_name(self) -> &'static str {
        match self {
            BypassReplacement::Delta => "delta",
        }
    }

    /// 人类可读说明。
    pub fn description(self) -> &'static str {
        match self {
            BypassReplacement::Delta => "h=[1.0]（delta at n=0，直通）",
        }
    }
}

/// DAC 复刻声明（WBS T38「记录无重建 DAC 复刻承诺」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DacReplicationClaim {
    /// 不复刻、不模拟任何专有 DAC 内部重建滤波。
    None,
}

impl DacReplicationClaim {
    /// schema 字符串。
    pub fn schema_name(self) -> &'static str {
        match self {
            DacReplicationClaim::None => DAC_REPLICATION_NONE,
        }
    }
}

/// 可落盘/可报告的无重建 DAC 复刻记录（claim + 原句）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DacReplicationRecord {
    /// 声明值：none。
    pub claim: DacReplicationClaim,
    /// 报告原句（§3.7 必须）。
    pub statement: &'static str,
    /// 补充承诺句（WBS T38 记录项）。
    pub commitment: &'static str,
}

impl DacReplicationRecord {
    /// 本软件旁路专用记录：claim=none + 两条声明原句。
    pub fn software_bypass_only() -> Self {
        DacReplicationRecord {
            claim: DacReplicationClaim::None,
            statement: NO_DAC_REPLICATION_STATEMENT,
            commitment: NO_DAC_REPLICATION_COMMITMENT,
        }
    }

    /// 是否声明为「无复刻」。
    pub fn is_none(&self) -> bool {
        self.claim == DacReplicationClaim::None
    }
}

// ---------------------------------------------------------------------------
// 输出链 / 后置输出保护 / 旁路计划（§5.1 规范链序）
// ---------------------------------------------------------------------------

/// 输出链级（只列与本任务相关的 §5.1 规范链序节点；顺序即规范序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputStage {
    /// 输入选择 A_map（仅选择/零填）。
    InputSelect,
    /// 低带矩阵 G 嵌入。
    LowBandMatrix,
    /// 分频（P_post 内）。
    Crossover,
    /// 模态 PEQ（P_post 内，可 IIR）。
    ModalPeq,
    /// 每路输出目标 FIR（P_post 内；**NOS 唯一允许旁路的级**）。
    TargetFir,
    /// 输出增益（P_post 内）。
    Gain,
    /// 输出延迟（P_post 内）。
    Delay,
    /// 输出极性（P_post 内）。
    Polarity,
    /// 保护/limiter（limiter/clip/ramp；**NOS 禁止旁路**）。
    Protection,
    /// scatter（输出行选择）。
    Scatter,
}

impl OutputStage {
    /// schema 字符串（报告/图计划用）。
    pub fn schema_name(self) -> &'static str {
        match self {
            OutputStage::InputSelect => "input_select",
            OutputStage::LowBandMatrix => "low_band_matrix",
            OutputStage::Crossover => "crossover",
            OutputStage::ModalPeq => "modal_peq",
            OutputStage::TargetFir => "output_target_fir",
            OutputStage::Gain => "gain",
            OutputStage::Delay => "delay",
            OutputStage::Polarity => "polarity",
            OutputStage::Protection => "protection",
            OutputStage::Scatter => "scatter",
        }
    }

    /// §5.1 规范链序：A_map → G → P_post(crossover → modal PEQ → 输出目标 FIR → gain → delay → polarity)
    /// → 保护/limiter → scatter。
    pub fn canonical_order() -> [OutputStage; 10] {
        [
            OutputStage::InputSelect,
            OutputStage::LowBandMatrix,
            OutputStage::Crossover,
            OutputStage::ModalPeq,
            OutputStage::TargetFir,
            OutputStage::Gain,
            OutputStage::Delay,
            OutputStage::Polarity,
            OutputStage::Protection,
            OutputStage::Scatter,
        ]
    }
}

/// §5.1 规范输出链（NOS 旁路计划的默认链条）。
pub fn canonical_output_chain() -> Vec<OutputStage> {
    OutputStage::canonical_order().to_vec()
}

/// 后置输出保护三件套（§3.7：limiter/clip/ramp 仍在图的其他节点执行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectionKind {
    /// 限幅器。
    Limiter,
    /// 削波检测/保护。
    Clip,
    /// 启动/切换 ramp（防爆音）。
    Ramp,
}

impl ProtectionKind {
    /// schema 字符串。
    pub fn schema_name(self) -> &'static str {
        match self {
            ProtectionKind::Limiter => "limiter",
            ProtectionKind::Clip => "clip",
            ProtectionKind::Ramp => "ramp",
        }
    }

    /// 全部保护类（顺序固定，便于报告/测试）。
    pub fn all() -> [ProtectionKind; 3] {
        [
            ProtectionKind::Limiter,
            ProtectionKind::Clip,
            ProtectionKind::Ramp,
        ]
    }
}

/// 后置输出保护启用状态（NOS 不得关闭其中任何一项）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostOutputProtection {
    /// limiter 是否启用。
    pub limiter: bool,
    /// clip 保护是否启用。
    pub clip: bool,
    /// ramp 是否启用。
    pub ramp: bool,
}

impl PostOutputProtection {
    /// 三项全启用（唯一允许与 NOS 共用的状态）。
    pub fn fully_enabled() -> Self {
        PostOutputProtection {
            limiter: true,
            clip: true,
            ramp: true,
        }
    }

    /// 是否三项全启用。
    pub fn is_fully_enabled(&self) -> bool {
        self.limiter && self.clip && self.ramp
    }

    /// 未启用的保护类（空 = 全启用）。
    pub fn disabled_kinds(&self) -> Vec<ProtectionKind> {
        let mut missing = Vec::new();
        if !self.limiter {
            missing.push(ProtectionKind::Limiter);
        }
        if !self.clip {
            missing.push(ProtectionKind::Clip);
        }
        if !self.ramp {
            missing.push(ProtectionKind::Ramp);
        }
        missing
    }
}

/// 旁路作用域（显式记录：只旁路哪一级、替换成什么、保护是否仍必须存在）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BypassScope {
    /// 被旁路的级（恒为 OutputStage::TargetFir）。
    pub stage: OutputStage,
    /// 替换物（delta）。
    pub replacement: BypassReplacement,
    /// 是否必须保留后置输出保护（恒 true）。
    pub protection_required: bool,
}

impl BypassScope {
    /// NOS 的唯一合法作用域。
    pub fn output_target_fir_only() -> Self {
        BypassScope {
            stage: OutputStage::TargetFir,
            replacement: BypassReplacement::Delta,
            protection_required: true,
        }
    }

    /// schema 字符串（= BYPASS_SCOPE）。
    pub fn schema_name(&self) -> &'static str {
        BYPASS_SCOPE
    }
}

/// NOS 旁路计划：显式声明「旁路目标 FIR、保留后置保护」。
#[derive(Clone, Debug, PartialEq)]
pub struct NosBypassPlan {
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 作用域（只旁路 TargetFir）。
    pub scope: BypassScope,
    /// 后置输出保护启用状态（必须三项全启用）。
    pub protection: PostOutputProtection,
    /// 实际输出链（规范序的子序列，含目标 FIR/保护/scatter）。
    pub chain: Vec<OutputStage>,
}

impl NosBypassPlan {
    /// 被旁路的级。
    pub fn bypassed_stage(&self) -> OutputStage {
        self.scope.stage
    }

    /// 该级是否被 NOS 旁路（仅目标 FIR 为 true；保护/增益/延迟/极性等一律 false）。
    pub fn bypasses(&self, stage: OutputStage) -> bool {
        stage == self.scope.stage
    }

    /// 该级是否为保护（且未被旁路）。
    pub fn protects(&self, stage: OutputStage) -> bool {
        stage == OutputStage::Protection && !self.bypasses(stage)
    }

    /// 链中某级的下标（重复/缺失时取第一个；计划构造已保证唯一）。
    pub fn stage_index(&self, stage: OutputStage) -> Option<usize> {
        self.chain.iter().position(|&s| s == stage)
    }

    /// 后置保护是否在目标 FIR 之后（规范序要求）。
    pub fn protection_after_bypass(&self) -> bool {
        match (
            self.stage_index(OutputStage::TargetFir),
            self.stage_index(OutputStage::Protection),
        ) {
            (Some(t), Some(p)) => p > t,
            _ => false,
        }
    }

    /// 保护是否仍完整执行（三项全启用 + 顺序正确）。
    pub fn preserves_protection(&self) -> bool {
        self.protection.is_fully_enabled() && self.protection_after_bypass()
    }

    /// 报告句：显式声明只旁路目标 FIR、保护仍在图内。
    pub fn report_text(&self) -> String {
        format!(
            "bypass_scope={} replacement={} protection_preserved={} protection_after_bypass={}",
            self.scope.schema_name(),
            self.scope.replacement.schema_name(),
            self.preserves_protection(),
            self.protection_after_bypass()
        )
    }
}

fn validate_fs(fs: f64) -> Result<(), NosError> {
    if !fs.is_finite() || fs < FS_ENGINEERING_MIN_HZ || fs > FS_ENGINEERING_MAX_HZ {
        return Err(NosError::Band(format!(
            "fs 必须有限且在工程范围 [{FS_ENGINEERING_MIN_HZ}, {FS_ENGINEERING_MAX_HZ}] Hz（典型 48k/96k），实际 {fs}"
        )));
    }
    Ok(())
}

/// 用规范输出链构造 NOS 旁路计划（最常见的显式旁路入口）。
pub fn plan_bypass(fs: f64, protection: PostOutputProtection) -> Result<NosBypassPlan, NosError> {
    plan_bypass_with_chain(fs, &canonical_output_chain(), protection)
}

/// 用给定输出链构造 NOS 旁路计划，并强制：
/// 1. limiter/clip/ramp 三项全启用（bypass 不绕保护）；
/// 2. 目标 FIR / 保护 / scatter 各出现一次，且顺序为 目标 FIR → 保护 → scatter。
///
/// 任一不满足 → E_FIR_NOS（这是 NOS 参数非恒等/绕保护，不是可降级警告）。
pub fn plan_bypass_with_chain(
    fs: f64,
    chain: &[OutputStage],
    protection: PostOutputProtection,
) -> Result<NosBypassPlan, NosError> {
    validate_fs(fs)?;

    let missing = protection.disabled_kinds();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|kind| kind.schema_name()).collect();
        return Err(NosError::Nos(format!(
            "后置输出保护不得被 NOS 旁路：缺 {}（§3.7 limiter/clip/ramp 仍在图的其他节点执行；DEC「NOS 不是绕保护」）",
            names.join("/")
        )));
    }

    let count = |stage: OutputStage| chain.iter().filter(|&&s| s == stage).count();
    for stage in [
        OutputStage::TargetFir,
        OutputStage::Protection,
        OutputStage::Scatter,
    ] {
        let n = count(stage);
        if n == 0 {
            return Err(NosError::Nos(format!(
                "输出链缺少必需级 {}（§5.1 规范链序 目标 FIR → 保护 → scatter）",
                stage.schema_name()
            )));
        }
        if n > 1 {
            return Err(NosError::Nos(format!(
                "输出链中 {} 重复出现 {n} 次（无法确定旁路/保护顺序）",
                stage.schema_name()
            )));
        }
    }

    let idx = |stage: OutputStage| {
        chain
            .iter()
            .position(|&s| s == stage)
            .expect("count 已保证存在")
    };
    let (t, p, s) = (
        idx(OutputStage::TargetFir),
        idx(OutputStage::Protection),
        idx(OutputStage::Scatter),
    );
    if !(t < p && p < s) {
        return Err(NosError::Nos(format!(
            "输出链顺序违反 §5.1 规范序：要求 目标 FIR(idx {t}) → 保护(idx {p}) → scatter(idx {s})；NOS 只旁路目标 FIR，不得把保护挪到其前或 scatter 之后"
        )));
    }

    Ok(NosBypassPlan {
        fs,
        scope: BypassScope::output_target_fir_only(),
        protection,
        chain: chain.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// 响应 / 振铃 / 有效支撑（§1.2–§1.5）
// ---------------------------------------------------------------------------

/// 单点复频响 H(f)=re+j·im。
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

    /// 20·log10(max(|H|, 1e-12))（§1.2 下限 −240 dB）。
    pub fn magnitude_db(&self) -> f64 {
        20.0 * self.magnitude().max(MAGNITUDE_FLOOR).log10()
    }

    /// wrap 到 (−180,180] 的相位（度）。
    pub fn phase_deg(&self) -> f64 {
        self.im.atan2(self.re) * 180.0 / PI
    }
}

/// 直接 DTFT 求 H(f)（O(L)；复指数递推 + 每 64 点重锚，独立于构造）。
pub fn response_at(coeffs: &[f64], fs: f64, f_hz: f64) -> FreqResponse {
    if !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return FreqResponse {
            re: f64::NAN,
            im: f64::NAN,
        };
    }
    let omega = 2.0 * PI * f_hz / fs;
    let step_re = (-omega).cos();
    let step_im = (-omega).sin();
    let mut cur_re = 1.0_f64;
    let mut cur_im = 0.0_f64;
    let mut h_re = 0.0_f64;
    let mut h_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        h_re += h * cur_re;
        h_im += h * cur_im;
        let next_re = cur_re * step_re - cur_im * step_im;
        let next_im = cur_re * step_im + cur_im * step_re;
        cur_re = next_re;
        cur_im = next_im;
        if n % 64 == 63 {
            let angle = -omega * (n as f64 + 1.0);
            cur_re = angle.cos();
            cur_im = angle.sin();
        }
    }
    FreqResponse { re: h_re, im: h_im }
}

/// |H(f)|（dB）。
pub fn magnitude_db_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).magnitude_db()
}

/// 相位（度，wrap 到 (−180,180]）。
pub fn phase_deg_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).phase_deg()
}

/// 有效支撑 (first, last, effective_taps)，eps 相对 max|h|（§1.3；本模块调用 eps=1e-6）。
pub fn effective_support(coeffs: &[f64], eps: f64) -> (usize, usize, usize) {
    if coeffs.is_empty() {
        return (0, 0, 0);
    }
    let peak = coeffs.iter().fold(0.0_f64, |acc, &h| acc.max(h.abs()));
    if peak == 0.0 || !peak.is_finite() || !eps.is_finite() {
        return (0, coeffs.len() - 1, coeffs.len());
    }
    let threshold = eps * peak;
    let first = coeffs
        .iter()
        .position(|&h| h.abs() > threshold)
        .unwrap_or(0);
    let last = coeffs
        .iter()
        .rposition(|&h| h.abs() > threshold)
        .unwrap_or(coeffs.len() - 1);
    (first, last, last - first + 1)
}

/// 振铃统计（§1.4；NOS 的 L=1 必然 n/a：主峰左右排除窗都越界）。
#[derive(Clone, Debug, PartialEq)]
pub struct RingingStats {
    /// 排除窗 W=ceil(0.000333·fs)（samples）。
    pub exclusion_samples: usize,
    /// pre_ring_db；不可用（支撑过短）为 None。
    pub pre_ring_db: Option<f64>,
    /// post_ring_db；不可用为 None。
    pub post_ring_db: Option<f64>,
    /// 是否适用（支撑足够长才有值）。
    pub applicable: bool,
    /// 不可用原因（applicable=false 时必填）。
    pub reason: Option<String>,
}

impl RingingStats {
    /// 支撑过短导致 n/a 的占位结果。
    fn not_applicable(exclusion_samples: usize, reason: String) -> Self {
        RingingStats {
            exclusion_samples,
            pre_ring_db: None,
            post_ring_db: None,
            applicable: false,
            reason: Some(reason),
        }
    }
}

/// 计算振铃统计（§1.4：p=首个 argmax|h|；W=ceil(0.000333·fs)；排除窗内不计）。
pub fn ringing_stats(coeffs: &[f64], fs: f64) -> RingingStats {
    if coeffs.is_empty() || !fs.is_finite() || fs <= 0.0 {
        return RingingStats::not_applicable(0, "空系数或 fs 非法".to_string());
    }
    let w = (RINGING_EXCLUSION_SECONDS * fs).ceil() as usize;
    let mut peak = 0.0_f64;
    let mut p_int = 0_usize;
    for (n, &h) in coeffs.iter().enumerate() {
        let a = h.abs();
        if a > peak {
            peak = a;
            p_int = n;
        }
    }
    let l = coeffs.len();
    if p_int < w || p_int + w >= l || peak == 0.0 {
        return RingingStats::not_applicable(
            w,
            format!("支撑过短：L={l}, 主峰 p={p_int}, 排除窗 W={w}，§1.4 两侧 n/a"),
        );
    }
    let mut pre = 0.0_f64;
    for &h in &coeffs[..p_int - w] {
        pre = pre.max(h.abs());
    }
    let mut post = 0.0_f64;
    for &h in &coeffs[p_int + w + 1..] {
        post = post.max(h.abs());
    }
    RingingStats {
        exclusion_samples: w,
        pre_ring_db: Some(20.0 * (pre / peak).max(MAGNITUDE_FLOOR).log10()),
        post_ring_db: Some(20.0 * (post / peak).max(MAGNITUDE_FLOOR).log10()),
        applicable: true,
        reason: None,
    }
}

/// 全频带平坦性扫描（§3.7「response 全 0 dB/0°」；§1.2 网格 20 Hz–0.499·fs）。
#[derive(Clone, Debug, PartialEq)]
pub struct FlatResponseScan {
    /// 扫描点数（>= max(8192, 8·L)）。
    pub grid_points: usize,
    /// max |mag_db|（应为 0）。
    pub max_abs_mag_db: f64,
    /// max |phase_deg|（应为 0）。
    pub max_abs_phase_deg: f64,
}

/// 在 §1.2 对数网格上扫描平坦性（20 Hz–0.499·fs，点数 >= max(8192, 8·L)）。
pub fn scan_flat_response(coeffs: &[f64], fs: f64) -> FlatResponseScan {
    let hi = NYQUIST_GUARD_FRACTION * fs;
    let grid_points = 8_192usize.max(8 * coeffs.len().max(1));
    if !fs.is_finite() || fs <= 0.0 || coeffs.is_empty() || !(hi > MEASUREMENT_LO_HZ) {
        return FlatResponseScan {
            grid_points,
            max_abs_mag_db: f64::INFINITY,
            max_abs_phase_deg: f64::INFINITY,
        };
    }
    let ratio = hi / MEASUREMENT_LO_HZ;
    let mut max_mag = 0.0_f64;
    let mut max_phase = 0.0_f64;
    for k in 0..grid_points {
        let t = k as f64 / (grid_points - 1) as f64;
        let f = MEASUREMENT_LO_HZ * ratio.powf(t);
        let r = response_at(coeffs, fs, f);
        max_mag = max_mag.max(r.magnitude_db().abs());
        max_phase = max_phase.max(r.phase_deg().abs());
    }
    FlatResponseScan {
        grid_points,
        max_abs_mag_db: max_mag,
        max_abs_phase_deg: max_phase,
    }
}

// ---------------------------------------------------------------------------
// 参数 / 错误 / 验证后参数
// ---------------------------------------------------------------------------

/// 非法频带参数（§3.7：NOS 不接受任何频带参数；出现即 E_FIR_NOS）。
///
/// 只用于「显式拒绝」：调用方若从旧 YAML/旧 profile 读到 band 参数，必须原样交给 NOS 校验并拒收，
/// 不能静默丢弃（静默丢弃等于把错误设计伪装成旁路）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosBandParams {
    /// 请求的通带 [lo, hi]（Hz）。
    pub passband_hz: [f64; 2],
    /// 请求的阻带 [lo, hi]（Hz）。
    pub stopband_hz: [f64; 2],
    /// 请求的通带纹波（dB）。
    pub ripple_db: f64,
    /// 请求的阻带衰减（dB）。
    pub attenuation_db: f64,
}

impl NosBandParams {
    /// 旧 profile 风格的完整频带参数（任一字段都会被拒绝）。
    pub fn new(
        passband_hz: [f64; 2],
        stopband_hz: [f64; 2],
        ripple_db: f64,
        attenuation_db: f64,
    ) -> Self {
        NosBandParams {
            passband_hz,
            stopband_hz,
            ripple_db,
            attenuation_db,
        }
    }
}

/// 非法窗参数（§3.7：NOS 不接受任何窗参数；出现即 E_FIR_NOS）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NosRequestedWindow {
    /// Kaiser 窗（含 beta）。
    Kaiser,
    /// Hann 窗。
    Hann,
    /// Blackman-Harris 窗。
    BlackmanHarris,
    /// 最小二乘/等波纹方法选择。
    LeastSquares,
}

impl NosRequestedWindow {
    /// schema 字符串。
    pub fn schema_name(self) -> &'static str {
        match self {
            NosRequestedWindow::Kaiser => "kaiser",
            NosRequestedWindow::Hann => "hann",
            NosRequestedWindow::BlackmanHarris => "blackman_harris",
            NosRequestedWindow::LeastSquares => "least_squares",
        }
    }
}

/// 可选目标记录（§6.10：nos 的幅度/相位目标「可选（仅记录）」，不参与设计）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NosTargetRecord {
    /// 目标来源标签（文件/sha 的短说明；只记录，不解析）。
    pub label: String,
    /// 目标是否含幅度列（只记录）。
    pub has_magnitude: bool,
    /// 目标是否含相位列（只记录；NOS 不消费相位）。
    pub has_phase: bool,
}

impl NosTargetRecord {
    /// 新建目标记录。
    pub fn new(label: impl Into<String>, has_magnitude: bool, has_phase: bool) -> Self {
        NosTargetRecord {
            label: label.into(),
            has_magnitude,
            has_phase,
        }
    }
}

/// NOS 设计参数（§3.7 参数表；字段可直改，构造器给默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosSpec {
    /// 运行采样率（Hz）；FIR fs 必须等于运行 fs（§7.2）。
    pub fs: f64,
    /// 显式 bypass 开关；NOS 必须为 true（不能隐式旁路）。
    pub bypass: bool,
    /// 支撑长度 L；NOS 必须 = 1。
    pub taps: usize,
    /// 声明体延迟 D（samples）；NOS 必须 = 0（任何 delay 归 T40）。
    pub declared_delay_samples: f64,
    /// 显式携带的频带参数（None 才合法；Some → E_FIR_NOS）。
    pub requested_band: Option<NosBandParams>,
    /// 显式携带的窗参数（None 才合法；Some → E_FIR_NOS）。
    pub requested_window: Option<NosRequestedWindow>,
    /// 可选目标记录（仅记录，不参与设计）。
    pub recorded_target: Option<NosTargetRecord>,
}

impl NosSpec {
    /// 显式 delta 旁路：fs、bypass=true、taps=1、D=0、无频带/窗参数、无 target。
    pub fn delta(fs: f64) -> Self {
        NosSpec {
            fs,
            bypass: true,
            taps: NOS_TAPS,
            declared_delay_samples: NOS_DELAY_SAMPLES,
            requested_band: None,
            requested_window: None,
            recorded_target: None,
        }
    }

    /// §3.7/V18 参考预设：48 kHz 的 delta。
    pub fn reference_preset_48k() -> Self {
        Self::delta(48_000.0)
    }

    /// 改 taps（测试/历史 profile 迁移用；合法值仍只有 1）。
    pub fn with_taps(mut self, taps: usize) -> Self {
        self.taps = taps;
        self
    }

    /// 改显式 bypass 开关（NOS 只接受 true）。
    pub fn with_bypass(mut self, bypass: bool) -> Self {
        self.bypass = bypass;
        self
    }

    /// 声明体延迟（NOS 只接受 0）。
    pub fn with_declared_delay_samples(mut self, delay_samples: f64) -> Self {
        self.declared_delay_samples = delay_samples;
        self
    }

    /// 显式携带频带参数（提交后 E_FIR_NOS；用于拒绝而不是静默丢弃）。
    pub fn with_band_params(mut self, band: NosBandParams) -> Self {
        self.requested_band = Some(band);
        self
    }

    /// 显式携带窗参数（提交后 E_FIR_NOS）。
    pub fn with_window(mut self, window: NosRequestedWindow) -> Self {
        self.requested_window = Some(window);
        self
    }

    /// 记录可选目标（只记录，不参与设计；§6.10）。
    pub fn with_recorded_target(mut self, target: NosTargetRecord) -> Self {
        self.recorded_target = Some(target);
        self
    }

    /// Nyquist 频率 fs/2（Hz）。
    pub fn nyquist_hz(&self) -> f64 {
        0.5 * self.fs
    }
}

/// NOS 失败码承载的错误（§2.5；不发明规范外成功路径）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NosError {
    /// E_FIR_TAPS：系数文件样本数与声明 taps 不符。
    Taps(String),
    /// E_FIR_NUMERIC：h 含 NaN/Inf（nos 的 max|h|=0 例外见 §2.5，归恒等判据）。
    Numeric(String),
    /// E_FIR_DELAY：declared delay 与家族规则 nos=0 不符。
    Delay(String),
    /// E_FIR_BAND：fs 等通用频带/采样参数越界。
    Band(String),
    /// E_FIR_NOS：NOS 参数非恒等（taps≠1/非显式 bypass/频带/窗参数/恒等失败/绕保护）。
    Nos(String),
}

impl NosError {
    /// 规范 §2.5 的失败码字符串。
    pub fn code(&self) -> &'static str {
        match self {
            NosError::Taps(_) => "E_FIR_TAPS",
            NosError::Numeric(_) => "E_FIR_NUMERIC",
            NosError::Delay(_) => "E_FIR_DELAY",
            NosError::Band(_) => "E_FIR_BAND",
            NosError::Nos(_) => "E_FIR_NOS",
        }
    }

    /// 人类可读细节。
    pub fn detail(&self) -> &str {
        match self {
            NosError::Taps(s)
            | NosError::Numeric(s)
            | NosError::Delay(s)
            | NosError::Band(s)
            | NosError::Nos(s) => s,
        }
    }
}

impl fmt::Display for NosError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for NosError {}

/// 通过 validate 的完整参数（派生量集中在这里，构造与验证共用同一口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedNosSpec {
    /// 原参数（克隆）。
    pub spec: NosSpec,
    /// 支撑 L（=1）。
    pub taps: usize,
    /// 体延迟 D（=0）。
    pub delay_samples: f64,
    /// delay_ms = D/fs·1000（=0）。
    pub delay_ms: f64,
    /// fs/2（Hz）。
    pub nyquist_hz: f64,
    /// manifest filtering 值（none）。
    pub filtering: &'static str,
    /// 旁路替换物（delta）。
    pub replacement: BypassReplacement,
    /// taps/内存预算分类。
    pub budget: NosBudget,
    /// 是否必须保留后置输出保护（恒 true）。
    pub protection_required: bool,
    /// 可选目标是否只记录（恒 true；§6.10）。
    pub target_recorded_only: bool,
}

/// 校验 NOS 参数（§3.7 参数约束 + §2.2 延迟规则 + §7.2 预算），返回派生量。
pub fn validate(spec: &NosSpec) -> Result<ValidatedNosSpec, NosError> {
    use NosError as E;

    validate_fs(spec.fs)?;
    if !spec.bypass {
        return Err(E::Nos(
            "NOS 必须是显式 bypass（bypass=true）；隐式旁路/自动旁路不属于本家族（§3.7 显式 bypass）"
                .to_string(),
        ));
    }
    if spec.taps != NOS_TAPS {
        return Err(E::Nos(format!(
            "NOS 的 taps 必须=1（delta，§3.7/§9 V18），实际 {}；需要其它支撑请用对应家族或回到调用方重设计",
            spec.taps
        )));
    }
    if !spec.declared_delay_samples.is_finite() {
        return Err(E::Delay(format!(
            "declared_delay_samples 必须有限且=0（nos），实际 {}",
            spec.declared_delay_samples
        )));
    }
    if spec.declared_delay_samples != NOS_DELAY_SAMPLES {
        return Err(E::Delay(format!(
            "NOS 的 delay_samples 必须=0（§2.2 家族规则 nos=0、§9 V18），实际 {}；整数/分数 delay 与极性由 T40（phase/delay/polarity）承担，不在 NOS 里冒充",
            spec.declared_delay_samples
        )));
    }
    if let Some(band) = &spec.requested_band {
        return Err(E::Nos(format!(
            "NOS 不接受任何频带参数（§3.7 拒收）：收到 passband_hz={:?}/stopband_hz={:?}/ripple_db={}/attenuation_db={}；需要频带响应请选 linear/min_*/apodizing/brickwall/corrected 对应家族",
            band.passband_hz, band.stopband_hz, band.ripple_db, band.attenuation_db
        )));
    }
    if let Some(window) = spec.requested_window {
        return Err(E::Nos(format!(
            "NOS 不接受任何窗参数（§3.7 拒收）：收到 window={}；旁路不设计任何窗",
            window.schema_name()
        )));
    }

    let delay_samples = NOS_DELAY_SAMPLES;
    let delay_ms = delay_samples / spec.fs * 1000.0;
    let budget = NosBudget::identity();
    Ok(ValidatedNosSpec {
        spec: spec.clone(),
        taps: NOS_TAPS,
        delay_samples,
        delay_ms,
        nyquist_hz: spec.nyquist_hz(),
        filtering: FILTERING_NONE,
        replacement: BypassReplacement::Delta,
        budget,
        protection_required: true,
        target_recorded_only: true,
    })
}

// ---------------------------------------------------------------------------
// 独立验证（§3.7 判据 + §1.2/§1.5/§2.4）
// ---------------------------------------------------------------------------

/// 平坦响应容差（dB；精确 delta 的 DTFT 应为 0，留浮点余量）。
pub const FLAT_MAGNITUDE_TOLERANCE_DB: f64 = 1e-9;

/// 平坦相位容差（度；精确 delta 的 DTFT 应为 0）。
pub const FLAT_PHASE_TOLERANCE_DEG: f64 = 1e-9;

/// §3.7 判据逐条结果。
#[derive(Clone, Debug, PartialEq)]
pub struct NosChecks {
    /// h 全部有限（NaN/Inf 已在 verify 硬错误拒绝；此处记录）。
    pub finite: bool,
    /// taps=1 且 len(h)=1。
    pub taps: bool,
    /// 恒等：|h[0]−1| <= 1e-12（§3.7/§9 V18）。
    pub identity: bool,
    /// D=0 且 delay_ms=0（§2.2 家族规则）。
    pub delay_zero: bool,
    /// response 全 0 dB/0°（§3.7；§1.2 网格复算）。
    pub flat_response: bool,
    /// 参考归一化：|H(reference)| 与声明增益差 <= 0.01 dB（§1.5）。
    pub reference: bool,
    /// 20 Hz/20 kHz Endpoint 完整（measured 或 not_applicable_at_fs，§2.4）。
    pub endpoints: bool,
    /// taps/内存预算分类（§7.2）。
    pub budget: bool,
    /// manifest filtering=none（§3.7）。
    pub filtering_none: bool,
}

impl NosChecks {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.finite
            && self.taps
            && self.identity
            && self.delay_zero
            && self.flat_response
            && self.reference
            && self.endpoints
            && self.budget
            && self.filtering_none
    }
}

/// 参考归一化统计（§1.5）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（Hz；0 = DC）。
    pub hz: f64,
    /// 声明增益（dB；NOS 固定 0 dB）。
    pub gain_db: f64,
    /// 实测增益（dB）。
    pub measured_gain_db: f64,
    /// |measured − declared|（dB）。
    pub error_db: f64,
}

/// NOS 独立验证报告（T55 报告器/独立 oracle 输入；不含 WAV/manifest 落盘）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosVerification {
    /// 采样率（Hz）。
    pub fs: f64,
    /// taps（=1）。
    pub taps: usize,
    /// 旁路替换物（delta）。
    pub replacement: BypassReplacement,
    /// D=0（samples）。
    pub delay_samples: f64,
    /// delay_ms（=0）。
    pub delay_ms: f64,
    /// 恒等误差 |h[0]−1|。
    pub identity_error: f64,
    /// 全带平坦性扫描（§3.7 response 全 0 dB/0°）。
    pub response: FlatResponseScan,
    /// 参考归一化（DC 0 dB）。
    pub reference: ReferenceStats,
    /// 20 Hz/20 kHz 端点（§2.4）。
    pub endpoints: NosEndpoints,
    /// 振铃（§1.4；L=1 时 n/a）。
    pub ringing: RingingStats,
    /// taps/内存预算分类（§7.2）。
    pub budget: NosBudget,
    /// manifest filtering 值（none）。
    pub filtering: &'static str,
    /// 无重建 DAC 复刻记录。
    pub dac_replication: DacReplicationRecord,
    /// 可选目标记录（只记录）。
    pub target_recorded: Option<NosTargetRecord>,
    /// 目标是否参与设计（恒 false，§6.10）。
    pub target_participated_in_design: bool,
    /// 逐条判据。
    pub checks: NosChecks,
    /// 未通过判据的人类可读原因（空=全过）。
    pub failures: Vec<String>,
}

impl NosVerification {
    /// 全部判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }

    /// 第一个未通过判据 → 规范失败码（全过时返回 E_FIR_NOS 的兜底说明，不用于成功路径）。
    pub fn criteria_to_error(&self) -> NosError {
        if self.passed() {
            return NosError::Nos("verification 全过，无错误可映射".to_string());
        }
        let mut reasons = Vec::new();
        if !self.checks.finite {
            reasons.push("finite");
        }
        if !self.checks.taps {
            reasons.push("taps");
        }
        if !self.checks.identity {
            reasons.push("identity");
        }
        if !self.checks.delay_zero {
            reasons.push("delay_zero");
        }
        if !self.checks.flat_response {
            reasons.push("flat_response");
        }
        if !self.checks.reference {
            reasons.push("reference");
        }
        if !self.checks.endpoints {
            reasons.push("endpoints");
        }
        if !self.checks.budget {
            reasons.push("budget");
        }
        if !self.checks.filtering_none {
            reasons.push("filtering_none");
        }
        let detail = format!(
            "NOS 验证未通过：{}；failures={:?}",
            reasons.join(","),
            self.failures
        );
        if !self.checks.finite {
            NosError::Numeric(detail)
        } else if !self.checks.taps {
            NosError::Taps(detail)
        } else {
            NosError::Nos(detail)
        }
    }
}

/// 独立验证系数是否为合法 NOS delta（§3.7 全部判据）。
///
/// 硬错误（结构不可修复）：spec 非法（validate 的 E_FIR_*）、len(h)!=1（E_FIR_TAPS）、
/// h 含 NaN/Inf（E_FIR_NUMERIC）。恒等/响应/参考等不达标写入 checks/failures 并返回报告，
/// 由调用方用 criteria_to_error() 映射失败码（与 T36 的 criteria 模式一致）。
pub fn verify_nos(spec: &NosSpec, coeffs: &[f64]) -> Result<NosVerification, NosError> {
    let validated = validate(spec)?;

    if coeffs.len() != NOS_TAPS {
        return Err(NosError::Taps(format!(
            "NOS 系数文件样本数必须=1（delta，§3.7），实际 {}",
            coeffs.len()
        )));
    }
    if coeffs.iter().any(|h| !h.is_finite()) {
        return Err(NosError::Numeric(
            "NOS 系数含 NaN/Inf；delta 必须 h=[1.0]".to_string(),
        ));
    }

    let h0 = coeffs[0];
    let identity_error = (h0 - NOS_COEFFICIENT).abs();
    let identity = identity_error <= IDENTITY_TOLERANCE;
    let response = scan_flat_response(coeffs, spec.fs);
    let flat_response = response.max_abs_mag_db <= FLAT_MAGNITUDE_TOLERANCE_DB
        && response.max_abs_phase_deg <= FLAT_PHASE_TOLERANCE_DEG;
    let measured_gain_db = magnitude_db_at(coeffs, spec.fs, 0.0);
    let reference = ReferenceStats {
        hz: 0.0,
        gain_db: 0.0,
        measured_gain_db,
        error_db: (measured_gain_db - 0.0).abs(),
    };
    let reference_ok = reference.error_db <= REFERENCE_TOLERANCE_DB;
    let endpoints = endpoints_for(spec.fs, coeffs);
    let endpoints_ok = endpoints.f20.status() == "measured"
        && match &endpoints.f20k {
            Endpoint::Measured { .. } => true,
            Endpoint::NotApplicableAtFs { reason } => !reason.is_empty(),
        };
    let budget = validated.budget;
    let budget_ok = budget.fits_regular_profile
        && budget.fits_extended_profile
        && budget.ram_bytes_f32 == F32_BYTES * NOS_TAPS;
    let filtering = validated.filtering;
    let filtering_none = filtering == FILTERING_NONE;
    let delay_zero = validated.delay_samples == NOS_DELAY_SAMPLES && validated.delay_ms == 0.0;
    let finite = true;
    let taps_ok = validated.taps == NOS_TAPS && coeffs.len() == NOS_TAPS;

    let checks = NosChecks {
        finite,
        taps: taps_ok,
        identity,
        delay_zero,
        flat_response,
        reference: reference_ok,
        endpoints: endpoints_ok,
        budget: budget_ok,
        filtering_none,
    };
    let mut failures = Vec::new();
    if !checks.taps {
        failures.push("taps 必须=1".to_string());
    }
    if !checks.identity {
        failures.push(format!(
            "恒等失败：|h[0]−1|={identity_error} > {IDENTITY_TOLERANCE}（§3.7/§9 V18）"
        ));
    }
    if !checks.delay_zero {
        failures.push("D 必须=0（§2.2 nos 规则）".to_string());
    }
    if !checks.flat_response {
        failures.push(format!(
            "response 非全 0 dB/0°：max|mag|={} dB, max|phase|={}°（§3.7）",
            response.max_abs_mag_db, response.max_abs_phase_deg
        ));
    }
    if !checks.reference {
        failures.push(format!(
            "参考归一化失败：|{}−{}|={} > {REFERENCE_TOLERANCE_DB} dB（§1.5）",
            reference.measured_gain_db, reference.gain_db, reference.error_db
        ));
    }
    if !checks.endpoints {
        failures.push("20 Hz/20 kHz Endpoint 缺失或 status 非法（§2.4）".to_string());
    }
    if !checks.budget {
        failures.push("taps/内存预算分类异常（§7.2）".to_string());
    }
    if !checks.filtering_none {
        failures.push("manifest filtering 必须=none（§3.7）".to_string());
    }

    Ok(NosVerification {
        fs: spec.fs,
        taps: coeffs.len(),
        replacement: validated.replacement,
        delay_samples: validated.delay_samples,
        delay_ms: validated.delay_ms,
        identity_error,
        response,
        reference,
        endpoints,
        ringing: ringing_stats(coeffs, spec.fs),
        budget,
        filtering,
        dac_replication: DacReplicationRecord::software_bypass_only(),
        target_recorded: spec.recorded_target.clone(),
        target_participated_in_design: false,
        checks,
        failures,
    })
}

fn endpoint_at(fs: f64, coeffs: &[f64], f_hz: f64) -> Endpoint {
    let r = response_at(coeffs, fs, f_hz);
    Endpoint::Measured {
        freq_hz: f_hz,
        mag_db: r.magnitude_db(),
        phase_deg: r.phase_deg(),
    }
}

/// 构造 20 Hz/20 kHz Endpoint（§2.4；20 kHz 在 0.499·fs 以下不可测时如实标记）。
pub fn endpoints_for(fs: f64, coeffs: &[f64]) -> NosEndpoints {
    let f20 = endpoint_at(fs, coeffs, MEASUREMENT_LO_HZ);
    let limit = NYQUIST_GUARD_FRACTION * fs;
    let f20k = if ENDPOINT_HI_HZ <= limit {
        endpoint_at(fs, coeffs, ENDPOINT_HI_HZ)
    } else {
        Endpoint::NotApplicableAtFs {
            reason: format!(
                "20 kHz > 0.499·fs={limit} Hz（§2.4 在当前 fs 下不可测），不得省略或写成裸字符串"
            ),
        }
    };
    NosEndpoints { f20, f20k }
}

// ---------------------------------------------------------------------------
// 设计产物 / manifest 声明 / 报告
// ---------------------------------------------------------------------------

/// khaen-fir/v1 manifest 中 NOS 相关字段的声明（T55 落盘用；本模块不写文件）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosManifestDeclaration {
    /// schema 常量（khaen-fir/v1）。
    pub schema: &'static str,
    /// family 字段（nos）。
    pub family: &'static str,
    /// taps 字段（1）。
    pub taps: usize,
    /// delay_samples 字段（0）。
    pub delay_samples: f64,
    /// delay_ms 字段（0）。
    pub delay_ms: f64,
    /// filtering 字段（none；§2.2/§3.7 差异见 FILTERING_FIELD_NOTE）。
    pub filtering: &'static str,
    /// 无重建 DAC 复刻声明值（none；放 design/报告段，不新增顶层未知字段）。
    pub dac_replication: &'static str,
    /// 保护是否被旁路（恒 false，§3.7）。
    pub protection_bypassed: bool,
    /// response 是否全 0 dB/0°（§3.7）。
    pub response_flat: bool,
}

/// 生成 NOS 的 manifest 声明（T55 消费；filtering 字段差异见 FILTERING_FIELD_NOTE）。
pub fn manifest_declaration(design: &NosDesign) -> NosManifestDeclaration {
    NosManifestDeclaration {
        schema: "khaen-fir/v1",
        family: FAMILY,
        taps: design.taps(),
        delay_samples: design.delay_samples,
        delay_ms: design.delay_ms,
        filtering: design.filtering,
        dac_replication: DAC_REPLICATION_NONE,
        protection_bypassed: false,
        response_flat: design.verification.checks.flat_response,
    }
}

/// NOS 设计产物（f64 系数 + 派生量 + 自验证报告）。
#[derive(Clone, Debug, PartialEq)]
pub struct NosDesign {
    /// 原参数。
    pub spec: NosSpec,
    /// f64 冲激响应（len=1，delta）。
    pub coeffs: Vec<f64>,
    /// 旁路替换物（delta）。
    pub replacement: BypassReplacement,
    /// 旁路作用域（只旁路目标 FIR、保留后置保护）。
    pub bypass_scope: BypassScope,
    /// manifest filtering 值（none）。
    pub filtering: &'static str,
    /// D=0（samples）。
    pub delay_samples: f64,
    /// delay_ms（=0）。
    pub delay_ms: f64,
    /// 参考频点（0 = DC）。
    pub reference_hz: f64,
    /// 声明参考增益（0 dB）。
    pub reference_gain_db: f64,
    /// max|h|（=1）。
    pub peak_abs: f64,
    /// Σh（=1，DC 增益）。
    pub sum_coeffs: f64,
    /// Σ|h|（=1，最坏峰值增益；§1.5 峰值余量建议）。
    pub sum_abs_coeffs: f64,
    /// 峰值余量建议（dB = −20log10(Σ|h|) = 0）。
    pub peak_margin_hint_db: f64,
    /// taps/内存预算分类。
    pub budget: NosBudget,
    /// 无重建 DAC 复刻记录。
    pub dac_replication: DacReplicationRecord,
    /// 可选目标记录（只记录）。
    pub target_recorded: Option<NosTargetRecord>,
    /// 目标是否参与设计（恒 false）。
    pub target_participated_in_design: bool,
    /// 自验证报告。
    pub verification: NosVerification,
}

impl NosDesign {
    /// taps = len(h)（=1）。
    pub fn taps(&self) -> usize {
        self.coeffs.len()
    }

    /// f32 导出（WAV/运行器用）。
    pub fn coeffs_f32(&self) -> Vec<f32> {
        self.coeffs.iter().map(|&h| h as f32).collect()
    }

    /// 有效支撑 (first, last, effective_taps)，eps=1e-6（§1.3）。
    pub fn effective_support(&self) -> (usize, usize, usize) {
        effective_support(&self.coeffs, 1e-6)
    }

    /// 振铃统计（§1.4）。
    pub fn ringing(&self) -> RingingStats {
        self.verification.ringing.clone()
    }

    /// manifest 声明（T55 落盘用）。
    pub fn manifest_declaration(&self) -> NosManifestDeclaration {
        manifest_declaration(self)
    }

    /// 报告文本（§3.7 必须含「本软件旁路，不改变/不模拟 MOTU DAC 内部重建滤波」+ 保护仍在图内）。
    pub fn report_text(&self) -> String {
        format!(
            "family={FAMILY}; filtering={}; replacement={}; delay_samples={}; delay_ms={}; response=flat(0 dB/0 deg); dac_replication={}; statement={}; commitment={}; protection={}; bypass_scope={}",
            self.filtering,
            self.replacement.schema_name(),
            self.delay_samples,
            self.delay_ms,
            self.dac_replication.claim.schema_name(),
            self.dac_replication.statement,
            self.dac_replication.commitment,
            PROTECTION_NOT_BYPASSED_STATEMENT,
            self.bypass_scope.schema_name()
        )
    }
}

/// NOS 设计入口：校验参数 → 生成 delta 系数 → 自验证（任何判据不过即返回对应 E_FIR_*）。
pub fn design(spec: &NosSpec) -> Result<NosDesign, NosError> {
    let validated = validate(spec)?;
    let coeffs = vec![NOS_COEFFICIENT];
    let verification = verify_nos(spec, &coeffs)?;
    if !verification.passed() {
        return Err(verification.criteria_to_error());
    }
    let sum_coeffs = coeffs.iter().sum::<f64>();
    let sum_abs_coeffs = coeffs.iter().map(|h| h.abs()).sum::<f64>();
    Ok(NosDesign {
        spec: spec.clone(),
        coeffs,
        replacement: validated.replacement,
        bypass_scope: BypassScope::output_target_fir_only(),
        filtering: validated.filtering,
        delay_samples: validated.delay_samples,
        delay_ms: validated.delay_ms,
        reference_hz: 0.0,
        reference_gain_db: 0.0,
        peak_abs: 1.0,
        sum_coeffs,
        sum_abs_coeffs,
        peak_margin_hint_db: -20.0 * sum_abs_coeffs.log10(),
        budget: validated.budget,
        dac_replication: DacReplicationRecord::software_bypass_only(),
        target_recorded: spec.recorded_target.clone(),
        target_participated_in_design: false,
        verification,
    })
}

/// 设计 + 判据闭环（与 design 等价；保留 T10/T36 的命名惯例供 T55 统一调用）。
pub fn design_checked(spec: &NosSpec) -> Result<NosDesign, NosError> {
    design(spec)
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T38 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod nos_tests {
    use super::*;

    include!("../tests/T38/nos_cases.rs");
}
