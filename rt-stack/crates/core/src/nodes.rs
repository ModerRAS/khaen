//! [NONRT] 图节点：输入选择/零填、role scatter 与输出 FIR、模态 IIR、volume ramp、limiter/保护节点
//! —— WBS T23 写域（crates/core/src/nodes.rs）。
//!
//! # 作用与边界
//!
//! 本模块把「配置驱动的输出图节点」编译成与既有内核/编译接口可直接衔接的 NONRT 计划：
//!
//! ~~~text
//! 输入选择 A_map（只选择/零填） → 低带矩阵 G（T12 编译）
//!   → P_post（crossover FIR → 模态 PEQ(IIR) → 输出目标 FIR → gain → delay → polarity）
//!   → volume ramp → 保护（limiter → clip → ramp） → role scatter → 容器零填
//! ~~~
//!
//! - **链序**取自 spec/fir-toolbox.md §5.1（A_map→G→P_post(crossover→modal PEQ→target FIR→
//!   gain→delay→polarity)→保护/limiter→scatter）并显式加入 volume ramp（compat-matrix P07：
//!   音量变化必须 ramp 防爆音，mute 与保护静音可区分）。本模块输出的节点图顺序即规范序。
//! - **禁止上混/下混/自动造声道**：A_map 只允许「每角色至多一个源、每源至多一个角色」；
//!   scatter 必须单射；未选源角色显式零填；容器中未被 scatter 使用的通道逐样本 +0.0
//!   （由 kernel 零填与计划报告共同保证）。任何 Mixer/上混/下混节点都不存在，未知节点类型
//!   在配置层即拒收（ERR_UNSUPPORTED/ERR_ROLES）。
//! - **不得按 role 名硬编码**：角色身份 = 索引（roles[i]）；RoleConfig::name 只用于
//!   KhaenRoleV1.name_hash（CRC-32C）等 CTL 元数据，不参与任何校验/滤波/路由决策。
//!   高度（HFL/HFR）不获得任何默认 HP/LP/人工 delay：没有显式 FIR 段就是 identity delta。
//! - **配置驱动**：所有滤波系数/时延/极性/增益/IIR 段/volume/保护参数都来自配置；
//!   本模块不发明默认阈值（compat-matrix P14：limiter attack/lookahead/true-peak、
//!   每输出 threshold 等未定项必须由配置显式给出；未给出的项标记 unresolved_p14，
//!   不静默补默认值）。
//!
//! # 与 T12 编译 / T07–T09 内核的接口
//!
//! - 输出 CompiledPlans（crate::compile::compile 的真实调用结果，factorized/fused 两种图 +
//!   选择/成本报告），因此与 T12 的 FactorizedGraph/FusedGraph/StagePlan 完全一致。
//! - 输出 DirectTopology + f32 系数视图 DirectCoeffsF32（A/G/P 行主序、声明支撑零填），
//!   DirectTopology::validate() 通过后可直接交给 T07 direct / T08 partitioned / T09 workers；
//!   超出当前 kernel 4096 taps 上限的 extended 图会显式标记 kernel_admissible=false，
//!   不假装能执行（T56 扩展准入另属后续）。
//! - 输出 plan 投影（DiagFirPlan/RolePlan/IirStagePlan/VolumePlan/ProtectionPlan），
//!   字段语义对齐 core-v1 §11.10（含 v1.1 delay_q16）；ABI 打包归 T26/T66（当前 abi.rs
//!   仍是 v1.0 scaffold，与 T12 的登记一致，本模块不写 ABI 字节、不碰 RT 状态）。
//! - 输出 RuntimeStage 顺序（kernel 之后）：模态 IIR → role delay → volume ramp → limiter →
//!   clip → protection ramp → scatter。内核当前只有单 P FIR 接口，因此模态 IIR（规范序在目标
//!   FIR 之前）与目标 FIR 交换到 kernel 之后：二者都是 LTI，交换精确（f64 验证，见
//!   tests/T23 的 nodes_modal_iir_fir_commutation），规范序仍记录在节点图/报告中。
//!
//! # 单次应用（禁止双扣/双乘）
//!
//! - role gain 与 polarity **折叠进输出对角 FIR 系数**（线性缩放/取负，精确）；RolePlan
//!   的 gain_q15 写 1.0（32767）、polarity 写 +1，避免运行时再乘一次。
//! - role delay 只写入 RolePlan.delay_samples/delay_q16（整数 + Q16 次样本），不再进 FIR。
//! - 模态 IIR 保持 IIR（不硬截成 FIR）；其尾长/极点/requires_f64 由本模块校验并记录。
//! - 保护（limiter/clip/ramp）不可绕过：三个节点必须对所有角色存在且 enabled，否则拒收；
//!   fallback/identity 图也必须保留保护（本模块的节点图恒包含保护节点）。
//!
//! # 非 RT 声明
//!
//! 本文件是 [NONRT] 模块：可分配、可 Drop，供 prepare/设计/报告使用，不进入 RT 调用图；
//! RT 执行（IIR 状态、volume ramp、limiter、保护、scatter）由 T21/T26 按本模块的
//! NodePlan/RuntimeStage 与冻结公式实现。本文件中的 biquad_process_df2t_f64 等
//! 带 _f64 的辅助函数是 offline 参考/测试用，不是 RT 入口。
//!
//! 权威输入（只读）：spec/core-v1.md §7/§9/§11.10/§13、spec/fir-toolbox.md §4/§5、
//! spec/compat-matrix.md §8（P01–P14）、scratch/lead/planner/DSP-HOST-DECISION.md §1/§3/§5、
//! scratch/lead/planner/KHAEN-RT-STACK-WBS.md §1/§7。

use crate::compile::{
    compile, convolve_linear, design_delay_to_plan, CompileInput, CompileOptions, CompileReport,
    CompiledPlans, DelayPlan, FusedPathPlan, FusionForm, MatrixStagePlan, StagePlan,
    COMPILE_NO_SOURCE,
};
use crate::direct::{
    DirectCoeffsF32, DirectTopology, DIRECT_MAX_CONTAINERS, DIRECT_MAX_PARTICIPANTS,
    DIRECT_MAX_ROLES, DIRECT_MAX_SOURCES, DIRECT_MAX_TAPS, DIRECT_NO_SOURCE,
};
use crate::rt::{
    RtStatus, MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES,
    MAX_PLAN_NODES,
};

// ---------------------------------------------------------------------------
// 冻结常量（结构/安全边界；不是音频调参默认值）
// ---------------------------------------------------------------------------

/// 每角色输出 FIR 段数结构上限（crossover/target/phase 组合；远小于 MAX_PLAN_NODES）。
pub const OUTPUT_FIR_SEGMENTS_MAX: usize = 8;
/// 每角色 biquad 总数上限（core-v1/§5.2：<= 32）。
pub const MODAL_BIQUADS_MAX: usize = 32;
/// 10 段模态 PEQ 定义的 peaking 上限（生产端 T41/T64 契约；消费端按 §5.2 只保证 <=32 总数）。
pub const MODAL_PEAKING_PRODUCER_MAX: usize = 10;
/// 极点半径上限（§5.4：|p| <= 0.999999）。
pub const POLE_RADIUS_MAX: f64 = 0.999_999;
/// f32 量化响应复算容差（§5.4：固定 2001 点网格上 <= 0.1 dB）。
pub const F32_QUANTIZATION_TOL_DB: f64 = 0.1;
/// 单段 biquad 增益绝对上限（§5.2：±24 dB）。
pub const MODAL_GAIN_LIMIT_DB: f64 = 24.0;
/// 模态 PEQ 频率下限（§5.2：20 Hz）。
pub const MODAL_FREQ_MIN_HZ: f64 = 20.0;
/// 模态 PEQ 频率上限（§5.2：min(20000, fs/2-1)）。
pub const MODAL_FREQ_MAX_HZ: f64 = 20_000.0;
/// peaking 的 RBJ Q 下限（§5.2：>= 0.1）。
pub const MODAL_PEAK_Q_MIN: f64 = 0.1;
/// shelf 的 RBJ slope S 下限（§5.2：[0.1, 1.0]，S 不是 Q）。
pub const MODAL_SHELF_S_MIN: f64 = 0.1;
/// shelf 的 RBJ slope S 上限。
pub const MODAL_SHELF_S_MAX: f64 = 1.0;
/// §5.4 f32 量化复算固定网格点数（20 Hz–20 kHz、2001 点、约 1/200 oct）。
pub const MODAL_EVAL_POINTS: usize = 2001;
/// §5.6 自洽性复算网格步进（2001 点中每 16 点取 1 点，约 1/12 oct）。
pub const MODAL_CONSISTENCY_STEP: usize = 16;
/// 自洽性幅度容差（§5.6：<= 0.1 dB）。
pub const MODAL_CONSISTENCY_DB: f64 = 0.1;
/// 自洽性相位容差（§5.6：<= 1 度）。
pub const MODAL_CONSISTENCY_DEG: f64 = 1.0;
/// 音量下限（compat-matrix P07/R19/W17：-150..+50 dB）。
pub const VOLUME_DB_MIN: f64 = -150.0;
/// 音量上限。
pub const VOLUME_DB_MAX: f64 = 50.0;
/// role gain 下限（channelmap v2 增益范围 -60..+12 dB；P04）。
pub const ROLE_GAIN_DB_MIN: f64 = -60.0;
/// role gain 上限（channelmap v2）。
pub const ROLE_GAIN_DB_MAX: f64 = 12.0;
/// 校准 trim 下限（CONTRACT：-12..+6 dB；P04）。
pub const ROLE_TRIM_DB_MIN: f64 = -12.0;
/// 校准 trim 上限。
pub const ROLE_TRIM_DB_MAX: f64 = 6.0;
/// role 级延迟上限（core-v1 §11.10：0..9600 samples @48k，200 ms）。
pub const ROLE_DELAY_MAX_MS: f64 = 200.0;
/// role 名元数据字节上限（ABI ctl_meta 用 u16 name_len）。
pub const ROLE_NAME_BYTES_MAX: usize = u16::MAX as usize;

/// KhaenDiagFirV1.flags bit0：protection（本模块保护是独立节点，输出对角 FIR 不置位）。
pub const DIAG_FLAG_PROTECTION: u16 = 1 << 0;
/// KhaenDiagFirV1.flags bit1：limiter（同上，独立节点）。
pub const DIAG_FLAG_LIMITER: u16 = 1 << 1;
/// KhaenDiagFirV1.flags bit2：本对角 FIR 含 crossover 段。
pub const DIAG_FLAG_CROSSOVER: u16 = 1 << 2;
/// KhaenDiagFirV1.flags bit3：本对角 FIR 含输出目标 FIR 段。
pub const DIAG_FLAG_TARGET: u16 = 1 << 3;
/// KhaenDiagFirV1.flags bit4：本对角 FIR 含 phase/delay/polarity FIR 段。
pub const DIAG_FLAG_PHASE_DELAY_POLARITY: u16 = 1 << 4;

/// KhaenRoleV1.flags bit0：角色 enabled。
pub const ROLE_FLAG_ENABLED: u16 = 1 << 0;
/// KhaenRoleV1.flags bit1：角色无节目源，必须逐样本零填。
pub const ROLE_FLAG_ZERO_FILL: u16 = 1 << 1;

