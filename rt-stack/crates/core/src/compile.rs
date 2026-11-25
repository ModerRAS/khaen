//! [NONRT] factorized/fused plan 编译 —— WBS T12 写域（crates/core/src/compile.rs）。
//!
//! 本模块把 T03 reference 语义图（A_map / 输入侧 A / 低带 G = delta_D I + g / 输出对角 P /
//! scatter）编译成两种可执行形态，并生成融合 cost 计划、按声明成本选择：
//!
//! 1. **factorized**：保留 `A -> G -> P` 三段（`u = A x`、`v = G u`、`y = P v`）；
//! 2. **fused**：只融合 G/P 两段，`F_ji = p_j * G_ji`（线性卷积、zero-pad），`A` 仍单独前置。
//!
//! 语义与公式（权威：reference/SPEC.md §2.3/§2.4/§2.5/§2.7、spec/core-v1.md §3.4/§7.1/§11.10、
//! spec/fir-toolbox.md §7.1、scratch/lead/planner/DSP-HOST-DECISION.md §3）：
//!
//! ~~~text
//! x_N -> A_map -> u (输入侧 FIR，作用列) -> G = delta_D I + g (5x5 低带嵌入) -> v
//!     -> P_post (输出对角 FIR，作用行) -> y -> scatter
//! fused:  F[j][k] = p_j (*) G[j][k]，L_F = L_G + L_P - 1（D 计入 L_G 支撑）
//!         完整单路图长（含 A）：L_total = L_A + L_G + L_P - 2（T03 §3.1；T04 的 L_F 只是 G/P 段）
//! ~~~
//!
//! **固定口径（不得交换/重复计数）：**
//! - 输入侧 delay/polarity 编码在 A_i 系数里（如 `[0, -1]` = 1 采样延迟且反相），作用在**列**；
//! - 输出侧 delay/polarity 编码在 p_j 系数里，作用在**行**；`P·G != G·P`（非对称矩阵）；
//! - `G` 含 `delta_D` 对角 baseline（D 计入支撑，`L_G >= D + 1`）；非参与角色行只有 baseline；
//! - fused 系数必须由**线性卷积**（zero-padded）得到：短 FFT 循环卷积属于禁止实现（T03 H11/C04 trap）；
//!   本模块只用精确多项式卷积生成系数，另提供 `oracle_*` 频谱函数供测试/独立复核 zero-pad 等价；
//! - `L_F > profile 上限` 时保留 factorized 图并报告 `FusedOverTapLimit`，或整体 INFEASIBLE；
//!   **禁止静默截尾**；
//! - 融合不一定省算：G=4096 + P=1024 时 direct 成本 10.32192 GF/s -> 12.2856 GF/s（25 路），
//!   因此编译必须同时报告两种成本并按成本选择，不得假称融合省算（DSP-HOST-DECISION §3）；
//! - 成本模型按 **path-sparse** 计：矩阵阶段只数声明非零 route，identity 的 A/P 不计 MAC
//!   （不是 dense O x O 全循环）；partitioned/FFT 数字是模型估计（公式见 partitioned_cost），
//!   **不是实测**，仅作 T56/报告参考；选择口径固定为 direct MAC/sample。
//!
//! 本模块是 **NONRT** 编译/设计期代码（可分配、可 Drop，不是 RT-MODULE）：结果经 T26/T66 打包成
//! plan（`KhaenPathV1`/`KhaenDiagFirV1`）；本模块不写 ABI 字节、不碰 RT 状态。当前 `abi.rs` 仍是
//! v1.0 scaffold（`KhaenPathV1` 无 `delay_q16` 字段、无 A 阶段表），plan 打包与 ABI v1.1 对齐归
//! integrator/T26（见 evidence/T12/manifest.md 的契约说明）。
//!
//! 半整数/分数 D：T04 允许的设计 D 由 [design_delay_to_plan] 拆成 `delay_samples + delay_q16`
//! （Q16，残差 < 1/65536 sample，core-v1 §3.4/§11.10/§12.3）；本模块的 G baseline D 仍是 T03
//! 参考语义的**整数** D。T03 oracle 对拍要求 `delay_q16 == 0` 的图（core-v1 §15.1 N3 规则），
//! 即分数部分必须已折入 A/P 系数（设计器职责）后再编译。
//!
//! 错误码（沿用冻结 `RtStatus`，不新增码）：通道/容器 -> `ERR_CHANNELS`；role/participants/
//! A_map/scatter -> `ERR_ROLES`；taps/D/L_F/profile -> `ERR_TAPS`；系数数组长度/重复路径 ->
//! `ERR_PATH_TABLE`；fs/B -> `ERR_RATE`/`ERR_BLOCK_SIZE`；系数预算/容量 -> `ERR_CAPACITY`。

use crate::rt::{
    RtStatus, BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_MATRIX_PATHS,
    MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, SAMPLE_RATES_HZ, TAPS_EXTENDED_MAX, TAPS_NORMAL_MAX,
};

// ---------------------------------------------------------------------------
// 常量与 A_map 哨兵
// ---------------------------------------------------------------------------

/// `src_of_role` 的“本角色无节目输入”哨兵（A_map 整行为 0，输出必须逐样本零填）。
pub const COMPILE_NO_SOURCE: u16 = u16::MAX;

/// `KhaenPathV1.flags` bit0：本路径系数含 `delta_D` 对角 baseline（core-v1 §11.10）。
pub const PATH_FLAG_DIAG_BASELINE_INCLUDED: u8 = 1 << 0;
/// `KhaenPathV1.flags` bit1：本路径是 G/P 融合结果（factorized 图里无此位）。
pub const PATH_FLAG_FUSED: u8 = 1 << 1;
/// `KhaenPathV1.flags` bit2：系数按 zero-padding 线性卷积生成（禁止短 FFT 循环混叠）。
pub const PATH_FLAG_ZERO_PAD_LINEAR_CONV: u8 = 1 << 2;
/// `KhaenPathV1.flags` bit3：输入侧 delay/polarity 已编码进系数（A_i 列语义）。
pub const PATH_FLAG_INPUT_SIDE_DELAY: u8 = 1 << 3;

/// 默认 f32 系数预算（4 MiB；调用方可按 profile 收紧/放宽）。
pub const COMPILE_DEFAULT_COEFF_BUDGET_BYTES: u64 = 4 * 1024 * 1024;

/// 编译输出必须打包进 plan 的 f32 系数区（`KhaenPlanHeaderV1.total_coeff_bytes: u32`）。
const PLAN_COEFF_BYTES_MAX: u64 = u32::MAX as u64;