const _: () = {
    // A_map 哨兵在 compile/direct 两个模块必须一致（T12/T07 冻结）。
    assert!(COMPILE_NO_SOURCE == DIRECT_NO_SOURCE);
    assert!(OUTPUT_FIR_SEGMENTS_MAX >= 1);
    assert!(OUTPUT_FIR_SEGMENTS_MAX <= MAX_PLAN_NODES);
    assert!(MODAL_BIQUADS_MAX >= 1 && MODAL_BIQUADS_MAX <= 32);
    assert!(MODAL_PEAKING_PRODUCER_MAX <= MODAL_BIQUADS_MAX);
    assert!(POLE_RADIUS_MAX < 1.0);
    assert!(F32_QUANTIZATION_TOL_DB > 0.0);
    assert!(MODAL_EVAL_POINTS == 2001);
    assert!(MODAL_CONSISTENCY_STEP >= 1);
    assert!(VOLUME_DB_MIN < VOLUME_DB_MAX);
    assert!(ROLE_GAIN_DB_MIN < ROLE_GAIN_DB_MAX);
    assert!(ROLE_TRIM_DB_MIN < ROLE_TRIM_DB_MAX);
    assert!(ROLE_DELAY_MAX_MS == 200.0);
    assert!(DIAG_FLAG_CROSSOVER == 0x04 && DIAG_FLAG_TARGET == 0x08);
    assert!(DIAG_FLAG_PHASE_DELAY_POLARITY == 0x10);
    assert!(ROLE_FLAG_ENABLED == 0x01 && ROLE_FLAG_ZERO_FILL == 0x02);
    assert!(DIRECT_MAX_ROLES == MAX_OUTPUT_CHANNELS);
    assert!(MAX_OUTPUT_ROLES <= DIRECT_MAX_ROLES);
    assert!(MAX_OUTPUT_CHANNELS == DIRECT_MAX_CONTAINERS);
    assert!(MAX_INPUT_CHANNELS <= DIRECT_MAX_SOURCES);
    assert!(MAX_LOW_BAND_PARTICIPANTS == DIRECT_MAX_PARTICIPANTS);
    assert!(DIRECT_MAX_TAPS == crate::rt::TAPS_NORMAL_MAX as usize);
};

// ---------------------------------------------------------------------------
// 节点类型与规范链序
// ---------------------------------------------------------------------------

/// 图节点类型（规范链序；Gain/Polarity 折叠进输出 FIR，见模块头「单次应用」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// 输入选择 A_map（只选择/零填，无 Mixer）。
    InputSelect,
    /// 低带矩阵 G（T12 编译，含 delta_D baseline）。
    LowBandMatrix,
    /// 每角色输出 FIR（crossover/target/phase 段线性卷积合成）。
    OutputFir,
    /// 模态 PEQ（IIR biquad 级联；不硬截 FIR）。
    ModalIir,
    /// 输出增益（折叠进输出 FIR 系数，不单独执行）。
    Gain,
    /// 角色/容器级延迟（整数 + Q16 次样本；运行时单一字段应用）。
    Delay,
    /// 输出极性（折叠进输出 FIR 系数符号，不单独执行）。
    Polarity,
    /// 全局音量 ramp（防爆音；mute 与保护静音可区分）。
    VolumeRamp,
    /// 每输出 limiter（角色求和后、scatter 前）。
    Limiter,
    /// 每输出 clip/有限性保护。
    ClipGuard,
    /// 启动/切换 ramp 保护。
    ProtectionRamp,
    /// role → 容器通道 scatter（单射；未用容器显式零填）。
    Scatter,
}

impl NodeKind {
    /// §5.1 规范链序（10 级 + volume ramp；Gain/Polarity 虽折叠仍保留在规范序中）。
    pub const fn canonical_order() -> [NodeKind; 12] {
        [
            NodeKind::InputSelect,
            NodeKind::LowBandMatrix,
            NodeKind::OutputFir,
            NodeKind::ModalIir,
            NodeKind::Gain,
            NodeKind::Delay,
            NodeKind::Polarity,
            NodeKind::VolumeRamp,
            NodeKind::Limiter,
            NodeKind::ClipGuard,
            NodeKind::ProtectionRamp,
            NodeKind::Scatter,
        ]
    }

    /// 规范序 rank（越大越靠后）。
    pub const fn rank(self) -> u8 {
        match self {
            NodeKind::InputSelect => 0,
            NodeKind::LowBandMatrix => 1,
            NodeKind::OutputFir => 2,
            NodeKind::ModalIir => 3,
            NodeKind::Gain => 4,
            NodeKind::Delay => 5,
            NodeKind::Polarity => 6,
            NodeKind::VolumeRamp => 7,
            NodeKind::Limiter => 8,
            NodeKind::ClipGuard => 9,
            NodeKind::ProtectionRamp => 10,
            NodeKind::Scatter => 11,
        }
    }

    /// 是否折叠进输出 FIR（不产生独立运行节点）。
    pub const fn is_folded(self) -> bool {
        matches!(self, NodeKind::Gain | NodeKind::Polarity)
    }

    /// 报告/schema 名（稳定字符串）。
    pub const fn schema_name(self) -> &'static str {
        match self {
            NodeKind::InputSelect => "input_select",
            NodeKind::LowBandMatrix => "low_band_matrix",
            NodeKind::OutputFir => "output_fir",
            NodeKind::ModalIir => "modal_peq_iir",
            NodeKind::Gain => "gain",
            NodeKind::Delay => "delay",
            NodeKind::Polarity => "polarity",
            NodeKind::VolumeRamp => "volume_ramp",
            NodeKind::Limiter => "limiter",
            NodeKind::ClipGuard => "clip_guard",
            NodeKind::ProtectionRamp => "protection_ramp",
            NodeKind::Scatter => "scatter",
        }
    }
}

/// 输出 FIR 段类型（规范序：crossover → target → phase/delay/polarity）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FirSegmentKind {
    /// 输出分频（T39；LR/BW 交给设计器，本节点只承载系数）。
    Crossover,
    /// 输出目标 FIR（T10/T11/T33–T38 家族产物）。
    Target,
    /// phase/delay/polarity FIR（T40；系数已含声明延迟/极性符号）。
    PhaseDelayPolarity,
}

impl FirSegmentKind {
    /// 规范序 rank（段序列必须非降）。
    pub const fn rank(self) -> u8 {
        match self {
            FirSegmentKind::Crossover => 0,
            FirSegmentKind::Target => 1,
            FirSegmentKind::PhaseDelayPolarity => 2,
        }
    }

    /// 报告/schema 名。
    pub const fn schema_name(self) -> &'static str {
        match self {
            FirSegmentKind::Crossover => "crossover",
            FirSegmentKind::Target => "output_target_fir",
            FirSegmentKind::PhaseDelayPolarity => "phase_delay_polarity_fir",
        }
    }

    /// KhaenDiagFirV1.flags 位。
    pub const fn diag_flag(self) -> u16 {
        match self {
            FirSegmentKind::Crossover => DIAG_FLAG_CROSSOVER,
            FirSegmentKind::Target => DIAG_FLAG_TARGET,
            FirSegmentKind::PhaseDelayPolarity => DIAG_FLAG_PHASE_DELAY_POLARITY,
        }
    }

    /// 规范序（供报告/测试）。
    pub const fn canonical_order() -> [FirSegmentKind; 3] {
        [
            FirSegmentKind::Crossover,
            FirSegmentKind::Target,
            FirSegmentKind::PhaseDelayPolarity,
        ]
    }
}

/// 模态 PEQ band 类型（§5.2；未知 type 在解析层拒收，不降级）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModalBandType {
    /// RBJ peaking（q 是 RBJ Q）。
    Peaking,
    /// RBJ low shelf（q 语义是 slope S）。
    LowShelf,
    /// RBJ high shelf（q 语义是 slope S）。
    HighShelf,
}

impl ModalBandType {
    /// 解析 schema 字符串；未知值返回 None（调用方拒收，不猜）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "peaking" => Some(ModalBandType::Peaking),
            "low_shelf" => Some(ModalBandType::LowShelf),
            "high_shelf" => Some(ModalBandType::HighShelf),
            _ => None,
        }
    }

    /// schema 字符串。
    pub const fn schema_name(self) -> &'static str {
        match self {
            ModalBandType::Peaking => "peaking",
            ModalBandType::LowShelf => "low_shelf",
            ModalBandType::HighShelf => "high_shelf",
        }
    }

    /// 是否 shelf（其 q 语义为 slope S）。
    pub const fn is_shelf(self) -> bool {
        matches!(self, ModalBandType::LowShelf | ModalBandType::HighShelf)
    }
}

/// kernel 之后的运行阶段（T26/T21 执行；规范执行顺序 = 本枚举声明顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeStage {
    /// 模态 PEQ IIR（与 kernel 的 P FIR 交换；LTI 精确）。
    ModalIir(u16),
    /// 角色级延迟（整数 + Q16；单一字段）。
    RoleDelay(u16),
    /// 全局音量 ramp。
    VolumeRamp,
    /// 每输出 limiter。
    Limiter(u16),
    /// 每输出 clip/有限性保护。
    ClipGuard(u16),
    /// 启动/切换 ramp。
    ProtectionRamp(u16),
    /// role → 容器 scatter（最后一级；之后无任何处理）。
    Scatter,
}

impl RuntimeStage {
    /// 报告/schema 名。
    pub const fn schema_name(self) -> &'static str {
        match self {
            RuntimeStage::ModalIir(_) => "modal_peq_iir",
            RuntimeStage::RoleDelay(_) => "role_delay",
            RuntimeStage::VolumeRamp => "volume_ramp",
            RuntimeStage::Limiter(_) => "limiter",
            RuntimeStage::ClipGuard(_) => "clip_guard",
            RuntimeStage::ProtectionRamp(_) => "protection_ramp",
            RuntimeStage::Scatter => "scatter",
        }
    }

    /// 关联角色（全局阶段返回 None）。
    pub const fn role(self) -> Option<u16> {
        match self {
            RuntimeStage::ModalIir(r)
            | RuntimeStage::RoleDelay(r)
            | RuntimeStage::Limiter(r)
            | RuntimeStage::ClipGuard(r)
            | RuntimeStage::ProtectionRamp(r) => Some(r),
            RuntimeStage::VolumeRamp | RuntimeStage::Scatter => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 配置结构（NONRT；全部来自 profile/校准/设计器，禁止 role 名硬编码）
// ---------------------------------------------------------------------------

/// 角色配置（角色身份 = 索引 i；name 只进 name_hash 元数据）。
#[derive(Clone, Debug, PartialEq)]
pub struct RoleConfig {
    /// A_map 选中的源通道（COMPILE_NO_SOURCE = 无源，显式零填）。
    pub src: u16,
    /// role → 容器通道（单射）。
    pub out_index: u16,
    /// channelmap 增益（-60..+12 dB；P04）。
    pub gain_db: f64,
    /// 校准 trim（-12..+6 dB；P04；与 gain_db 相加为有效增益）。
    pub trim_db: f64,
    /// 角色级延迟（设计样本，可分数；<= 200 ms，Q16 量化）。
    pub delay_samples: f64,
    /// 极性（1/-1；折叠进输出 FIR 符号）。
    pub polarity: i8,
    /// role 名（只做 metadata / name_hash；None = 无名字）。
    pub name: Option<String>,
}

impl RoleConfig {
    /// 有效设计增益（channelmap + trim）。
    pub fn effective_gain_db(&self) -> f64 {
        self.gain_db + self.trim_db
    }

    /// 是否无源（显式零填）。
    pub fn is_zero_fill(&self) -> bool {
        self.src == COMPILE_NO_SOURCE
    }
}

/// 输出 FIR 段（系数已含该段声明的延迟/极性；本节点只做配置校验与线性卷积合成）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirSegment {
    /// 段类型（规范序非降）。
    pub kind: FirSegmentKind,
    /// 声明支撑 taps（= coefficients.len()）。
    pub taps: u32,
    /// 因果 FIR 系数（f64 设计域；段内已含其声明延迟）。
    pub coefficients: Vec<f64>,
    /// 声明整数延迟（含在支撑内，只做报告/计划投影）。
    pub delay_samples: u32,
    /// 声明 Q16 次样本延迟（0..=65535）。
    pub delay_q16: u32,
}

impl FirSegment {
    /// 由系数构造（taps 必须等于系数长度；延迟由调用方声明）。
    pub fn new(
        kind: FirSegmentKind,
        coefficients: Vec<f64>,
        delay_samples: u32,
        delay_q16: u32,
    ) -> Self {
        let taps = coefficients.len() as u32;
        Self {
            kind,
            taps,
            coefficients,
            delay_samples,
            delay_q16,
        }
    }

    /// identity 段（delta，D=0）。
    pub fn identity(kind: FirSegmentKind) -> Self {
        Self::new(kind, vec![1.0], 0, 0)
    }

    /// 声明总延迟（Q16 计）。
    pub fn total_delay_q16(&self) -> u64 {
        (self.delay_samples as u64) * 65_536 + self.delay_q16 as u64
    }
}

/// 每角色输出 FIR 链（空链 = identity delta；不发明 HP/LP/delay）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OutputFirChain {
    /// 段序列（规范序：crossover → target → phase/delay/polarity）。
    pub segments: Vec<FirSegment>,
}