const _: () = {
    assert!(COMPILE_NO_SOURCE == u16::MAX);
    assert!(PATH_FLAG_DIAG_BASELINE_INCLUDED == 0x01);
    assert!(PATH_FLAG_FUSED == 0x02);
    assert!(PATH_FLAG_ZERO_PAD_LINEAR_CONV == 0x04);
    assert!(PATH_FLAG_INPUT_SIDE_DELAY == 0x08);
    assert!(MAX_INPUT_CHANNELS == 20);
    assert!(MAX_OUTPUT_ROLES == 16);
    assert!(MAX_LOW_BAND_PARTICIPANTS == 8);
    assert!(MAX_MATRIX_PATHS == 128);
    assert!(TAPS_NORMAL_MAX == 4096 && TAPS_EXTENDED_MAX == 16_384);
    assert!(COMPILE_DEFAULT_COEFF_BUDGET_BYTES == 4 * 1024 * 1024);
};

// ---------------------------------------------------------------------------
// 半整数/分数 D -> plan 的 (delay_samples, delay_q16)
// ---------------------------------------------------------------------------

/// plan 层时延（整数样本 + Q16 次样本；core-v1 §11.10/§12.3 I01）。
///
/// `total_delay_q16 = delay_samples * 65536 + delay_q16`，`delay_q16` 在 [0, 65535]；
/// 量化残差 < 1/65536 sample（round-to-nearest 保证 <= 1/131072）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelayPlan {
    /// 整数样本部分。
    pub delay_samples: u32,
    /// 次样本 Q16 部分（0..=65535）。
    pub delay_q16: u32,
}

impl DelayPlan {
    /// 零时延。
    pub const ZERO: Self = Self {
        delay_samples: 0,
        delay_q16: 0,
    };

    /// 总时延的 Q16 表示（值本身以 1/65536 sample 为单位）。
    pub const fn total_delay_q16(self) -> u64 {
        ((self.delay_samples as u64) << 16) | (self.delay_q16 as u64)
    }
}

/// 把设计域总时延（可为半整数/分数样本）拆成 plan 的 `(delay_samples, delay_q16)`。
///
/// 规则（core-v1 §3.4/§11.10/§12.3）：整数部分写 `delay_samples`；余数
/// `round(frac * 65536)` 写 `delay_q16`；若 `round` 进到 65536 则向整数部分进位。
/// 量化残差 < 1/65536 sample。拒绝负值/NaN/Inf/超出 `u32` 样本范围。
pub fn design_delay_to_plan(total_delay_samples: f64) -> Result<DelayPlan, RtStatus> {
    if !total_delay_samples.is_finite() || total_delay_samples < 0.0 {
        return Err(RtStatus::ERR_TAPS);
    }
    let integer = total_delay_samples.floor();
    if integer > u32::MAX as f64 {
        return Err(RtStatus::ERR_TAPS);
    }
    let mut delay_samples = integer as u32;
    let frac = total_delay_samples - integer;
    let mut delay_q16 = (frac * 65_536.0).round() as u32;
    if delay_q16 >= 65_536 {
        // 进位：0.999999... -> 1.0 sample。
        delay_samples = match delay_samples.checked_add(1) {
            Some(v) => v,
            None => return Err(RtStatus::ERR_TAPS),
        };
        delay_q16 = 0;
    }
    Ok(DelayPlan {
        delay_samples,
        delay_q16,
    })
}

/// 某 `DelayPlan` 相对设计值 `total_delay_samples` 的量化残差（样本，绝对值）。
pub fn delay_plan_residual_samples(plan: DelayPlan, total_delay_samples: f64) -> f64 {
    let realized = plan.delay_samples as f64 + plan.delay_q16 as f64 / 65_536.0;
    (total_delay_samples - realized).abs()
}

// ---------------------------------------------------------------------------
// 编译输入：T03 reference 语义图
// ---------------------------------------------------------------------------

/// 编译输入：T03 reference 语义图（只借用，不拥有）。
///
/// 数组布局（行主序、声明支撑零填充）：
/// - `a`：`n_src * l_a`，`[src][tap]`；输入侧 FIR（含输入侧延迟/极性，作用**列**）；
/// - `g`：`m * m * l_g`，`[dense_row][dense_col][tap]`；**不含** `delta_D` baseline；
/// - `p`：`n_role * l_p`，`[role][tap]`；输出侧对角 FIR（含输出侧延迟/极性，作用**行**）。
#[derive(Clone, Copy, Debug)]
pub struct CompileInput<'a> {
    /// 源通道数 N。
    pub n_src: u16,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 播放容器通道数 C（>= O）。
    pub container_channels: u16,
    /// 低带公共 baseline 整数延迟 D（样本，计入 L_G 支撑）。
    pub d_samples: u32,
    /// 低带 participants（角色索引，有序；对应 g 的 dense 行/列）。
    pub participants: &'a [u16],
    /// A_map：每个角色行选中的源（[COMPILE_NO_SOURCE] = 整行零填）。
    pub src_of_role: &'a [u16],
    /// 角色到容器通道单射 scatter。
    pub scatter: &'a [u16],
    /// 输入侧 FIR 系数（N x L_A）。
    pub a: &'a [f64],
    /// 输入侧声明支撑 L_A（>= 1）。
    pub l_a: u32,
    /// 低带 g 系数（M x M x L_G，不含 delta_D）。
    pub g: &'a [f64],
    /// G 声明支撑 L_G（>= D + 1）。
    pub l_g: u32,
    /// 输出对角 FIR 系数（O x L_P）。
    pub p: &'a [f64],
    /// 输出侧声明支撑 L_P（>= 1）。
    pub l_p: u32,
}

impl<'a> CompileInput<'a> {
    /// participants 数 M。
    pub fn m(&self) -> usize {
        self.participants.len()
    }