impl OutputFirChain {
    /// 空链（identity）。
    pub fn identity() -> Self {
        Self::default()
    }

    /// 单段链。
    pub fn single(segment: FirSegment) -> Self {
        Self {
            segments: vec![segment],
        }
    }

    /// 是否空链（identity）。
    pub fn is_identity(&self) -> bool {
        self.segments.is_empty()
    }
}

/// RBJ 模态 PEQ band（字段语义与 §5.2/§5.3 一致；b/a 以 a0=1 归一）。
#[derive(Clone, Debug, PartialEq)]
pub struct ModalBandConfig {
    /// band 类型。
    pub band_type: ModalBandType,
    /// 中心/转折频率（Hz）。
    pub freq_hz: f64,
    /// peaking 为 RBJ Q；shelf 为 slope S（不是 Q）。
    pub q: f64,
    /// 单段增益（dB，|gain| <= 24）。
    pub gain_db: f64,
    /// 归一化分子系数（f64）。
    pub b: [f64; 3],
    /// 归一化分母系数（a0 = 1）。
    pub a: [f64; 3],
}

impl ModalBandConfig {
    /// 由契约字段按 §5.3 RBJ 公式构造（f64 设计；a0=1 归一）。
    pub fn from_params(
        band_type: ModalBandType,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
        fs: f64,
    ) -> Result<Self, RtStatus> {
        let (b, a) = rbj_biquad(band_type, freq_hz, q, gain_db, fs)?;
        Ok(Self {
            band_type,
            freq_hz,
            q,
            gain_db,
            b,
            a,
        })
    }

    /// 由已导出系数 + 契约字段构造；会做 §5.6 自洽性检查（字段 vs b/a）。
    pub fn from_coefficients(
        band_type: ModalBandType,
        freq_hz: f64,
        q: f64,
        gain_db: f64,
        b: [f64; 3],
        a: [f64; 3],
        fs: f64,
    ) -> Result<Self, RtStatus> {
        let band = Self {
            band_type,
            freq_hz,
            q,
            gain_db,
            b,
            a,
        };
        band.validate(fs)?;
        let (max_db, max_deg) = band.self_consistency_deviation(fs);
        if max_db > MODAL_CONSISTENCY_DB || max_deg > MODAL_CONSISTENCY_DEG {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(band)
    }

    /// 基础校验：字段范围、有限性、a0=1、稳定性。
    pub fn validate(&self, fs: f64) -> Result<(), RtStatus> {
        if !fs.is_finite() || fs <= 0.0 {
            return Err(RtStatus::ERR_RATE);
        }
        let upper = modal_freq_upper_hz(fs);
        if !self.freq_hz.is_finite() || self.freq_hz < MODAL_FREQ_MIN_HZ || self.freq_hz > upper {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if !self.q.is_finite() || self.q <= 0.0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        match self.band_type {
            ModalBandType::Peaking => {
                if self.q < MODAL_PEAK_Q_MIN {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
            }
            ModalBandType::LowShelf | ModalBandType::HighShelf => {
                if !(MODAL_SHELF_S_MIN..=MODAL_SHELF_S_MAX).contains(&self.q) {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
            }
        }
        if !self.gain_db.is_finite() || self.gain_db.abs() > MODAL_GAIN_LIMIT_DB {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if !self.a[0].is_finite() || (self.a[0] - 1.0).abs() > 1e-9 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        for v in self.b.iter().chain(self.a.iter()) {
            if !v.is_finite() {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
        }
        if self.max_pole_radius() > POLE_RADIUS_MAX {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(())
    }

    /// biquad 复数响应 (re, im)（z = e^{jw}）。
    pub fn complex_response(&self, freq_hz: f64, fs: f64) -> (f64, f64) {
        biquad_complex_response(&self.b, &self.a, freq_hz, fs)
    }

    /// biquad 幅度响应（dB）。
    pub fn magnitude_db(&self, freq_hz: f64, fs: f64) -> f64 {
        biquad_magnitude_db(&self.b, &self.a, freq_hz, fs)
    }

    /// biquad 相位响应（deg）。
    pub fn phase_deg(&self, freq_hz: f64, fs: f64) -> f64 {
        biquad_phase_deg(&self.b, &self.a, freq_hz, fs)
    }

    /// 极点半径最大值（a0=1 的二次分母）。
    pub fn max_pole_radius(&self) -> f64 {
        biquad_max_pole_radius(&self.a)
    }

    /// §5.4 f32 量化后最大响应偏差（dB，固定 2001 点网格）。
    pub fn f32_quantization_max_deviation_db(&self, fs: f64) -> f64 {
        let b_f32 = quantize_coeffs_f32(&self.b);
        let a_f32 = quantize_coeffs_f32(&self.a);
        max_magnitude_deviation_db(&self.b, &self.a, &b_f32, &a_f32, fs)
    }

    /// §5.4 是否需要 f64 运行路径（f32 量化偏差 > 0.1 dB）。
    pub fn requires_f64(&self, fs: f64) -> bool {
        self.f32_quantization_max_deviation_db(fs) > F32_QUANTIZATION_TOL_DB
    }

    /// §5.6 自洽性：由字段按 §5.3 重算的响应 vs 导出 b/a 响应的最大偏差 (dB, deg)。
    pub fn self_consistency_deviation(&self, fs: f64) -> (f64, f64) {
        self_consistency_deviation(
            self.band_type,
            self.freq_hz,
            self.q,
            self.gain_db,
            &self.b,
            &self.a,
            fs,
        )
    }
}

/// 每角色模态 PEQ 通道（IIR biquad 级联；消费端按 §5.2 接受 <= 32 段任意已知 type）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ModalIirChannel {
    /// biquad 段（有序；每段独立状态）。
    pub bands: Vec<ModalBandConfig>,
}

impl ModalIirChannel {
    /// 空通道（无 IIR）。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 由 band 列表构造。
    pub fn new(bands: Vec<ModalBandConfig>) -> Self {
        Self { bands }
    }

    /// 是否无 IIR。
    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// 段数。
    pub fn band_count(&self) -> usize {
        self.bands.len()
    }

    /// 任一段是否需要 f64 运行路径。
    pub fn requires_f64(&self, fs: f64) -> bool {
        self.bands.iter().any(|band| band.requires_f64(fs))
    }

    /// 通道级 cascade 幅度（dB，f64 系数）。
    pub fn cascade_magnitude_db(&self, freq_hz: f64, fs: f64) -> f64 {
        self.bands
            .iter()
            .map(|band| band.magnitude_db(freq_hz, fs))
            .sum()
    }
}

/// 音量 ramp 节点配置（compat-matrix P07：音量变化必须 ramp；mute 与保护静音可区分）。
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeRampConfig {
    /// 目标音量（dB，-150..+50）。
    pub volume_db: f64,
    /// 用户 mute（与保护静音不同；保护静音由 T21 安全路径执行，不写本字段）。
    pub user_mute: bool,
    /// ramp 帧数（0 = 立即生效；>0 = 在 ramp_frames 帧内线性 dB 插值到目标）。
    pub ramp_frames: u32,
}

impl VolumeRampConfig {
    /// 校验量程与有限性。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if !self.volume_db.is_finite()
            || self.volume_db < VOLUME_DB_MIN
            || self.volume_db > VOLUME_DB_MAX
        {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(())
    }

    /// 目标线性增益（user_mute 时为 0.0）。
    pub fn target_linear_gain(&self) -> f64 {
        if self.user_mute {
            0.0
        } else {
            db_to_linear(self.volume_db)
        }
    }
}

/// 每输出 limiter 配置（参数由 profile 显式给出；P14 未定项不发明默认值）。
#[derive(Clone, Debug, PartialEq)]
pub struct LimiterConfig {
    /// 必须为 true（保护不得被 bypass；false -> ERR_UNSUPPORTED）。
    pub enabled: bool,
    /// 限幅阈值（dBFS，<= 0；每输出显式配置）。
    pub threshold_db: f64,
    /// release 时间（ms，> 0）。
    pub release_ms: f64,
    /// attack 时间（ms，> 0）；None = P14 未定，标记 unresolved_p14。
    pub attack_ms: Option<f64>,
    /// lookahead 帧数；None = P14 未定，标记 unresolved_p14。
    pub lookahead_frames: Option<u32>,
    /// 最大增益帽（dB，>= 0；P03/P04）。
    pub max_gain_db: f64,
}

impl LimiterConfig {
    /// 必填字段构造（attack/lookahead 走 with_* 显式补充）。
    pub fn new(threshold_db: f64, release_ms: f64, max_gain_db: f64) -> Self {
        Self {
            enabled: true,
            threshold_db,
            release_ms,
            attack_ms: None,
            lookahead_frames: None,
            max_gain_db,
        }
    }

    /// 显式设置 attack（ms）。
    pub fn with_attack_ms(mut self, attack_ms: f64) -> Self {
        self.attack_ms = Some(attack_ms);
        self
    }

    /// 显式设置 lookahead（帧）。
    pub fn with_lookahead_frames(mut self, frames: u32) -> Self {
        self.lookahead_frames = Some(frames);
        self
    }

    /// 校验（结构性/安全边界；不冻结 DSP 公式）。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if !self.enabled {
            return Err(RtStatus::ERR_UNSUPPORTED);
        }
        if !self.threshold_db.is_finite() || self.threshold_db > 0.0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if !self.release_ms.is_finite() || self.release_ms <= 0.0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if let Some(attack_ms) = self.attack_ms {
            if !attack_ms.is_finite() || attack_ms <= 0.0 {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
        }
        if !self.max_gain_db.is_finite() || self.max_gain_db < 0.0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(())
    }

    /// 是否仍有 P14 未定参数（attack/lookahead 未配置）。
    pub fn unresolved_p14(&self) -> bool {
        self.attack_ms.is_none() || self.lookahead_frames.is_none()
    }
}

/// 每输出 clip/有限性保护配置（T22 的格式层 sanitize/clip 计数与本节不同层，不重复执行）。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipGuardConfig {
    /// 必须为 true（保护不得被 bypass）。
    pub enabled: bool,
    /// 输出 ceiling（dBFS，<= 0；显式配置，不发明默认值）。
    pub ceiling_db: f64,
    /// 是否计数非有限样本（计数动作由 RT 实现，配置显式）。
    pub count_nonfinite: bool,
    /// 是否计数削波样本。
    pub count_clip: bool,
}

impl ClipGuardConfig {
    /// 显式 ceiling 构造。
    pub fn new(ceiling_db: f64) -> Self {
        Self {
            enabled: true,
            ceiling_db,
            count_nonfinite: true,
            count_clip: true,
        }
    }

    /// 校验。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if !self.enabled {
            return Err(RtStatus::ERR_UNSUPPORTED);
        }
        if !self.ceiling_db.is_finite() || self.ceiling_db > 0.0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(())
    }
}

/// 启动/切换 ramp 保护配置（防爆音；ramp_frames > 0）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProtectionRampConfig {
    /// 必须为 true（保护不得被 bypass）。
    pub enabled: bool,
    /// 启动/切换 ramp 帧数（> 0）。
    pub ramp_frames: u32,
}

impl ProtectionRampConfig {
    /// 显式帧数构造。
    pub fn new(ramp_frames: u32) -> Self {
        Self {
            enabled: true,
            ramp_frames,
        }
    }