    /// 校验输入图（T03 §2/§3.1 + core-v1 容量约束）。返回 OK 或冻结错误码。
    pub fn validate(&self) -> RtStatus {
        let n_src = self.n_src as usize;
        let n_role = self.n_role as usize;
        let containers = self.container_channels as usize;
        let m = self.m();
        let l_a = self.l_a as usize;
        let l_g = self.l_g as usize;
        let l_p = self.l_p as usize;

        if n_src == 0 || n_src > MAX_INPUT_CHANNELS {
            return RtStatus::ERR_CHANNELS;
        }
        if n_role == 0 || n_role > MAX_OUTPUT_ROLES {
            return RtStatus::ERR_ROLES;
        }
        if containers < n_role || containers > MAX_OUTPUT_CHANNELS {
            return RtStatus::ERR_CHANNELS;
        }
        if l_a == 0 || l_g == 0 || l_p == 0 {
            return RtStatus::ERR_TAPS;
        }
        // D 必须计入 G 支撑（T03 §3.1：L_G >= D + 1）。
        if self.d_samples as u64 >= self.l_g as u64 {
            return RtStatus::ERR_TAPS;
        }
        // 数组长度必须精确等于声明支撑乘积（不做静默补零/截断）。
        let a_len = match n_src.checked_mul(l_a) {
            Some(v) => v,
            None => return RtStatus::ERR_CAPACITY,
        };
        let g_len = match m.checked_mul(m).and_then(|v| v.checked_mul(l_g)) {
            Some(v) => v,
            None => return RtStatus::ERR_CAPACITY,
        };
        let p_len = match n_role.checked_mul(l_p) {
            Some(v) => v,
            None => return RtStatus::ERR_CAPACITY,
        };
        if self.a.len() != a_len || self.g.len() != g_len || self.p.len() != p_len {
            return RtStatus::ERR_PATH_TABLE;
        }
        if self.participants.len() != m || m == 0 || m > MAX_LOW_BAND_PARTICIPANTS || m > n_role {
            return RtStatus::ERR_ROLES;
        }
        if self.src_of_role.len() != n_role || self.scatter.len() != n_role {
            return RtStatus::ERR_ROLES;
        }
        // 所有系数必须有限（NaN/Inf 在编译期拒绝，不进入 plan）。
        for &v in self.a.iter().chain(self.g.iter()).chain(self.p.iter()) {
            if !v.is_finite() {
                return RtStatus::ERR_PATH_TABLE;
            }
        }

        // participants：范围 + 唯一。
        let mut seen_roles = [false; MAX_OUTPUT_ROLES];
        for &role in self.participants.iter() {
            let role = role as usize;
            if role >= n_role || seen_roles[role] {
                return RtStatus::ERR_ROLES;
            }
            seen_roles[role] = true;
        }

        // A_map：每行至多一个源、每源至多一行（只选择/零填，禁止复制/分发）。
        let mut source_used = [false; MAX_INPUT_CHANNELS];
        for &src in self.src_of_role.iter() {
            if src != COMPILE_NO_SOURCE {
                let src = src as usize;
                if src >= n_src || source_used[src] {
                    return RtStatus::ERR_ROLES;
                }
                source_used[src] = true;
            }
        }

        // scatter：容器范围 + 单射。
        let mut seen_containers = [false; MAX_OUTPUT_CHANNELS];
        for &c in self.scatter.iter() {
            let c = c as usize;
            if c >= containers || seen_containers[c] {
                return RtStatus::ERR_ROLES;
            }
            seen_containers[c] = true;
        }

        RtStatus::OK
    }
}

// ---------------------------------------------------------------------------
// 编译选项与成本口径
// ---------------------------------------------------------------------------

/// 编译选项（profile 级；由 T26/T56/校准 daemon 在 prepare 期给出）。
#[derive(Clone, Copy, Debug)]
pub struct CompileOptions {
    /// 采样率（48k/96k，rt::SAMPLE_RATES_HZ）。
    pub sample_rate_hz: u32,
    /// block 大小（64/128，rt::BLOCK_SIZES）。
    pub block_size: u16,
    /// 本 profile 每 route 有效支撑上限（常规 4096；extended 8192/16384）。
    pub max_taps: u32,
    /// 是否已通过 extended profile 显式准入（max_taps > 4096 时必须为 true）。
    pub extended_taps_admitted: bool,
    /// plan f32 系数区预算（字节）。
    pub coeff_budget_bytes: u64,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            sample_rate_hz: 48_000,
            block_size: 128,
            max_taps: TAPS_NORMAL_MAX,
            extended_taps_admitted: false,
            coeff_budget_bytes: COMPILE_DEFAULT_COEFF_BUDGET_BYTES,
        }
    }
}

impl CompileOptions {
    /// 校验选项；返回 OK 或冻结错误码。
    pub fn validate(&self) -> RtStatus {
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate_hz) {
            return RtStatus::ERR_RATE;
        }
        if !BLOCK_SIZES.contains(&self.block_size) {
            return RtStatus::ERR_BLOCK_SIZE;
        }
        if self.max_taps == 0 || self.max_taps > TAPS_EXTENDED_MAX {
            return RtStatus::ERR_TAPS;
        }
        if self.max_taps > TAPS_NORMAL_MAX && !self.extended_taps_admitted {
            return RtStatus::ERR_TAPS;
        }
        if self.coeff_budget_bytes == 0 {
            return RtStatus::ERR_CAPACITY;
        }
        RtStatus::OK
    }
}

/// 编译形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FusionForm {
    /// 保留 A/G/P 三段。
    Factorized,
    /// 融合 G/P：`F_ji = p_j * G_ji`（A 仍单独前置）。
    FusedGp,
}

/// 选择原因（写入编译报告；禁止“静默选择”）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionReason {
    /// fused direct MAC/sample 严格小于 factorized，且资源可行 -> 选 fused。
    FusedCheaper,
    /// fused 成本不低于 factorized -> 保留 factorized（融合不一定省算）。
    FusedNotCheaper,
    /// fused 的 `L_F` 超出本 profile `max_taps` -> 保留 factorized（重新准入/INFEASIBLE，不截尾）。
    FusedOverTapLimit,
    /// fused 矩阵路径数超出 `MAX_MATRIX_PATHS` -> 保留 factorized。
    FusedOverRouteLimit,
    /// fused 系数区超出预算 -> 保留 factorized。
    FusedOverCoeffBudget,
}

/// 分区/FFT 成本模型（informative；仅供 T56/报告，不参与本任务选择）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartitionedCost {
    /// 共享 FFT 长度 K = 2 x B。
    pub fft_size: u32,
    /// 最大每路径分区数 `ceil(taps/B)`。
    pub max_partitions: u32,
    /// 每 block 共享变换数（每个用到的源 1 次 + 每个输出 1 次）。
    pub transforms_per_block: u32,
    /// 模型估计 real FLOP/s（公式见 partitioned_cost；不是实测）。
    pub real_flops_per_second: f64,
}

/// 单形态成本估计（direct 时间域为主口径）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostEstimate {
    /// 本成本对应的形态。
    pub form: FusionForm,
    /// 非零矩阵路径数（factorized 为 G 路径，fused 为 F 路径）。
    pub routes: u32,
    /// 输入侧 A 阶段每采样 MAC 数（identity A 计 0）。
    pub input_stage_taps: u64,
    /// 矩阵阶段每采样 MAC 数（factorized: routes*L_G；fused: routes*L_F）。
    pub matrix_stage_taps: u64,
    /// 输出侧 P 阶段每采样 MAC 数（factorized 才非零；identity P 计 0）。
    pub output_stage_taps: u64,
    /// 每采样 MAC 总数（= 三段之和）。
    pub macs_per_sample: u64,
    /// direct 理论算力：`2 * fs * macs_per_sample`（FLOP/s）。
    pub flops_per_second: f64,
    /// 单 route 最大 taps。
    pub peak_taps: u32,
    /// f32 系数字节（含 A 的 dense 存储）。
    pub coeff_bytes_f32: u64,
    /// 分区/FFT 模型估计。
    pub partitioned: PartitionedCost,
}

/// 编译报告：两种形态的成本 + 选择 + 长度/限制事实。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileReport {
    /// 选择的形态。
    pub chosen: FusionForm,
    /// 选择原因。
    pub reason: SelectionReason,
    /// factorized 成本。
    pub factorized: CostEstimate,
    /// fused 成本。
    pub fused: CostEstimate,
    /// `L_F = L_G + L_P - 1`（G/P 融合段长度；T04 §7.1）。
    pub fused_len_gp: u32,
    /// `L_total = L_A + L_G + L_P - 2`（含 A 的完整单路图长；T03 §3.1）。
    pub full_path_len: u32,
    /// fused 是否在本 profile 内可行（taps/路径数/预算）。
    pub fused_feasible: bool,
    /// fused 系数是否实际生成（可行才生成，避免超大分配）。
    pub fused_built: bool,
    /// 本 profile 每 route taps 上限。
    pub taps_limit: u32,
    /// extended profile 是否已准入。
    pub extended_taps_admitted: bool,
    /// 系数预算（字节）。
    pub coeff_budget_bytes: u64,
    /// 非零矩阵路径数。
    pub route_count: u32,
    /// `factorized.macs - fused.macs`（>0 表示 fused 更省；<=0 表示融合不省/相等）。
    pub saved_macs_per_sample: i64,
    /// 对应的 FLOP/s 差（>0 表示 fused 更省）。
    pub saved_flops_per_second: f64,
    /// 采样率（成本口径）。
    pub sample_rate_hz: u32,
    /// block 大小（成本口径）。
    pub block_size: u16,
}

// ---------------------------------------------------------------------------
// 编译产物
// ---------------------------------------------------------------------------

/// factorized 图（A/G/P 三段 + 选择/零填/scatter 元数据；数组为行主序声明支撑）。
#[derive(Clone, Debug)]
pub struct FactorizedGraph {
    /// 源通道数 N。
    pub n_src: u16,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 容器通道数 C。
    pub container_channels: u16,
    /// 低带 baseline 整数延迟 D。
    pub d_samples: u32,
    /// 输入侧声明支撑 L_A。
    pub l_a: u32,
    /// G 声明支撑 L_G。
    pub l_g: u32,
    /// 输出侧声明支撑 L_P。
    pub l_p: u32,
    /// participants（对应 g dense 行列）。
    pub participants: Vec<u16>,
    /// A_map。
    pub src_of_role: Vec<u16>,
    /// 角色 -> 容器通道。
    pub scatter: Vec<u16>,
    /// 输入侧 FIR（N x L_A）。
    pub a: Vec<f64>,
    /// 低带 g（M x M x L_G，不含 delta_D）。
    pub g: Vec<f64>,
    /// 输出对角 FIR（O x L_P）。
    pub p: Vec<f64>,
}

impl FactorizedGraph {
    /// 完整单路图长 `L_A + L_G + L_P - 2`（T03 §3.1）。
    pub fn full_path_len(&self) -> u32 {
        self.l_a + self.l_g + self.l_p - 2
    }

    /// 输入侧 A 阶段计划：每个源一条 route（taps = L_A，delay 已编码在系数里）。
    pub fn input_stage_plans(&self) -> Vec<StagePlan> {
        let l_a = self.l_a as usize;
        let n_src = self.n_src as usize;
        let mut out = Vec::with_capacity(n_src);
        for src in 0..n_src {
            let coeffs = &self.a[src * l_a..(src + 1) * l_a];
            out.push(StagePlan {
                index: src as u16,
                in_index: src as u16,
                taps: self.l_a,
                delay_samples: 0,
                delay_q16: 0,
                polarity: 1,
                flags: PATH_FLAG_INPUT_SIDE_DELAY,
                coeffs_f32: to_f32(coeffs),
            });
        }
        out
    }

    /// 矩阵 G 阶段计划：`delta_D` baseline 对角 + 非零 g cell（行/列语义固定）。
    pub fn matrix_stage_plans(&self) -> Vec<MatrixStagePlan> {
        let n_role = self.n_role as usize;
        let l_g = self.l_g as usize;
        let d = self.d_samples as usize;
        let m = self.participants.len();
        let positions = role_positions(n_role, &self.participants);
        let mut out = Vec::new();
        for out_role in 0..n_role {
            for in_role in 0..n_role {
                let src = self.src_of_role[in_role];
                if src == COMPILE_NO_SOURCE {
                    continue;
                }
                let coeffs = match g_vector(&positions, &self.g, m, l_g, out_role, in_role, d) {
                    Some(coeffs) => coeffs,
                    None => continue,
                };
                let diag_baseline = out_role == in_role;
                out.push(MatrixStagePlan {
                    out_index: out_role as u16,
                    in_role: in_role as u16,
                    in_source: src,
                    diag_baseline,
                    taps: self.l_g,
                    // baseline D 只存在于对角路径；offdiag 的 g cell 无 delta_D（T03 §2.4）。
                    delay_samples: if diag_baseline { self.d_samples } else { 0 },
                    delay_q16: 0,
                    coeffs_f32: to_f32(&coeffs),
                });
            }
        }
        out
    }

    /// 输出侧 P 阶段计划：每个角色一条对角 FIR。
    pub fn output_stage_plans(&self) -> Vec<StagePlan> {
        let l_p = self.l_p as usize;
        let n_role = self.n_role as usize;
        let mut out = Vec::with_capacity(n_role);
        for role in 0..n_role {
            let coeffs = &self.p[role * l_p..(role + 1) * l_p];
            out.push(StagePlan {
                index: role as u16,
                in_index: role as u16,
                taps: self.l_p,
                delay_samples: 0,
                delay_q16: 0,
                polarity: 1,
                flags: 0,
                coeffs_f32: to_f32(coeffs),
            });
        }
        out
    }
}

/// 一条 fused 路径（`F_ji = p_j * G_ji`；A 仍单独前置）。
#[derive(Clone, Debug, PartialEq)]
pub struct FusedPath {
    /// 输入源通道索引（A_map 选择后的源，不是角色索引）。
    pub in_index: u16,
    /// 物理输出角色索引。
    pub out_index: u16,
    /// 对应的 G 输入角色索引（诊断/plan 元数据用）。
    pub in_role: u16,
    /// 本路径 taps（= L_F）。
    pub taps: u32,
    /// 是否含 `delta_D` 对角 baseline。
    pub diag_baseline: bool,
    /// 融合系数（f64 设计域；打包 plan 时转 f32）。
    pub coeffs: Vec<f64>,
}