    /// 校验。
    pub fn validate(&self) -> Result<(), RtStatus> {
        if !self.enabled {
            return Err(RtStatus::ERR_UNSUPPORTED);
        }
        if self.ramp_frames == 0 {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        Ok(())
    }
}

/// 保护配置（limiter/clip/ramp 三件套；T38 后置输出保护必须全启用）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProtectionConfig {
    /// 每角色 limiter（长度必须 = n_role）。
    pub limiter: Vec<LimiterConfig>,
    /// clip/有限性保护（全局）。
    pub clip: ClipGuardConfig,
    /// 启动/切换 ramp（全局参数，按角色节点执行）。
    pub ramp: ProtectionRampConfig,
}

/// 图级 profile 配置（显式，无隐式默认）。
#[derive(Clone, Copy, Debug)]
pub struct GraphConfig {
    /// T12 编译选项（fs/B/max_taps/extended/预算）。
    pub options: CompileOptions,
    /// 设计增益帽（dB；P04；所有 role/trim 叠加后不得超过）。
    pub max_gain_db: f64,
    /// stereo strict 受限 profile（默认场景显式 true；见 §1）。
    pub stereo_strict: bool,
    /// stereo strict 时允许产生节目输出的角色集合（位图；显式来自 profile，不按名字）。
    pub allowed_role_mask: u16,
    /// 是否允许低带模态校正支援到非节目输出角色（默认 false；非默认 profile 才 true）。
    pub modal_support_to_other_outputs: bool,
    /// 非默认 profile 显式裁决（modal_support_to_other_outputs=true 时必须 true）。
    pub explicit_profile_approved: bool,
}

impl GraphConfig {
    /// 普通配置（stereo_strict=false、无 mask、无跨箱支援）。
    pub fn new(options: CompileOptions, max_gain_db: f64) -> Self {
        Self {
            options,
            max_gain_db,
            stereo_strict: false,
            allowed_role_mask: 0,
            modal_support_to_other_outputs: false,
            explicit_profile_approved: false,
        }
    }
}

/// 低带矩阵配置（T12 CompileInput.g 的配置载体；不含 delta_D baseline）。
#[derive(Clone, Debug, PartialEq)]
pub struct LowBandConfig {
    /// 低带 participants（角色索引，有序；对应 g 的 dense 行/列）。
    pub participants: Vec<u16>,
    /// g 系数（M × M × L_G，行主序；不含 delta_D）。
    pub g: Vec<f64>,
    /// G 声明支撑（>= d_samples + 1）。
    pub l_g: u32,
    /// 公共 baseline 整数延迟 D（样本；计入 L_G 支撑）。
    pub d_samples: u32,
}

impl LowBandConfig {
    /// M。
    pub fn m(&self) -> usize {
        self.participants.len()
    }
}

/// 完整节点图配置（配置驱动；所有字段显式来自 profile/校准/设计器产物）。
#[derive(Clone, Debug)]
pub struct NodeGraph {
    /// 输入源通道数 N。
    pub n_src: u16,
    /// 播放容器通道数 C（>= n_role）。
    pub container_channels: u16,
    /// 角色表（索引 = role_id；name 只做元数据）。
    pub roles: Vec<RoleConfig>,
    /// 输入侧 FIR 系数（N × L_A 行主序）。
    pub a: Vec<f64>,
    /// 输入侧声明支撑 L_A。
    pub l_a: u32,
    /// 低带矩阵配置。
    pub low_band: LowBandConfig,
    /// 每角色输出 FIR 链（长度 = n_role；空链 = identity）。
    pub output_fir: Vec<OutputFirChain>,
    /// 每角色模态 PEQ（长度 = n_role；空通道 = 无 IIR）。
    pub modal_iir: Vec<ModalIirChannel>,
    /// 全局 volume ramp。
    pub volume: VolumeRampConfig,
    /// 保护三件套。
    pub protection: ProtectionConfig,
    /// 图级 profile 配置。
    pub config: GraphConfig,
}

impl NodeGraph {
    /// 角色数 O。
    pub fn n_role(&self) -> usize {
        self.roles.len()
    }

    /// 校验图并返回报告（不生成计划投影）。
    pub fn validate(&self) -> Result<GraphReport, RtStatus> {
        Ok(self.lower()?.report)
    }

    /// 校验 + 合成 + T12 编译 + kernel/plan 投影（单入口；详见模块头）。
    pub fn lower(&self) -> Result<LoweredGraph, RtStatus> {
        let prepared = self.prepare()?;
        let plans = self.run_compile(&prepared)?;
        let (plan, kernel_status) = self.project_plan(&prepared, &plans)?;
        let report = self.build_report(&prepared, &plans, &plan, kernel_status);
        Ok(LoweredGraph {
            report,
            plan,
            plans,
        })
    }

    // -- 校验/准备 ------------------------------------------------------------