/// fused 图：`y_j = sum_k (F_jk * u_k)`，`u` 为输入侧 A/A_map 之后的角色域信号。
#[derive(Clone, Debug)]
pub struct FusedGraph {
    /// 源通道数 N。
    pub n_src: u16,
    /// 输出角色数 O。
    pub n_role: u16,
    /// 容器通道数 C。
    pub container_channels: u16,
    /// 低带 baseline 整数延迟 D（已计入 `L_F` 支撑）。
    pub d_samples: u32,
    /// `L_F = L_G + L_P - 1`。
    pub l_f: u32,
    /// 完整单路图长 `L_A + L_G + L_P - 2`。
    pub full_path_len: u32,
    /// participants（诊断用）。
    pub participants: Vec<u16>,
    /// A_map。
    pub src_of_role: Vec<u16>,
    /// 角色 -> 容器通道。
    pub scatter: Vec<u16>,
    /// 按 `(out_index, in_index)` 升序的路径表（core-v1 §11.10 表顺序）。
    pub paths: Vec<FusedPath>,
}

/// 编译后的两条计划（fused 不可行时为 `None`，只保留 factorized）。
#[derive(Clone, Debug)]
pub struct CompiledPlans {
    /// factorized 图（总是生成；保证可执行回退）。
    pub factorized: FactorizedGraph,
    /// fused 图（仅在本 profile 内可行时生成）。
    pub fused: Option<FusedGraph>,
    /// 成本/选择报告。
    pub report: CompileReport,
}

/// 选择的计划视图。
#[derive(Clone, Copy, Debug)]
pub enum SelectedPlan<'a> {
    /// 选择 factorized（A/G/P 三段）。
    Factorized(&'a FactorizedGraph),
    /// 选择 fused（A + F）。
    Fused(&'a FusedGraph),
}

impl CompiledPlans {
    /// 按报告选择返回实际执行形态。
    pub fn selected(&self) -> SelectedPlan<'_> {
        match (self.report.chosen, self.fused.as_ref()) {
            (FusionForm::FusedGp, Some(fused)) => SelectedPlan::Fused(fused),
            _ => SelectedPlan::Factorized(&self.factorized),
        }
    }
}

/// fused 路径的 plan 投影（字段对齐 `KhaenPathV1` 语义；打包/ABI 对齐归 T26）。
#[derive(Clone, Debug, PartialEq)]
pub struct FusedPathPlan {
    /// 输入源通道（< N）。
    pub in_index: u16,
    /// 物理输出索引（< O）。
    pub out_index: u16,
    /// taps（= L_F）。
    pub taps: u32,
    /// 输出侧声明整数延迟（baseline D，已含在支撑内；kernel 不得重复加）。
    pub delay_samples: u32,
    /// 次样本延迟（本任务编译图恒为 0；分数 D 需先折入系数，core-v1 §15.1 N3）。
    pub delay_q16: u32,
    /// 系数外的极性乘子；融合图恒为 1（输出极性已折入系数，避免双乘）。
    pub polarity: i8,
    /// `KhaenPathV1.flags` 子集（见 PATH_FLAG_*）。
    pub flags: u8,
    /// f32 系数（plan arena 形态）。
    pub coeffs_f32: Vec<f32>,
}

impl FusedGraph {
    /// 生成 plan 投影（按 `(out_index, in_index)` 升序，即 `self.paths` 顺序）。
    pub fn plan_paths(&self) -> Vec<FusedPathPlan> {
        self.paths
            .iter()
            .map(|path| {
                let mut flags = PATH_FLAG_FUSED | PATH_FLAG_ZERO_PAD_LINEAR_CONV;
                if path.diag_baseline {
                    flags |= PATH_FLAG_DIAG_BASELINE_INCLUDED;
                }
                FusedPathPlan {
                    in_index: path.in_index,
                    out_index: path.out_index,
                    taps: path.taps,
                    // 对角路径含 delta_D baseline；offdiag 路径无 baseline，delay 得 0。
                    delay_samples: if path.diag_baseline {
                        self.d_samples
                    } else {
                        0
                    },
                    delay_q16: 0,
                    polarity: 1,
                    flags,
                    coeffs_f32: to_f32(&path.coeffs),
                }
            })
            .collect()
    }
}

/// 通用 stage 计划（输入 A / 输出 P）。
#[derive(Clone, Debug, PartialEq)]
pub struct StagePlan {
    /// stage 内序号（A: src；P: role）。
    pub index: u16,
    /// 对应输入/输出索引。
    pub in_index: u16,
    /// 声明 taps。
    pub taps: u32,
    /// 额外整数延迟（系数已含时写 0）。
    pub delay_samples: u32,
    /// 额外 Q16 延迟。
    pub delay_q16: u32,
    /// 系数外极性（系数已含时写 1）。
    pub polarity: i8,
    /// 标志位。
    pub flags: u8,
    /// f32 系数。
    pub coeffs_f32: Vec<f32>,
}

/// 矩阵 G 阶段计划（含 baseline 的 route 系数）。
#[derive(Clone, Debug, PartialEq)]
pub struct MatrixStagePlan {
    /// 输出角色。
    pub out_index: u16,
    /// G 输入角色。
    pub in_role: u16,
    /// 该输入角色选中的源。
    pub in_source: u16,
    /// 是否含 delta_D baseline。
    pub diag_baseline: bool,
    /// 声明 taps（= L_G）。
    pub taps: u32,
    /// baseline 延迟 D（已含在支撑内）。
    pub delay_samples: u32,
    /// Q16 延迟（本任务恒 0）。
    pub delay_q16: u32,
    /// f32 系数（含 baseline）。
    pub coeffs_f32: Vec<f32>,
}

// ---------------------------------------------------------------------------
// 编译入口
// ---------------------------------------------------------------------------

/// 编译 factorized + fused 两种图、生成成本计划并按 direct MAC 成本选择。
///
/// 不写 plan 字节；不做上混/下混（A_map 只选择/零填，由 [CompileInput::validate] 强制）。
pub fn compile(
    input: &CompileInput<'_>,
    options: &CompileOptions,
) -> Result<CompiledPlans, RtStatus> {
    let status = options.validate();
    if !status.is_ok() {
        return Err(status);
    }
    let status = input.validate();
    if !status.is_ok() {
        return Err(status);
    }

    let n_src = input.n_src as usize;
    let n_role = input.n_role as usize;
    let m = input.m();
    let l_a = input.l_a;
    let l_g = input.l_g;
    let l_p = input.l_p;

    // 长度加法（checked：溢出即 INFEASIBLE，不截尾）。
    let fused_len_gp = l_g
        .checked_add(l_p)
        .and_then(|v| v.checked_sub(1))
        .ok_or(RtStatus::ERR_TAPS)?;
    let full_path_len = l_a
        .checked_add(l_g)
        .and_then(|v| v.checked_add(l_p))
        .and_then(|v| v.checked_sub(2))
        .ok_or(RtStatus::ERR_TAPS)?;

    let routes = matrix_routes(input);
    let route_count = routes.len() as u32;
    if route_count as usize > MAX_MATRIX_PATHS {
        // factorized 也放不下这么多矩阵路径 -> INFEASIBLE（不静默丢弃路径）。
        return Err(RtStatus::ERR_PATH_TABLE);
    }

    let used_sources = used_source_count(input);
    let a_stage_taps = input_stage_cost(input);
    let p_stage_taps = output_stage_cost(input);

    let factorized_cost = cost_factorized(
        a_stage_taps,
        p_stage_taps,
        route_count,
        l_a,
        l_g,
        l_p,
        n_src,
        m,
        n_role,
        used_sources,
        options,
    );
    let fused_cost = cost_fused(
        a_stage_taps,
        route_count,
        fused_len_gp,
        l_a,
        n_src,
        n_role,
        used_sources,
        options,
    );

    // factorized 可行性（选中形态必须可执行；否则整体 INFEASIBLE）。
    let factor_peak = l_a.max(l_g).max(l_p);
    if factor_peak > options.max_taps {
        return Err(RtStatus::ERR_TAPS);
    }
    if factorized_cost.coeff_bytes_f32 > options.coeff_budget_bytes
        || factorized_cost.coeff_bytes_f32 > PLAN_COEFF_BYTES_MAX
    {
        return Err(RtStatus::ERR_CAPACITY);
    }

    // fused 可行性（仅影响是否可选，不影响 factorized 回退）。
    let fused_routes_ok = route_count as usize <= MAX_MATRIX_PATHS;
    let fused_taps_ok = fused_len_gp <= options.max_taps;
    let fused_budget_ok = fused_cost.coeff_bytes_f32 <= options.coeff_budget_bytes
        && fused_cost.coeff_bytes_f32 <= PLAN_COEFF_BYTES_MAX;
    let fused_feasible = fused_routes_ok && fused_taps_ok && fused_budget_ok;

    let (chosen, reason) = if !fused_routes_ok {
        (FusionForm::Factorized, SelectionReason::FusedOverRouteLimit)
    } else if !fused_taps_ok {
        (FusionForm::Factorized, SelectionReason::FusedOverTapLimit)
    } else if !fused_budget_ok {
        (
            FusionForm::Factorized,
            SelectionReason::FusedOverCoeffBudget,
        )
    } else if fused_cost.macs_per_sample < factorized_cost.macs_per_sample {
        (FusionForm::FusedGp, SelectionReason::FusedCheaper)
    } else {
        (FusionForm::Factorized, SelectionReason::FusedNotCheaper)
    };

    let factorized = FactorizedGraph {
        n_src: input.n_src,
        n_role: input.n_role,
        container_channels: input.container_channels,
        d_samples: input.d_samples,
        l_a: input.l_a,
        l_g: input.l_g,
        l_p: input.l_p,
        participants: input.participants.to_vec(),
        src_of_role: input.src_of_role.to_vec(),
        scatter: input.scatter.to_vec(),
        a: input.a.to_vec(),
        g: input.g.to_vec(),
        p: input.p.to_vec(),
    };

    // 只有可行才实际生成系数，避免生成后不得不截断（禁止静默截尾）。
    let fused = if fused_feasible {
        Some(build_fused(input, &routes, fused_len_gp, full_path_len))
    } else {
        None
    };

    let saved_macs_per_sample =
        factorized_cost.macs_per_sample as i64 - fused_cost.macs_per_sample as i64;
    let saved_flops_per_second = factorized_cost.flops_per_second - fused_cost.flops_per_second;

    let report = CompileReport {
        chosen,
        reason,
        factorized: factorized_cost,
        fused: fused_cost,
        fused_len_gp,
        full_path_len,
        fused_feasible,
        fused_built: fused.is_some(),
        taps_limit: options.max_taps,
        extended_taps_admitted: options.extended_taps_admitted,
        coeff_budget_bytes: options.coeff_budget_bytes,
        route_count,
        saved_macs_per_sample,
        saved_flops_per_second,
        sample_rate_hz: options.sample_rate_hz,
        block_size: options.block_size,
    };

    Ok(CompiledPlans {
        factorized,
        fused,
        report,
    })
}

// ---------------------------------------------------------------------------
// 路线枚举 / G 向量 / fused 构造
// ---------------------------------------------------------------------------

/// 非零矩阵路径（factorized 的 G route / fused 的 F route 一一对应）。
#[derive(Clone, Copy, Debug)]
struct MatrixRoute {
    out_index: u16,
    in_role: u16,
    in_source: u16,
    diag_baseline: bool,
}

fn role_positions(n_role: usize, participants: &[u16]) -> Vec<u16> {
    let mut positions = vec![COMPILE_NO_SOURCE; n_role];
    for (pos, &role) in participants.iter().enumerate() {
        positions[role as usize] = pos as u16;
    }
    positions
}

fn g_cell(g: &[f64], m: usize, l_g: usize, row: usize, col: usize) -> &[f64] {
    let offset = (row * m + col) * l_g;
    &g[offset..offset + l_g]
}

fn g_cell_nonzero(g: &[f64], m: usize, l_g: usize, row: usize, col: usize) -> bool {
    g_cell(g, m, l_g, row, col).iter().any(|v| *v != 0.0)
}

/// 构造某条 G 路径系数（长度 L_G）：`delta_D` baseline + g cell；零路径返回 None。
fn g_vector(
    positions: &[u16],
    g: &[f64],
    m: usize,
    l_g: usize,
    out_role: usize,
    in_role: usize,
    d: usize,
) -> Option<Vec<f64>> {
    let out_pos = positions[out_role];
    let in_pos = positions[in_role];
    let diag = out_role == in_role;
    let has_cell = out_pos != COMPILE_NO_SOURCE && in_pos != COMPILE_NO_SOURCE;
    let cell_nonzero = has_cell && g_cell_nonzero(g, m, l_g, out_pos as usize, in_pos as usize);
    if !diag && !cell_nonzero {
        return None;
    }
    let mut coeffs = vec![0.0; l_g];
    if diag {
        coeffs[d] = 1.0;
    }
    if cell_nonzero {
        let cell = g_cell(g, m, l_g, out_pos as usize, in_pos as usize);
        for (slot, value) in coeffs.iter_mut().zip(cell.iter()) {
            *slot += *value;
        }
    }
    Some(coeffs)
}