    fn prepare(&self) -> Result<Prepared, RtStatus> {
        let status = self.config.options.validate();
        if !status.is_ok() {
            return Err(status);
        }
        if !self.config.max_gain_db.is_finite() {
            return Err(RtStatus::ERR_ROLES);
        }
        if self.n_src == 0 || self.n_src as usize > MAX_INPUT_CHANNELS {
            return Err(RtStatus::ERR_CHANNELS);
        }
        let n_role = self.roles.len();
        if n_role == 0 || n_role > MAX_OUTPUT_ROLES {
            return Err(RtStatus::ERR_ROLES);
        }
        if self.container_channels < n_role as u16
            || self.container_channels == 0
            || self.container_channels as usize > MAX_OUTPUT_CHANNELS
        {
            return Err(RtStatus::ERR_CHANNELS);
        }

        let fs = self.config.options.sample_rate_hz as f64;
        let delay_limit = ROLE_DELAY_MAX_MS * fs / 1000.0;

        // 角色表：A_map 只选择/零填（每源至多一个角色）、scatter 单射、增益/延迟/极性范围。
        let mut source_used = [false; MAX_INPUT_CHANNELS];
        let mut container_used = [false; MAX_OUTPUT_CHANNELS];
        let mut gains = Vec::with_capacity(n_role);
        let mut delays = Vec::with_capacity(n_role);
        for role in &self.roles {
            if role.src != COMPILE_NO_SOURCE {
                let src = role.src as usize;
                if src >= self.n_src as usize || source_used[src] {
                    return Err(RtStatus::ERR_ROLES);
                }
                source_used[src] = true;
            }
            let out = role.out_index as usize;
            if out >= self.container_channels as usize || container_used[out] {
                return Err(RtStatus::ERR_ROLES);
            }
            container_used[out] = true;

            if !role.gain_db.is_finite()
                || role.gain_db < ROLE_GAIN_DB_MIN
                || role.gain_db > ROLE_GAIN_DB_MAX
            {
                return Err(RtStatus::ERR_ROLES);
            }
            if !role.trim_db.is_finite()
                || role.trim_db < ROLE_TRIM_DB_MIN
                || role.trim_db > ROLE_TRIM_DB_MAX
            {
                return Err(RtStatus::ERR_ROLES);
            }
            let gain = role.effective_gain_db();
            if !gain.is_finite() || gain > self.config.max_gain_db {
                return Err(RtStatus::ERR_ROLES);
            }
            if !role.delay_samples.is_finite()
                || role.delay_samples < 0.0
                || role.delay_samples > delay_limit
            {
                return Err(RtStatus::ERR_ROLES);
            }
            let delay = design_delay_to_plan(role.delay_samples)?;
            if role.polarity != 1 && role.polarity != -1 {
                return Err(RtStatus::ERR_ROLES);
            }
            if let Some(name) = role.name.as_ref() {
                if name.len() > ROLE_NAME_BYTES_MAX {
                    return Err(RtStatus::ERR_ROLES);
                }
            }
            gains.push(gain);
            delays.push(delay);
        }

        // 输入侧 FIR。
        if self.l_a == 0 || self.l_a > self.config.options.max_taps {
            return Err(RtStatus::ERR_TAPS);
        }
        let a_len = (self.n_src as usize)
            .checked_mul(self.l_a as usize)
            .ok_or(RtStatus::ERR_CAPACITY)?;
        if self.a.len() != a_len {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if self.a.iter().any(|v| !v.is_finite()) {
            return Err(RtStatus::ERR_PATH_TABLE);
        }

        // 低带 G。
        let m = self.low_band.m();
        if m == 0 || m > MAX_LOW_BAND_PARTICIPANTS || m > n_role {
            return Err(RtStatus::ERR_ROLES);
        }
        let mut role_seen = [false; MAX_OUTPUT_ROLES];
        for &role in &self.low_band.participants {
            let role = role as usize;
            if role >= n_role || role_seen[role] {
                return Err(RtStatus::ERR_ROLES);
            }
            role_seen[role] = true;
        }
        if self.low_band.l_g == 0
            || self.low_band.l_g > self.config.options.max_taps
            || self.low_band.d_samples >= self.low_band.l_g
        {
            return Err(RtStatus::ERR_TAPS);
        }
        let g_len = m
            .checked_mul(m)
            .and_then(|v| v.checked_mul(self.low_band.l_g as usize))
            .ok_or(RtStatus::ERR_CAPACITY)?;
        if self.low_band.g.len() != g_len {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        if self.low_band.g.iter().any(|v| !v.is_finite()) {
            return Err(RtStatus::ERR_PATH_TABLE);
        }

        // 输出 FIR 链。
        if self.output_fir.len() != n_role {
            return Err(RtStatus::ERR_ROLES);
        }
        let composed = self.compose_output_fir(&gains)?;

        // 模态 PEQ。
        if self.modal_iir.len() != n_role {
            return Err(RtStatus::ERR_ROLES);
        }
        let mut modal_requires_f64 = Vec::with_capacity(n_role);
        let mut modal_max_dev = Vec::with_capacity(n_role);
        for channel in &self.modal_iir {
            if channel.bands.len() > MODAL_BIQUADS_MAX {
                return Err(RtStatus::ERR_CAPACITY);
            }
            let mut requires = false;
            let mut max_dev: f64 = 0.0;
            for band in &channel.bands {
                band.validate(fs)?;
                let (db_dev, deg_dev) = band.self_consistency_deviation(fs);
                if db_dev > MODAL_CONSISTENCY_DB || deg_dev > MODAL_CONSISTENCY_DEG {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
                let dev = band.f32_quantization_max_deviation_db(fs);
                if dev > max_dev {
                    max_dev = dev;
                }
                if dev > F32_QUANTIZATION_TOL_DB {
                    requires = true;
                }
            }
            modal_requires_f64.push(requires);
            modal_max_dev.push(max_dev);
        }

        // volume ramp。
        self.volume.validate()?;

        // 保护三件套：存在 + enabled + 结构校验（不可 bypass）。
        if self.protection.limiter.len() != n_role {
            return Err(RtStatus::ERR_ROLES);
        }
        for limiter in &self.protection.limiter {
            limiter.validate()?;
        }
        self.protection.clip.validate()?;
        self.protection.ramp.validate()?;

        // stereo strict 受限 profile（不按 role 名，全部用显式 mask/源选择）。
        self.validate_stereo_strict(&composed)?;

        // 节点图与节点数上限。
        let nodes = self.build_node_chain(&delays);
        if nodes.len() > MAX_PLAN_NODES {
            return Err(RtStatus::ERR_CAPACITY);
        }

        Ok(Prepared {
            gains,
            delays,
            composed,
            modal_requires_f64,
            modal_max_dev,
            nodes,
        })
    }

    /// stereo strict / 跨箱支援校验（core-v1 §11.10 flags、DSP-HOST-DECISION §1）。
    fn validate_stereo_strict(&self, composed: &ComposedOutput) -> Result<(), RtStatus> {
        if self.config.modal_support_to_other_outputs && !self.config.explicit_profile_approved {
            return Err(RtStatus::ERR_ROLES);
        }
        if !self.config.stereo_strict {
            return Ok(());
        }
        let n_role = self.roles.len();
        let mask = self.config.allowed_role_mask;
        if mask == 0 {
            return Err(RtStatus::ERR_ROLES);
        }
        if n_role < 16 && (mask >> n_role) != 0 {
            return Err(RtStatus::ERR_ROLES);
        }
        for (index, role) in self.roles.iter().enumerate() {
            let allowed = (mask & (1 << index)) != 0;
            if role.src != COMPILE_NO_SOURCE && !allowed {
                return Err(RtStatus::ERR_ROLES);
            }
            if allowed && role.src == COMPILE_NO_SOURCE {
                return Err(RtStatus::ERR_ROLES);
            }
            if allowed {
                continue;
            }
            // 非允许输出角色：必须是显式零填 identity（不得用 coeff 造输出）。
            if composed.rows[index].len() != 1 || composed.rows[index][0] != 1.0 {
                return Err(RtStatus::ERR_ROLES);
            }
            if composed.delays[index] != DelayPlan::ZERO {
                return Err(RtStatus::ERR_ROLES);
            }
            if !self.modal_iir[index].is_empty() {
                return Err(RtStatus::ERR_ROLES);
            }
            if self.roles[index].effective_gain_db() != 0.0 {
                return Err(RtStatus::ERR_ROLES);
            }
            if self.roles[index].delay_samples != 0.0 {
                return Err(RtStatus::ERR_ROLES);
            }
        }
        if !self.config.modal_support_to_other_outputs {
            // 低带 G 不得向非允许角色生成输出（禁止 runtime mask 破坏预测）。
            for &role in &self.low_band.participants {
                if (mask & (1 << role)) == 0 {
                    return Err(RtStatus::ERR_ROLES);
                }
            }
        }
        Ok(())
    }

    /// 输出 FIR 链合成（规范序线性卷积；gain/polarity 折叠；空链 = identity）。
    fn compose_output_fir(&self, gains: &[f64]) -> Result<ComposedOutput, RtStatus> {
        let n_role = self.roles.len();
        let mut rows = Vec::with_capacity(n_role);
        let mut supports = Vec::with_capacity(n_role);
        let mut flags = Vec::with_capacity(n_role);
        let mut delays = Vec::with_capacity(n_role);
        let mut l_p: u32 = 1;
        for (index, chain) in self.output_fir.iter().enumerate() {
            if chain.segments.len() > OUTPUT_FIR_SEGMENTS_MAX {
                return Err(RtStatus::ERR_CAPACITY);
            }
            let mut coefficients = vec![1.0f64];
            let mut support: u32 = 1;
            let mut delay_q16_total: u64 = 0;
            let mut diag_flags: u16 = 0;
            let mut last_rank: Option<u8> = None;
            for segment in &chain.segments {
                if segment.taps == 0 || segment.taps > self.config.options.max_taps {
                    return Err(RtStatus::ERR_TAPS);
                }
                if segment.coefficients.len() != segment.taps as usize {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
                if segment.coefficients.iter().any(|v| !v.is_finite()) {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
                if segment.delay_q16 > 65_535 {
                    return Err(RtStatus::ERR_TAPS);
                }
                let total = segment.total_delay_q16();
                if total >= (segment.taps as u64) << 16 {
                    return Err(RtStatus::ERR_TAPS);
                }
                if let Some(rank) = last_rank {
                    if segment.kind.rank() < rank {
                        return Err(RtStatus::ERR_PATH_TABLE);
                    }
                }
                last_rank = Some(segment.kind.rank());
                diag_flags |= segment.kind.diag_flag();
                coefficients = convolve_linear(&coefficients, &segment.coefficients);
                support = coefficients.len() as u32;
                delay_q16_total = delay_q16_total.saturating_add(total);
                if delay_q16_total >= (support as u64) << 16 {
                    return Err(RtStatus::ERR_TAPS);
                }
            }
            if support > self.config.options.max_taps {
                return Err(RtStatus::ERR_TAPS);
            }
            // 单次应用：有效增益折叠进系数（线性缩放精确）；极性折叠为符号。
            let gain = gains[index];
            if gain != 0.0 {
                let linear = db_to_linear(gain);
                for coefficient in coefficients.iter_mut() {
                    *coefficient *= linear;
                }
            }
            if self.roles[index].polarity == -1 {
                for coefficient in coefficients.iter_mut() {
                    *coefficient = -*coefficient;
                }
            }
            if coefficients.iter().any(|v| !v.is_finite()) {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
            if support > l_p {
                l_p = support;
            }
            supports.push(support);
            flags.push(diag_flags);
            delays.push(DelayPlan {
                delay_samples: (delay_q16_total >> 16) as u32,
                delay_q16: (delay_q16_total & 0xFFFF) as u32,
            });
            rows.push(coefficients);
        }
        Ok(ComposedOutput {
            rows,
            supports,
            flags,
            delays,
            l_p,
        })
    }

    // -- 编译 / 投影 ----------------------------------------------------------

    fn run_compile(&self, prepared: &Prepared) -> Result<CompiledPlans, RtStatus> {
        let n_role = self.roles.len();
        let l_p = prepared.composed.l_p as usize;
        let mut p = vec![0.0f64; n_role * l_p];
        for (index, row) in prepared.composed.rows.iter().enumerate() {
            p[index * l_p..index * l_p + row.len()].copy_from_slice(row);
        }
        let src_of_role: Vec<u16> = self.roles.iter().map(|role| role.src).collect();
        let scatter: Vec<u16> = self.roles.iter().map(|role| role.out_index).collect();
        let input = CompileInput {
            n_src: self.n_src,
            n_role: n_role as u16,
            container_channels: self.container_channels,
            d_samples: self.low_band.d_samples,
            participants: &self.low_band.participants,
            src_of_role: &src_of_role,
            scatter: &scatter,
            a: &self.a,
            l_a: self.l_a,
            g: &self.low_band.g,
            l_g: self.low_band.l_g,
            p: &p,
            l_p: prepared.composed.l_p,
        };
        compile(&input, &self.config.options)
    }

    fn project_plan(
        &self,
        prepared: &Prepared,
        plans: &CompiledPlans,
    ) -> Result<(NodePlan, Option<RtStatus>), RtStatus> {
        let n_role = self.roles.len();
        let topology = self.build_topology(prepared)?;
        let kernel_error = if topology.is_none() {
            Some(RtStatus::ERR_TAPS)
        } else {
            None
        };
        let coeffs = match topology.as_ref() {
            Some(_) => Some(self.build_kernel_coeffs(prepared)?),
            None => None,
        };

        // plan 投影：diags 按 out_index 升序（core-v1 §11.10），roles 按 role_id 升序。
        let mut order: Vec<usize> = (0..n_role).collect();
        order.sort_by_key(|&index| self.roles[index].out_index);
        let mut diags = Vec::with_capacity(n_role);
        for &index in &order {
            let role = &self.roles[index];
            let composed = &prepared.composed;
            let mut coeffs_f32 = vec![0.0f32; composed.l_p as usize];
            for (tap, &value) in composed.rows[index].iter().enumerate() {
                let quantized = value as f32;
                if !quantized.is_finite() {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
                coeffs_f32[tap] = quantized;
            }
            let delay = composed.delays[index];
            diags.push(DiagFirPlan {
                out_index: role.out_index,
                flags: composed.flags[index],
                taps: composed.l_p,
                support_taps: composed.supports[index],
                delay_samples: delay.delay_samples,
                delay_q16: delay.delay_q16,
                coeffs_f32,
            });
        }

        let mut roles = Vec::with_capacity(n_role);
        for (index, role) in self.roles.iter().enumerate() {
            let mut flags = ROLE_FLAG_ENABLED;
            if role.is_zero_fill() {
                flags |= ROLE_FLAG_ZERO_FILL;
            }
            let name_hash = match role.name.as_ref() {
                Some(name) => crc32c(name.as_bytes()),
                None => 0,
            };
            let delay = prepared.delays[index];
            roles.push(RolePlan {
                role_id: index as u16,
                out_index: role.out_index,
                flags,
                delay_samples: delay.delay_samples,
                delay_q16: delay.delay_q16,
                gain_q15: i16::MAX,
                polarity: 1,
                effective_gain_db: prepared.gains[index],
                peq_count: self.modal_iir[index].band_count() as u16,
                name_hash,
            });
        }

        let mut iir = Vec::new();
        let fs = self.config.options.sample_rate_hz as f64;
        for (index, channel) in self.modal_iir.iter().enumerate() {
            if channel.is_empty() {
                continue;
            }
            let mut biquads = Vec::with_capacity(channel.bands.len());
            for band in &channel.bands {
                let b_f32 = coeffs_to_f32(&band.b);
                let a_f32 = coeffs_to_f32(&band.a);
                biquads.push(BiquadPlan {
                    band_type: band.band_type,
                    freq_hz: band.freq_hz,
                    q: band.q,
                    gain_db: band.gain_db,
                    b: band.b,
                    a: band.a,
                    b_f32,
                    a_f32,
                    requires_f64: band.requires_f64(fs),
                });
            }
            iir.push(IirStagePlan {
                role: index as u16,
                out_index: self.roles[index].out_index,
                bands: biquads,
            });
        }

        let volume = VolumePlan {
            volume_db: self.volume.volume_db,
            user_mute: self.volume.user_mute,
            ramp_frames: self.volume.ramp_frames,
            target_linear_gain: self.volume.target_linear_gain(),
            gain_db_min: VOLUME_DB_MIN,
            gain_db_max: VOLUME_DB_MAX,
        };
        let mut limiters = Vec::with_capacity(n_role);
        let protection_unresolved_p14 = self
            .protection
            .limiter
            .iter()
            .any(LimiterConfig::unresolved_p14);
        for (index, limiter) in self.protection.limiter.iter().enumerate() {
            limiters.push(LimiterPlan {
                role: index as u16,
                out_index: self.roles[index].out_index,
                enabled: limiter.enabled,
                threshold_db: limiter.threshold_db,
                release_ms: limiter.release_ms,
                attack_ms: limiter.attack_ms,
                lookahead_frames: limiter.lookahead_frames,
                max_gain_db: limiter.max_gain_db,
                unresolved_p14: limiter.unresolved_p14(),
            });
        }
        let protection = ProtectionPlan {
            limiters,
            clip: self.protection.clip.clone(),
            ramp: self.protection.ramp.clone(),
            insertion: "post_role_sum_pre_scatter",
            unresolved_p14: protection_unresolved_p14,
        };

        let runtime_stages = self.build_runtime_stages(prepared);
        let mut zero_fill_roles = Vec::new();
        for (index, role) in self.roles.iter().enumerate() {
            if role.is_zero_fill() {
                zero_fill_roles.push(index as u16);
            }
        }
        let scatter: Vec<u16> = self.roles.iter().map(|role| role.out_index).collect();
        let zero_fill_containers = scatter_zero_fill_containers(&scatter, self.container_channels);

        let plan = NodePlan {
            nodes: prepared.nodes.clone(),
            scatter,
            zero_fill_roles,
            zero_fill_containers,
            diags,
            roles,
            iir,
            volume,
            protection,
            runtime_stages,
            topology,
            coeffs,
            l_a: self.l_a,
            l_g: self.low_band.l_g,
            l_p: prepared.composed.l_p,
            selected: plans.report.chosen,
            input_stages: plans.factorized.input_stage_plans(),
            matrix_stages: plans.factorized.matrix_stage_plans(),
            output_stages: plans.factorized.output_stage_plans(),
            fused_stages: plans
                .fused
                .as_ref()
                .map(|fused| fused.plan_paths())
                .unwrap_or_default(),
        };
        Ok((plan, kernel_error))
    }

    fn build_topology(&self, prepared: &Prepared) -> Result<Option<DirectTopology>, RtStatus> {
        let n_role = self.roles.len();
        let l_a = u16::try_from(self.l_a).map_err(|_| RtStatus::ERR_TAPS)?;
        let l_g = u16::try_from(self.low_band.l_g).map_err(|_| RtStatus::ERR_TAPS)?;
        let l_p = u16::try_from(prepared.composed.l_p).map_err(|_| RtStatus::ERR_TAPS)?;
        let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
        let mut scatter = [0u16; DIRECT_MAX_ROLES];
        for (index, role) in self.roles.iter().enumerate() {
            src_of_role[index] = role.src;
            scatter[index] = role.out_index;
        }
        let mut participants = [0u16; DIRECT_MAX_PARTICIPANTS];
        for (index, &role) in self.low_band.participants.iter().enumerate() {
            participants[index] = role;
        }
        let topology = DirectTopology {
            n_src: self.n_src,
            n_role: n_role as u16,
            container_channels: self.container_channels,
            d_samples: self.low_band.d_samples,
            l_a,
            l_g,
            l_p,
            m: self.low_band.m() as u16,
            src_of_role,
            participants,
            scatter,
        };
        let path_len =
            self.l_a as u64 + self.low_band.l_g as u64 + prepared.composed.l_p as u64 - 2;
        let status = topology.validate();
        if status.is_ok() {
            Ok(Some(topology))
        } else if status == RtStatus::ERR_TAPS && path_len > DIRECT_MAX_TAPS as u64 {
            Ok(None)
        } else {
            Err(status)
        }
    }

    fn build_kernel_coeffs(&self, prepared: &Prepared) -> Result<KernelCoeffsF32, RtStatus> {
        let n_role = self.roles.len();
        let l_a = self.l_a as usize;
        let l_g = self.low_band.l_g as usize;
        let l_p = prepared.composed.l_p as usize;
        let mut a = Vec::with_capacity(self.n_src as usize * l_a);
        for &value in &self.a {
            let value = value as f32;
            if !value.is_finite() {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
            a.push(value);
        }
        let mut g = Vec::with_capacity(self.low_band.m() * self.low_band.m() * l_g);
        for &value in &self.low_band.g {
            let value = value as f32;
            if !value.is_finite() {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
            g.push(value);
        }
        let mut p = vec![0.0f32; n_role * l_p];
        for (index, row) in prepared.composed.rows.iter().enumerate() {
            for (tap, &value) in row.iter().enumerate() {
                let value = value as f32;
                if !value.is_finite() {
                    return Err(RtStatus::ERR_PATH_TABLE);
                }
                p[index * l_p + tap] = value;
            }
        }
        Ok(KernelCoeffsF32 { a, g, p })
    }

    fn build_runtime_stages(&self, prepared: &Prepared) -> Vec<RuntimeStage> {
        let mut stages = Vec::new();
        for (index, channel) in self.modal_iir.iter().enumerate() {
            if !channel.is_empty() {
                stages.push(RuntimeStage::ModalIir(index as u16));
            }
        }
        for (index, delay) in prepared.delays.iter().enumerate() {
            if delay.total_delay_q16() != 0 {
                stages.push(RuntimeStage::RoleDelay(index as u16));
            }
        }
        stages.push(RuntimeStage::VolumeRamp);
        for index in 0..self.roles.len() {
            stages.push(RuntimeStage::Limiter(index as u16));
        }
        for index in 0..self.roles.len() {
            stages.push(RuntimeStage::ClipGuard(index as u16));
        }
        for index in 0..self.roles.len() {
            stages.push(RuntimeStage::ProtectionRamp(index as u16));
        }
        stages.push(RuntimeStage::Scatter);
        stages
    }

    fn build_node_chain(&self, delays: &[DelayPlan]) -> Vec<PlanNode> {
        let mut nodes = Vec::new();
        nodes.push(PlanNode {
            kind: NodeKind::InputSelect,
            role: None,
        });
        nodes.push(PlanNode {
            kind: NodeKind::LowBandMatrix,
            role: None,
        });
        for index in 0..self.roles.len() {
            nodes.push(PlanNode {
                kind: NodeKind::OutputFir,
                role: Some(index as u16),
            });
        }
        for (index, channel) in self.modal_iir.iter().enumerate() {
            if !channel.is_empty() {
                nodes.push(PlanNode {
                    kind: NodeKind::ModalIir,
                    role: Some(index as u16),
                });
            }
        }
        for (index, delay) in delays.iter().enumerate() {
            if delay.total_delay_q16() != 0 {
                nodes.push(PlanNode {
                    kind: NodeKind::Delay,
                    role: Some(index as u16),
                });
            }
        }
        nodes.push(PlanNode {
            kind: NodeKind::VolumeRamp,
            role: None,
        });
        for index in 0..self.roles.len() {
            nodes.push(PlanNode {
                kind: NodeKind::Limiter,
                role: Some(index as u16),
            });
        }
        for index in 0..self.roles.len() {
            nodes.push(PlanNode {
                kind: NodeKind::ClipGuard,
                role: Some(index as u16),
            });
        }
        for index in 0..self.roles.len() {
            nodes.push(PlanNode {
                kind: NodeKind::ProtectionRamp,
                role: Some(index as u16),
            });
        }
        nodes.push(PlanNode {
            kind: NodeKind::Scatter,
            role: None,
        });
        nodes
    }

    fn build_report(
        &self,
        prepared: &Prepared,
        plans: &CompiledPlans,
        plan: &NodePlan,
        kernel_error: Option<RtStatus>,
    ) -> GraphReport {
        let modal_f32_admissible = !prepared.modal_requires_f64.iter().any(|v| *v);
        let kernel_admissible = plan.topology.is_some();
        let runtime_f32_admissible = modal_f32_admissible && kernel_admissible;
        let mut total_delay_q16 = Vec::with_capacity(self.roles.len());
        for (index, delay) in prepared.delays.iter().enumerate() {
            let composed = &prepared.composed.delays[index];
            total_delay_q16.push(
                delay
                    .total_delay_q16()
                    .saturating_add(composed.total_delay_q16()),
            );
        }
        let mut warnings: Vec<&'static str> = Vec::new();
        warnings.push(
            "T23: role gain/polarity 折叠进输出对角 FIR（单次应用，不双乘）；RolePlan.gain_q15/polarity 为 identity",
        );
        warnings
            .push("T23: 模态 IIR 在 kernel 单 P FIR 之后执行（与目标 FIR 为 LTI 交换，f64 验证）");
        if !modal_f32_admissible {
            warnings
                .push("T23: 存在 requires_f64 的模态段，v1 f32 运行期不得加载（ERR_PRECISION）");
        }
        if !kernel_admissible {
            warnings.push("T23: extended taps 超出当前 direct/partitioned kernel 4096 上限（kernel_admissible=false）");
        }
        if plan.protection.unresolved_p14 {
            warnings.push(
                "P14: limiter attack/lookahead 未配置（未定项，不发明默认值；需 T02/T22+用户裁决）",
            );
        }
        if self.config.modal_support_to_other_outputs {
            warnings.push("T23: modal_support_to_other_outputs=true 属非默认 profile（须独立求解/应用/holdout）");
        }
        let coeff_bytes_f32 = (self.a.len()
            + self.low_band.g.len()
            + self.roles.len() * prepared.composed.l_p as usize)
            as u64
            * 4;
        GraphReport {
            n_src: self.n_src,
            n_role: self.roles.len() as u16,
            container_channels: self.container_channels,
            node_count: plan.nodes.len() as u16,
            zero_fill_roles: plan.zero_fill_roles.clone(),
            zero_fill_containers: plan.zero_fill_containers.clone(),
            output_fir_support: prepared.composed.supports.clone(),
            output_fir_delay_q16: prepared
                .composed
                .delays
                .iter()
                .map(|delay| delay.total_delay_q16())
                .collect(),
            role_delay_q16: prepared
                .delays
                .iter()
                .map(|delay| delay.total_delay_q16())
                .collect(),
            total_delay_q16,
            role_gain_db: prepared.gains.clone(),
            role_polarity: self.roles.iter().map(|role| role.polarity).collect(),
            modal_biquads: self
                .modal_iir
                .iter()
                .map(|channel| channel.band_count() as u16)
                .collect(),
            modal_requires_f64: prepared.modal_requires_f64.clone(),
            modal_f32_max_deviation_db: prepared.modal_max_dev.clone(),
            modal_f32_admissible,
            volume_db: self.volume.volume_db,
            user_mute: self.volume.user_mute,
            volume_ramp_frames: self.volume.ramp_frames,
            protection_roles: self.protection.limiter.len() as u16,
            protection_unresolved_p14: plan.protection.unresolved_p14,
            kernel_admissible,
            kernel_error,
            runtime_f32_admissible,
            l_a: self.l_a,
            l_g: self.low_band.l_g,
            l_p: prepared.composed.l_p,
            coeff_bytes_f32,
            compile: plans.report,
            warnings,
            sample_rate_hz: self.config.options.sample_rate_hz,
            max_gain_db: self.config.max_gain_db,
            stereo_strict: self.config.stereo_strict,
            allowed_role_mask: self.config.allowed_role_mask,
            modal_support_to_other_outputs: self.config.modal_support_to_other_outputs,
        }
    }
}

// ---------------------------------------------------------------------------
// 内部准备产物
// ---------------------------------------------------------------------------

struct Prepared {
    gains: Vec<f64>,
    delays: Vec<DelayPlan>,
    composed: ComposedOutput,
    modal_requires_f64: Vec<bool>,
    modal_max_dev: Vec<f64>,
    nodes: Vec<PlanNode>,
}

struct ComposedOutput {
    rows: Vec<Vec<f64>>,
    supports: Vec<u32>,
    flags: Vec<u16>,
    delays: Vec<DelayPlan>,
    l_p: u32,
}

// ---------------------------------------------------------------------------
// 计划投影（NONRT；字段对齐 core-v1 §11.10 v1.1，含 delay_q16；ABI 打包归 T26/T66）
// ---------------------------------------------------------------------------

/// 计划中的逻辑节点（role=None 为全局节点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanNode {
    /// 节点类型。
    pub kind: NodeKind,
    /// 关联角色（全局节点 None）。
    pub role: Option<u16>,
}

/// 输出对角 FIR 计划投影（KhaenDiagFirV1 语义 + v1.1 delay_q16）。
#[derive(Clone, Debug, PartialEq)]
pub struct DiagFirPlan {
    /// 容器物理输出索引（= role.out_index；按升序排列）。
    pub out_index: u16,
    /// KhaenDiagFirV1.flags（本模块只置 crossover/target/phase 位）。
    pub flags: u16,
    /// 声明支撑（= plan 公共 L_P；系数已零填到该长度）。
    pub taps: u32,
    /// 本角色实际合成支撑（报告用；<= taps）。
    pub support_taps: u32,
    /// 整数延迟 D_int（含在支撑内）。
    pub delay_samples: u32,
    /// Q16 次样本延迟（0..=65535）。
    pub delay_q16: u32,
    /// f32 系数（长度 = taps；kernel/plan 形态）。
    pub coeffs_f32: Vec<f32>,
}

/// role 计划投影（KhaenRoleV1 语义 + v1.1 delay_q16；gain/polarity 已折叠）。
#[derive(Clone, Debug, PartialEq)]
pub struct RolePlan {
    /// role_id（= 配置索引）。
    pub role_id: u16,
    /// 容器输出索引。
    pub out_index: u16,
    /// KhaenRoleV1.flags（enabled/zero_fill）。
    pub flags: u16,
    /// 角色级整数延迟。
    pub delay_samples: u32,
    /// 角色级 Q16 延迟。
    pub delay_q16: u32,
    /// Q15 线性增益（identity = i16::MAX；有效增益已折叠进对角 FIR）。
    pub gain_q15: i16,
    /// 极性（identity = +1；极性已折叠进对角 FIR）。
    pub polarity: i8,
    /// 有效设计增益（报告；已折叠）。
    pub effective_gain_db: f64,
    /// 模态 PEQ 段数。
    pub peq_count: u16,
    /// role 名 CRC-32C（无名字 = 0）。
    pub name_hash: u32,
}

/// 单个 biquad 计划（f64 设计 + f32 运行系数 + requires_f64）。
#[derive(Clone, Debug, PartialEq)]
pub struct BiquadPlan {
    /// band 类型。
    pub band_type: ModalBandType,
    /// 频率（Hz）。
    pub freq_hz: f64,
    /// peaking Q / shelf S。
    pub q: f64,
    /// 增益（dB）。
    pub gain_db: f64,
    /// f64 系数（设计/报告）。
    pub b: [f64; 3],
    /// f64 系数。
    pub a: [f64; 3],
    /// f32 运行系数（a0=1；requires_f64 时不得加载）。
    pub b_f32: [f32; 3],
    /// f32 运行系数。
    pub a_f32: [f32; 3],
    /// §5.4：f32 量化偏差 > 0.1 dB -> 不得用 f32 运行路径。
    pub requires_f64: bool,
}

/// 每角色模态 IIR 计划。
#[derive(Clone, Debug, PartialEq)]
pub struct IirStagePlan {
    /// role 索引。
    pub role: u16,
    /// 容器输出索引。
    pub out_index: u16,
    /// biquad 段（有序；每段独立状态）。
    pub bands: Vec<BiquadPlan>,
}

/// volume ramp 计划。
#[derive(Clone, Debug, PartialEq)]
pub struct VolumePlan {
    /// 目标音量（dB）。
    pub volume_db: f64,
    /// 用户 mute（与保护静音区分）。
    pub user_mute: bool,
    /// ramp 帧数（0 = 立即）。
    pub ramp_frames: u32,
    /// 目标线性增益（mute 时 0）。
    pub target_linear_gain: f64,
    /// 量程下限（-150 dB）。
    pub gain_db_min: f64,
    /// 量程上限（+50 dB）。
    pub gain_db_max: f64,
}

/// 每输出 limiter 计划。
#[derive(Clone, Debug, PartialEq)]
pub struct LimiterPlan {
    /// role 索引。
    pub role: u16,
    /// 容器输出索引。
    pub out_index: u16,
    /// 启用（恒 true；bypass 被拒收）。
    pub enabled: bool,
    /// 阈值（dBFS）。
    pub threshold_db: f64,
    /// release（ms）。
    pub release_ms: f64,
    /// attack（ms；None = P14 未定）。
    pub attack_ms: Option<f64>,
    /// lookahead（帧；None = P14 未定）。
    pub lookahead_frames: Option<u32>,
    /// 最大增益帽（dB）。
    pub max_gain_db: f64,
    /// 是否含 P14 未定参数。
    pub unresolved_p14: bool,
}

/// 保护计划（limiter/clip/ramp 三件套；不可绕过、scatter 之前）。
#[derive(Clone, Debug, PartialEq)]
pub struct ProtectionPlan {
    /// 每角色 limiter。
    pub limiters: Vec<LimiterPlan>,
    /// clip/有限性保护。
    pub clip: ClipGuardConfig,
    /// 启动/切换 ramp。
    pub ramp: ProtectionRampConfig,
    /// 插入点（compat-matrix P03：角色求和后、scatter 前）。
    pub insertion: &'static str,
    /// 是否含 P14 未定参数。
    pub unresolved_p14: bool,
}

/// kernel f32 系数（A/G/P 行主序；布局与 DirectCoeffsF32 一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct KernelCoeffsF32 {
    /// 输入侧 FIR（N × L_A）。
    pub a: Vec<f32>,
    /// 低带 g（M × M × L_G，不含 delta_D）。
    pub g: Vec<f32>,
    /// 输出对角 FIR（O × L_P；后续角色零填）。
    pub p: Vec<f32>,
}

/// 完整 plan 投影（NONRT；T26 打包/RT 执行的数据源）。
#[derive(Clone, Debug)]
pub struct NodePlan {
    /// 逻辑节点链（规范序）。
    pub nodes: Vec<PlanNode>,
    /// role → 容器通道 scatter。
    pub scatter: Vec<u16>,
    /// 无源角色（显式零填）。
    pub zero_fill_roles: Vec<u16>,
    /// 未被 scatter 使用的容器通道（kernel 逐样本 +0.0）。
    pub zero_fill_containers: Vec<u16>,
    /// 输出对角 FIR（按 out_index 升序）。
    pub diags: Vec<DiagFirPlan>,
    /// role 表（按 role_id 升序）。
    pub roles: Vec<RolePlan>,
    /// 模态 IIR（仅非空通道；role 升序）。
    pub iir: Vec<IirStagePlan>,
    /// volume ramp。
    pub volume: VolumePlan,
    /// 保护三件套。
    pub protection: ProtectionPlan,
    /// kernel 之后的运行阶段（规范顺序）。
    pub runtime_stages: Vec<RuntimeStage>,
    /// kernel 拓扑（超 4096 taps extended 图为 None）。
    pub topology: Option<DirectTopology>,
    /// kernel f32 系数（仅 kernel_admissible 时 Some）。
    pub coeffs: Option<KernelCoeffsF32>,
    /// 输入侧声明支撑 L_A。
    pub l_a: u32,
    /// G 声明支撑 L_G。
    pub l_g: u32,
    /// 输出声明支撑 L_P（公共）。
    pub l_p: u32,
    /// T12 选择的图形态。
    pub selected: FusionForm,
    /// T12 factorized 输入阶段计划。
    pub input_stages: Vec<StagePlan>,
    /// T12 factorized 矩阵阶段计划。
    pub matrix_stages: Vec<MatrixStagePlan>,
    /// T12 factorized 输出阶段计划。
    pub output_stages: Vec<StagePlan>,
    /// T12 fused 路径计划（不可行时为空）。
    pub fused_stages: Vec<FusedPathPlan>,
}

impl NodePlan {
    /// 节点数。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// kernel 拓扑是否可执行（direct/partitioned 当前 4096 taps 上限内）。
    pub fn kernel_admissible(&self) -> bool {
        self.topology.is_some()
    }

    /// kernel 系数视图（不可执行时 None）。
    pub fn direct_coeffs(&self) -> Option<DirectCoeffsF32<'_>> {
        self.coeffs.as_ref().map(|coeffs| DirectCoeffsF32 {
            a_fir: &coeffs.a,
            g: &coeffs.g,
            p: &coeffs.p,
        })
    }

    /// 是否存在 requires_f64 的模态段（v1 f32 运行期不得加载）。
    pub fn requires_f64(&self) -> bool {
        self.iir
            .iter()
            .any(|stage| stage.bands.iter().any(|band| band.requires_f64))
    }

    /// scatter 的容器零填列表（与字段一致，便于调用方复核）。
    pub fn container_zero_fill(&self, container_channels: u16) -> Vec<u16> {
        let mut used = vec![false; container_channels as usize];
        for &out in &self.scatter {
            used[out as usize] = true;
        }
        used.iter()
            .enumerate()
            .filter(|(_, used)| !**used)
            .map(|(index, _)| index as u16)
            .collect()
    }
}

/// 校验+投影产物。
#[derive(Clone, Debug)]
pub struct LoweredGraph {
    /// 报告。
    pub report: GraphReport,
    /// plan 投影。
    pub plan: NodePlan,
    /// T12 编译结果（factorized/fused + 成本/选择）。
    pub plans: CompiledPlans,
}

impl LoweredGraph {
    /// T12 选择的图形态。
    pub fn selected_form(&self) -> FusionForm {
        self.plans.report.chosen
    }

    /// v1 f32 运行期是否可准入（无 requires_f64 且 kernel 可执行）。
    pub fn runtime_f32_admissible(&self) -> bool {
        self.report.runtime_f32_admissible
    }

    /// v1 f32 运行期准入检查：requires_f64 -> ERR_PRECISION；kernel 不可执行 -> ERR_TAPS。
    pub fn validate_runtime_f32(&self) -> Result<(), RtStatus> {
        if !self.report.modal_f32_admissible {
            return Err(RtStatus::ERR_PRECISION);
        }
        if !self.report.kernel_admissible {
            return Err(RtStatus::ERR_TAPS);
        }
        Ok(())
    }
}

/// 图级报告（准备/校验/投影事实；T26/T55/T56 消费）。
#[derive(Clone, Debug)]
pub struct GraphReport {
    /// 输入源数 N。
    pub n_src: u16,
    /// 角色数 O。
    pub n_role: u16,
    /// 容器通道数 C。
    pub container_channels: u16,
    /// 节点数。
    pub node_count: u16,
    /// 无源角色（显式零填）。
    pub zero_fill_roles: Vec<u16>,
    /// 未用容器通道（显式 +0.0）。
    pub zero_fill_containers: Vec<u16>,
    /// 每角色输出 FIR 实际合成支撑。
    pub output_fir_support: Vec<u32>,
    /// 每角色输出 FIR 声明延迟（Q16）。
    pub output_fir_delay_q16: Vec<u64>,
    /// 每角色 role 级延迟（Q16）。
    pub role_delay_q16: Vec<u64>,
    /// 每角色总延迟（FIR + role，Q16）。
    pub total_delay_q16: Vec<u64>,
    /// 每角色有效设计增益（dB；已折叠进 FIR）。
    pub role_gain_db: Vec<f64>,
    /// 每角色极性（已折叠进 FIR）。
    pub role_polarity: Vec<i8>,
    /// 每角色 biquad 段数。
    pub modal_biquads: Vec<u16>,
    /// 每角色是否需要 f64（任一 band requires_f64）。
    pub modal_requires_f64: Vec<bool>,
    /// 每角色 f32 最大响应偏差（dB）。
    pub modal_f32_max_deviation_db: Vec<f64>,
    /// 全部模态段是否可用 f32 运行。
    pub modal_f32_admissible: bool,
    /// 目标音量（dB）。
    pub volume_db: f64,
    /// 用户 mute。
    pub user_mute: bool,
    /// ramp 帧数。
    pub volume_ramp_frames: u32,
    /// 受保护角色数。
    pub protection_roles: u16,
    /// 是否有 P14 未定参数。
    pub protection_unresolved_p14: bool,
    /// kernel 是否可执行。
    pub kernel_admissible: bool,
    /// kernel 不可执行原因。
    pub kernel_error: Option<RtStatus>,
    /// v1 f32 运行期是否可准入。
    pub runtime_f32_admissible: bool,
    /// L_A。
    pub l_a: u32,
    /// L_G。
    pub l_g: u32,
    /// L_P。
    pub l_p: u32,
    /// f32 系数字节数（a+g+p，含角色零填）。
    pub coeff_bytes_f32: u64,
    /// T12 编译报告。
    pub compile: CompileReport,
    /// 静态警告（不静默隐藏的近似/未定项/交换说明）。
    pub warnings: Vec<&'static str>,
    /// 采样率（Hz）。
    pub sample_rate_hz: u32,
    /// 设计增益帽（dB）。
    pub max_gain_db: f64,
    /// stereo strict 标志。
    pub stereo_strict: bool,
    /// 允许输出角色掩码。
    pub allowed_role_mask: u16,
    /// 跨箱模态支援标志。
    pub modal_support_to_other_outputs: bool,
}

// ---------------------------------------------------------------------------
// 通用数学/参考辅助（offline；f64；非 RT 入口）
// ---------------------------------------------------------------------------

/// dB -> 线性增益。
pub fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// 线性增益 -> dB（0 及负值返回 -inf，不做静默钳位）。
pub fn linear_to_db(linear: f64) -> f64 {
    20.0 * linear.log10()
}

/// volume ramp 每帧 dB 步长（ramp_frames=0 -> 直接到目标，返回 0）。
pub fn volume_ramp_db_step(from_db: f64, to_db: f64, ramp_frames: u32) -> f64 {
    if ramp_frames == 0 {
        0.0
    } else {
        (to_db - from_db) / ramp_frames as f64
    }
}

/// volume ramp 第 frame 帧的 dB 值（frame=0 -> from；frame>=ramp_frames -> to）。
///
/// 语义冻结：线性 dB 插值、单调、无过冲；mute 由独立字段/阶段执行（不是音量 -inf）。
pub fn volume_gain_db_at_frame(from_db: f64, to_db: f64, ramp_frames: u32, frame: u32) -> f64 {
    if ramp_frames == 0 || frame >= ramp_frames {
        return to_db;
    }
    from_db + (to_db - from_db) * frame as f64 / ramp_frames as f64
}

/// volume ramp 第 frame 帧的线性增益（便于测试/报告复算）。
pub fn volume_linear_gain_at_frame(from_db: f64, to_db: f64, ramp_frames: u32, frame: u32) -> f64 {
    db_to_linear(volume_gain_db_at_frame(from_db, to_db, ramp_frames, frame))
}

/// biquad 复数响应 (re, im)（z = e^{jw}；a0 必须已归一为 1）。
pub fn biquad_complex_response(b: &[f64; 3], a: &[f64; 3], freq_hz: f64, fs: f64) -> (f64, f64) {
    let w = 2.0 * core::f64::consts::PI * freq_hz / fs;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let num_re = b[0] + b[1] * c1 + b[2] * c2;
    let num_im = -(b[1] * s1 + b[2] * s2);
    let den_re = a[0] + a[1] * c1 + a[2] * c2;
    let den_im = -(a[1] * s1 + a[2] * s2);
    let denom = den_re * den_re + den_im * den_im;
    if denom == 0.0 {
        return (f64::NAN, f64::NAN);
    }
    (
        (num_re * den_re + num_im * den_im) / denom,
        (num_im * den_re - num_re * den_im) / denom,
    )
}

/// biquad 幅度响应（dB）。
pub fn biquad_magnitude_db(b: &[f64; 3], a: &[f64; 3], freq_hz: f64, fs: f64) -> f64 {
    let (re, im) = biquad_complex_response(b, a, freq_hz, fs);
    let magnitude = (re * re + im * im).sqrt();
    if magnitude <= 0.0 || !magnitude.is_finite() {
        return f64::NEG_INFINITY;
    }
    20.0 * magnitude.log10()
}

/// biquad 相位响应（deg）。
pub fn biquad_phase_deg(b: &[f64; 3], a: &[f64; 3], freq_hz: f64, fs: f64) -> f64 {
    let (re, im) = biquad_complex_response(b, a, freq_hz, fs);
    im.atan2(re).to_degrees()
}

/// biquad 极点半径最大值（a0=1；复数共轭对返回 sqrt(a2)，实根取较大 |root|）。
pub fn biquad_max_pole_radius(a: &[f64; 3]) -> f64 {
    let a1 = a[1];
    let a2 = a[2];
    let disc = a1 * a1 - 4.0 * a2;
    if disc >= 0.0 {
        let root = disc.sqrt();
        let r1 = (-a1 + root) / 2.0;
        let r2 = (-a1 - root) / 2.0;
        r1.abs().max(r2.abs())
    } else {
        a2.abs().sqrt()
    }
}

/// biquad 参考处理（transposed direct form II；[TEST]/offline，不是 RT 入口）。
///
/// state 为 2 个状态样本（跨调用保持）；返回本样本输出。
pub fn biquad_process_df2t_f64(
    b: &[f64; 3],
    a: &[f64; 3],
    state: &mut [f64; 2],
    input: f64,
) -> f64 {
    let output = b[0] * input + state[0];
    state[0] = b[1] * input - a[1] * output + state[1];
    state[1] = b[2] * input - a[2] * output;
    output
}

/// 计算 RBJ biquad 系数（§5.3 规范公式；a0 归一到 1；含契约数值界限/稳定性检查）。
pub fn rbj_biquad(
    band_type: ModalBandType,
    freq_hz: f64,
    q: f64,
    gain_db: f64,
    fs: f64,
) -> Result<([f64; 3], [f64; 3]), RtStatus> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err(RtStatus::ERR_RATE);
    }
    if !freq_hz.is_finite() || !q.is_finite() || !gain_db.is_finite() {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    let upper = modal_freq_upper_hz(fs);
    if freq_hz < MODAL_FREQ_MIN_HZ || freq_hz > upper {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    if !(q > 0.0) {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    if gain_db.abs() > MODAL_GAIN_LIMIT_DB {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    match band_type {
        ModalBandType::Peaking => {
            if q < MODAL_PEAK_Q_MIN {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
        }
        ModalBandType::LowShelf | ModalBandType::HighShelf => {
            if !(MODAL_SHELF_S_MIN..=MODAL_SHELF_S_MAX).contains(&q) {
                return Err(RtStatus::ERR_PATH_TABLE);
            }
        }
    }
    let w0 = 2.0 * core::f64::consts::PI * freq_hz / fs;
    let (sin_w0, cos_w0) = w0.sin_cos();
    let a = 10f64.powf(gain_db / 40.0);
    let (raw_b, raw_a) = match band_type {
        ModalBandType::Peaking => {
            let alpha = sin_w0 / (2.0 * q);
            (
                [1.0 + alpha * a, -2.0 * cos_w0, 1.0 - alpha * a],
                [1.0 + alpha / a, -2.0 * cos_w0, 1.0 - alpha / a],
            )
        }
        ModalBandType::LowShelf => {
            let alpha = shelf_alpha(sin_w0, a, q);
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            (
                [
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
                    a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                ],
                [
                    (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
                    (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                ],
            )
        }
        ModalBandType::HighShelf => {
            let alpha = shelf_alpha(sin_w0, a, q);
            let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
            (
                [
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
                    a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
                ],
                [
                    (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
                    (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
                ],
            )
        }
    };
    let a0 = raw_a[0];
    if !a0.is_finite() || a0.abs() < 1e-300 {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    let b = [raw_b[0] / a0, raw_b[1] / a0, raw_b[2] / a0];
    let normalized_a = [1.0, raw_a[1] / a0, raw_a[2] / a0];
    for value in b.iter().chain(normalized_a.iter()) {
        if !value.is_finite() {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
    }
    if biquad_max_pole_radius(&normalized_a) > POLE_RADIUS_MAX {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    Ok((b, normalized_a))
}

fn shelf_alpha(sin_w0: f64, a: f64, s: f64) -> f64 {
    sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt()
}

/// 精确线性卷积（零填；拒绝空输入）。
pub fn compose_linear_fir(a: &[f64], b: &[f64]) -> Result<Vec<f64>, RtStatus> {
    if a.is_empty() || b.is_empty() {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    Ok(convolve_linear(a, b))
}

/// 按规范顺序合成多个 FIR 段（crossover → target → phase；调用方保证段序已校验）。
pub fn compose_fir_segments(segments: &[FirSegment]) -> Result<Vec<f64>, RtStatus> {
    if segments.is_empty() {
        return Ok(vec![1.0]);
    }
    let mut coefficients = segments[0].coefficients.clone();
    if coefficients.is_empty() || coefficients.iter().any(|v| !v.is_finite()) {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    for segment in &segments[1..] {
        if segment.coefficients.is_empty() || segment.coefficients.iter().any(|v| !v.is_finite()) {
            return Err(RtStatus::ERR_PATH_TABLE);
        }
        coefficients = convolve_linear(&coefficients, &segment.coefficients);
    }
    Ok(coefficients)
}

/// scatter 未使用的容器通道（按升序；kernel 必须逐样本 +0.0）。
pub fn scatter_zero_fill_containers(scatter: &[u16], container_channels: u16) -> Vec<u16> {
    let mut used = vec![false; container_channels as usize];
    for &out in scatter {
        if (out as usize) < used.len() {
            used[out as usize] = true;
        }
    }
    used.iter()
        .enumerate()
        .filter(|(_, used)| !**used)
        .map(|(index, _)| index as u16)
        .collect()
}

/// CRC-32C（Castagnoli；KhaenRoleV1.name_hash 口径；空输入 = 0）。
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82F6_3B78u32 & mask);
        }
    }
    !crc
}

/// 模态 PEQ 频率上限：min(20000, fs/2 - 1)。
pub fn modal_freq_upper_hz(fs: f64) -> f64 {
    let nyquist_guard = fs / 2.0 - 1.0;
    if nyquist_guard < MODAL_FREQ_MAX_HZ {
        nyquist_guard
    } else {
        MODAL_FREQ_MAX_HZ
    }
}

fn quantize_coeffs_f32(coefficients: &[f64; 3]) -> [f64; 3] {
    [
        coefficients[0] as f32 as f64,
        coefficients[1] as f32 as f64,
        coefficients[2] as f32 as f64,
    ]
}

fn coeffs_to_f32(coefficients: &[f64; 3]) -> [f32; 3] {
    [
        coefficients[0] as f32,
        coefficients[1] as f32,
        coefficients[2] as f32,
    ]
}

fn modal_grid_freq(index: usize) -> f64 {
    let exponent = index as f64 / (MODAL_EVAL_POINTS - 1) as f64;
    MODAL_FREQ_MIN_HZ * 1000f64.powf(exponent)
}

fn max_magnitude_deviation_db(
    b: &[f64; 3],
    a: &[f64; 3],
    b_f32: &[f64; 3],
    a_f32: &[f64; 3],
    fs: f64,
) -> f64 {
    let mut max_dev: f64 = 0.0;
    for index in 0..MODAL_EVAL_POINTS {
        let freq = modal_grid_freq(index);
        let reference = biquad_magnitude_db(b, a, freq, fs);
        let quantized = biquad_magnitude_db(b_f32, a_f32, freq, fs);
        if !reference.is_finite() || !quantized.is_finite() {
            return f64::INFINITY;
        }
        let dev = (reference - quantized).abs();
        if dev > max_dev {
            max_dev = dev;
        }
    }
    max_dev
}

fn self_consistency_deviation(
    band_type: ModalBandType,
    freq_hz: f64,
    q: f64,
    gain_db: f64,
    b: &[f64; 3],
    a: &[f64; 3],
    fs: f64,
) -> (f64, f64) {
    let Ok((reference_b, reference_a)) = rbj_biquad(band_type, freq_hz, q, gain_db, fs) else {
        return (f64::INFINITY, f64::INFINITY);
    };
    let mut max_db: f64 = 0.0;
    let mut max_deg: f64 = 0.0;
    for index in (0..MODAL_EVAL_POINTS).step_by(MODAL_CONSISTENCY_STEP) {
        let freq = modal_grid_freq(index);
        let reference = biquad_magnitude_db(&reference_b, &reference_a, freq, fs);
        let exported = biquad_magnitude_db(b, a, freq, fs);
        if !reference.is_finite() || !exported.is_finite() {
            return (f64::INFINITY, f64::INFINITY);
        }
        max_db = max_db.max((reference - exported).abs());
        let reference_phase = biquad_phase_deg(&reference_b, &reference_a, freq, fs);
        let exported_phase = biquad_phase_deg(b, a, freq, fs);
        let mut diff = (reference_phase - exported_phase).rem_euclid(360.0);
        if diff > 180.0 {
            diff = 360.0 - diff;
        }
        max_deg = max_deg.max(diff);
    }
    (max_db, max_deg)
}

// ---------------------------------------------------------------------------
// 静态断言（配置/投影常量）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(MODAL_EVAL_POINTS > MODAL_CONSISTENCY_STEP);
    assert!(MODAL_FREQ_MIN_HZ < MODAL_FREQ_MAX_HZ);
};