/// 按 `(out_index, in_role)` 升序枚举非零矩阵路径（无 source 的输入列不产生路径）。
fn matrix_routes(input: &CompileInput<'_>) -> Vec<MatrixRoute> {
    let n_role = input.n_role as usize;
    let m = input.m();
    let l_g = input.l_g as usize;
    let positions = role_positions(n_role, input.participants);
    let mut routes = Vec::new();
    for out_role in 0..n_role {
        for in_role in 0..n_role {
            let in_source = input.src_of_role[in_role];
            if in_source == COMPILE_NO_SOURCE {
                continue;
            }
            let diag = out_role == in_role;
            let out_pos = positions[out_role];
            let in_pos = positions[in_role];
            let has_cell = out_pos != COMPILE_NO_SOURCE && in_pos != COMPILE_NO_SOURCE;
            let cell_nonzero =
                has_cell && g_cell_nonzero(input.g, m, l_g, out_pos as usize, in_pos as usize);
            if !diag && !cell_nonzero {
                continue;
            }
            routes.push(MatrixRoute {
                out_index: out_role as u16,
                in_role: in_role as u16,
                in_source,
                diag_baseline: diag,
            });
        }
    }
    routes
}

fn build_fused(
    input: &CompileInput<'_>,
    routes: &[MatrixRoute],
    l_f: u32,
    full_path_len: u32,
) -> FusedGraph {
    let n_role = input.n_role as usize;
    let m = input.m();
    let l_g = input.l_g as usize;
    let l_p = input.l_p as usize;
    let d = input.d_samples as usize;
    let positions = role_positions(n_role, input.participants);
    let mut paths = Vec::with_capacity(routes.len());
    for route in routes {
        let out_role = route.out_index as usize;
        let in_role = route.in_role as usize;
        let g_coeffs = match g_vector(&positions, input.g, m, l_g, out_role, in_role, d) {
            Some(coeffs) => coeffs,
            None => continue,
        };
        let p_row = &input.p[out_role * l_p..(out_role + 1) * l_p];
        let coeffs = convolve_linear(p_row, &g_coeffs);
        debug_assert_eq!(coeffs.len() as u32, l_f);
        paths.push(FusedPath {
            in_index: route.in_source,
            out_index: route.out_index,
            in_role: route.in_role,
            taps: l_f,
            diag_baseline: route.diag_baseline,
            coeffs,
        });
    }
    // core-v1 §11.10：Path 表按 (out_index, in_index) 升序、无重复。
    paths.sort_by_key(|path| (path.out_index, path.in_index));
    FusedGraph {
        n_src: input.n_src,
        n_role: input.n_role,
        container_channels: input.container_channels,
        d_samples: input.d_samples,
        l_f,
        full_path_len,
        participants: input.participants.to_vec(),
        src_of_role: input.src_of_role.to_vec(),
        scatter: input.scatter.to_vec(),
        paths,
    }
}

// ---------------------------------------------------------------------------
// 成本模型
// ---------------------------------------------------------------------------

fn is_identity_fir(fir: &[f64]) -> bool {
    fir.len() == 1 && fir[0] == 1.0
}

/// 输入侧 A 阶段成本：identity `[1]` 的 A 不计 MAC（数学上是直通）；
/// 其它（含声明支撑 >1 的）按 `L_A` taps 计，保守不假设稀疏。
fn input_stage_cost(input: &CompileInput<'_>) -> u64 {
    let l_a = input.l_a as usize;
    let n_src = input.n_src as usize;
    let mut used = vec![false; n_src];
    for &src in input.src_of_role {
        if src != COMPILE_NO_SOURCE {
            used[src as usize] = true;
        }
    }
    let mut taps = 0u64;
    for (src, is_used) in used.iter().enumerate() {
        if !is_used {
            continue;
        }
        let fir = &input.a[src * l_a..(src + 1) * l_a];
        if !is_identity_fir(fir) {
            taps += input.l_a as u64;
        }
    }
    taps
}

/// 输出侧 P 阶段成本：identity `[1]` 的 P 不计 MAC；其它按 `L_P` taps 计。
fn output_stage_cost(input: &CompileInput<'_>) -> u64 {
    let l_p = input.l_p as usize;
    let n_role = input.n_role as usize;
    let mut taps = 0u64;
    for role in 0..n_role {
        let fir = &input.p[role * l_p..(role + 1) * l_p];
        if is_identity_fir(fir) {
            continue;
        }
        taps += input.l_p as u64;
    }
    taps
}

fn used_source_count(input: &CompileInput<'_>) -> u32 {
    let n_src = input.n_src as usize;
    let mut used = vec![false; n_src];
    for &src in input.src_of_role {
        if src != COMPILE_NO_SOURCE {
            used[src as usize] = true;
        }
    }
    used.iter().filter(|v| **v).count() as u32
}

/// 分区/FFT 模型（informative）：
/// `flops/block = 6*sum_route_taps + 5*K*log2(K)*transforms`，
/// `flops/s = flops/block * fs / B`。频域每路径每 block 复乘数 = sum taps
/// （K=2B 推导），complex MAC 计 6 real FLOP；transforms = 用到的源数 + 输出角色数
/// （共享 FFT 假设）。**不是实测数字**。
fn partitioned_cost(
    total_route_taps: u64,
    used_sources: u32,
    n_role: u32,
    max_route_taps: u32,
    block_size: u16,
    sample_rate_hz: u32,
) -> PartitionedCost {
    let fft_size = 2 * block_size as u32;
    let log2 = fft_size.ilog2() as u64;
    let transforms = used_sources as u64 + n_role as u64;
    let fft_flops = 5 * fft_size as u64 * log2 * transforms;
    let per_block = 6 * total_route_taps + fft_flops;
    let flops_per_second = per_block as f64 * sample_rate_hz as f64 / block_size as f64;
    PartitionedCost {
        fft_size,
        max_partitions: if block_size == 0 {
            0
        } else {
            (max_route_taps + block_size as u32 - 1) / block_size as u32
        },
        transforms_per_block: transforms as u32,
        real_flops_per_second: flops_per_second,
    }
}

#[allow(clippy::too_many_arguments)]
fn cost_factorized(
    a_stage_taps: u64,
    p_stage_taps: u64,
    route_count: u32,
    l_a: u32,
    l_g: u32,
    l_p: u32,
    n_src: usize,
    m: usize,
    n_role: usize,
    used_sources: u32,
    options: &CompileOptions,
) -> CostEstimate {
    let matrix_stage_taps = route_count as u64 * l_g as u64;
    let macs_per_sample = a_stage_taps + matrix_stage_taps + p_stage_taps;
    let coeff_bytes_f32 =
        (n_src as u64 * l_a as u64 + m as u64 * m as u64 * l_g as u64 + n_role as u64 * l_p as u64)
            * 4;
    let peak_taps = l_a.max(l_g).max(l_p);
    let total_route_taps = matrix_stage_taps + p_stage_taps + a_stage_taps;
    CostEstimate {
        form: FusionForm::Factorized,
        routes: route_count,
        input_stage_taps: a_stage_taps,
        matrix_stage_taps,
        output_stage_taps: p_stage_taps,
        macs_per_sample,
        flops_per_second: 2.0 * options.sample_rate_hz as f64 * macs_per_sample as f64,
        peak_taps,
        coeff_bytes_f32,
        partitioned: partitioned_cost(
            total_route_taps,
            used_sources,
            n_role as u32,
            peak_taps,
            options.block_size,
            options.sample_rate_hz,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn cost_fused(
    a_stage_taps: u64,
    route_count: u32,
    l_f: u32,
    l_a: u32,
    n_src: usize,
    n_role: usize,
    used_sources: u32,
    options: &CompileOptions,
) -> CostEstimate {
    let matrix_stage_taps = route_count as u64 * l_f as u64;
    let macs_per_sample = a_stage_taps + matrix_stage_taps;
    let coeff_bytes_f32 = (n_src as u64 * l_a as u64 + matrix_stage_taps) * 4;
    let peak_taps = l_a.max(l_f);
    let total_route_taps = matrix_stage_taps + a_stage_taps;
    CostEstimate {
        form: FusionForm::FusedGp,
        routes: route_count,
        input_stage_taps: a_stage_taps,
        matrix_stage_taps,
        output_stage_taps: 0,
        macs_per_sample,
        flops_per_second: 2.0 * options.sample_rate_hz as f64 * macs_per_sample as f64,
        peak_taps,
        coeff_bytes_f32,
        // fused 图里 G/P 已合并为 F，矩阵阶段即全部 content 计算；transform 数按
        // “源 FFT + 输出 IFFT”计，与 factorized 的共享 FFT 口径一致。
        partitioned: partitioned_cost(
            total_route_taps,
            used_sources,
            n_role as u32,
            peak_taps,
            options.block_size,
            options.sample_rate_hz,
        ),
    }
}

// ---------------------------------------------------------------------------
// 线性卷积 / zero-padded 频谱 oracle（测试与独立复核用；不参与 plan 生成）
// ---------------------------------------------------------------------------

/// 精确线性卷积（多项式乘法）。输入非空时输出长度 `a.len() + b.len() - 1`。
///
/// 这是 fused 系数生成使用的实现：不使用短 FFT 循环卷积，天然满足 zero-padding 语义。
pub fn convolve_linear(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, &av) in a.iter().enumerate() {
        if av == 0.0 {
            continue;
        }
        for (j, &bv) in b.iter().enumerate() {
            out[i + j] += av * bv;
        }
    }
    out
}

/// 不小于 n 的最小 2 的幂（n=0 -> 1）。
pub fn next_pow2(n: usize) -> usize {
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

fn dft_naive(real: &[f64], imag: &[f64], inverse: bool) -> (Vec<f64>, Vec<f64>) {
    let n = real.len();
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut out_real = vec![0.0; n];
    let mut out_imag = vec![0.0; n];
    for k in 0..n {
        let mut sum_real = 0.0;
        let mut sum_imag = 0.0;
        for t in 0..n {
            let angle = sign * core::f64::consts::TAU * (k as f64) * (t as f64) / n as f64;
            let c = angle.cos();
            let s = angle.sin();
            sum_real += real[t] * c - imag[t] * s;
            sum_imag += real[t] * s + imag[t] * c;
        }
        out_real[k] = sum_real;
        out_imag[k] = sum_imag;
    }
    (out_real, out_imag)
}

fn spectral_convolve(a: &[f64], b: &[f64], n: usize, out_len: usize) -> Vec<f64> {
    let n = n.max(1);
    let mut a_real = vec![0.0; n];
    let a_imag = vec![0.0; n];
    let mut b_real = vec![0.0; n];
    let b_imag = vec![0.0; n];
    for (i, &v) in a.iter().take(n).enumerate() {
        a_real[i] = v;
    }
    for (i, &v) in b.iter().take(n).enumerate() {
        b_real[i] = v;
    }
    let (ar, ai) = dft_naive(&a_real, &a_imag, false);
    let (br, bi) = dft_naive(&b_real, &b_imag, false);
    let mut prod_real = vec![0.0; n];
    let mut prod_imag = vec![0.0; n];
    for k in 0..n {
        prod_real[k] = ar[k] * br[k] - ai[k] * bi[k];
        prod_imag[k] = ar[k] * bi[k] + ai[k] * br[k];
    }
    let (mut yr, _) = dft_naive(&prod_real, &prod_imag, true);
    for value in yr.iter_mut() {
        *value /= n as f64;
    }
    yr.truncate(out_len.min(n));
    yr
}

/// 零填充频谱卷积 oracle：`n = next_pow2(len_a+len_b-1)`，与 [convolve_linear] 等价。
///
/// 仅测试/独立复核用（naive DFT，O(n^2)）；证明“phase 乘谱必须 zero padding 到线性卷积长度”。
pub fn oracle_spectral_convolve_zero_padded(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let linear_len = a.len() + b.len() - 1;
    let n = next_pow2(linear_len);
    spectral_convolve(a, b, n, linear_len)
}

/// 短长度循环（circular）卷积 oracle：用于反断言“未 zero-pad 会混叠”。
///
/// `n` 必须 >= `max(a.len(), b.len())`；返回 n 点循环卷积（周期折叠）。
pub fn oracle_spectral_convolve_circular(a: &[f64], b: &[f64], n: usize) -> Vec<f64> {
    assert!(n > 0, "circular convolution length must be positive");
    assert!(
        n >= a.len() && n >= b.len(),
        "circular length must be >= both input lengths"
    );
    spectral_convolve(a, b, n, n)
}

fn to_f32(values: &[f64]) -> Vec<f32> {
    values.iter().map(|v| *v as f32).collect()
}

// ---------------------------------------------------------------------------
// 编译期静态契约（只做常量绑定，不引入 RT 语义）
// ---------------------------------------------------------------------------

const _: () = {
    // 融合长度公式的边界：L_G=4096 + L_P=1024 - 1 = 5119（core-v1 §3.3 / T04 V14）。
    assert!(4096u32 + 1024u32 - 1 == 5119);
    assert!(8192u32 + 4096u32 - 1 == 12287);
    assert!(2048u32 + 2048u32 - 1 == 4095);
    // T03 §3.1：完整图长 = L_A + L_G + L_P - 2（A=delta 时与 L_F 相差 L_A-1）。
    assert!(1u32 + 4096u32 + 1024u32 - 2 == 5119);
    // DelayPlan::total_delay_q16 的位移/掩码口径（Q16）。
    assert!(65536u64 == 1u64 << 16);
    assert!((u16::MAX as u32) == 65535);
};
