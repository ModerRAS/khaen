//! [NONRT] 全图静态 hybrid DAG 计划与资源预算 —— WBS T56 写域（crates/core/src/schedule_plan.rs）。
//!
//! 本模块是 RT-first DSP 栈的 prepare 期（非 RT）计划器：把整图（input pre、input FFT、
//! 矩阵 G MAC、对角 FIR P、reduce、IFFT、P_post 保护、scatter）展开成固定 worker 数（1/2/4）
//! 的静态 B1/B2/B3 任务 DAG，并给出每 block 的 MAC 当量工作量、每 worker/phase 负载、
//! taps profile 准入（常规 4096 / extended 8192 / extended 16384 / >16384 仅 offline）、
//! 最坏完成时间预算准入、热切（crossfade）双图 CPU/RAM 峰值准入，以及 fallback 图
//! （G=I 保 P_post 保护）预分配/时限准入。
//!
//! 权威输入（只读）：
//! - scratch/lead/planner/KHAEN-RT-STACK-WBS.md T56 行 + 第 1/4/6/7 节；
//! - scratch/lead/planner/DSP-HOST-DECISION.md 第 4 节（四核/静态 DAG/phase）、第 5 节（RT 规范）；
//! - scratch/lead/planner/DSP-HOST-CPU-BUDGET.md 第 2/3/5/10 节（taps/FFT/融合/扩展数表）；
//! - rt-stack/spec/core-v1.md 第 3 节（TAPS/FFT/WORKERS/PARTITIONS 常量）、第 5 节（fallback）、
//!   第 7 节（phase/reduce）；
//! - rt-stack/spec/fir-toolbox.md 第 7.2 节（运行准入 4096/8192/16384/offline）；
//! - T09 crates/core/src/workers.rs（B1/B2/B3 静态 phase DAG、固定 reduce、ACK_MASK）；
//! - T12 crates/core/src/compile.rs（融合形态/长度加法/成本口径）；
//! - T55 crates/designer/src/report.rs（TapsProfile 语义、预算报告口径、FFT 成本模型）；
//! - T20 crates/rt-host/src/swap.rs（crossfade 预算/峰值/RAM 下界口径）。
//!
//! # 单位与成本口径（可复算）
//!
//! - 工作量一律用 f32 MAC 当量（mac_eq）：1 次实 MAC = 1；1 次复数 MAC（G/谱乘）按
//!   CPU-BUDGET 第 1 节 / T55 FLOPS_PER_COMPLEX_MAC = 8 FLOP 计 = 4 MAC 当量（1 MAC = 2 FLOP）；
//!   一次实 FFT/IFFT 按 2.5*K*log2(K) FLOP 计 = 1.25*K*log2(K) MAC 当量（K = 2B）；
//!   相位/归约/保护用保守的 per-sample 计数（见各 helper 文档）。
//! - 全部为模型估计，不是实测；真实 deadline 必须由 T29/T30/T46 的板上测量裁决，本模块只做
//!   静态资源/预算准入（超预算就拒绝，不静默降级）。
//! - 最坏完成时间 = sum_phase(max_worker(phase 工作量))/core_macs_per_second；验收门默认
//!   gate = T_block/2（core-v1 第 3.5 节；48k/128 = 1,333,333 ns）。严格小于才准入（50% 门）。
//!
//! # 全图 stage（T56 DoD：含 G / 对角 P / FFT / reduce）
//!
//! | phase | stage | 说明 |
//! | --- | --- | --- |
//! | B1 | Pre | 输入选择/格式/R 类预处理（每源 tile） |
//! | B1 | InputFirA | 输入侧 A FIR（每输出角色 tile，作用列；T09 B1 口径） |
//! | B1 | InputFft | 共享输入 FFT（每源 tile；FFT 形态） |
//! | B2 | MatrixG | 低带矩阵 G MAC（每输入列 tile，行/路径在 tile 内固定累加） |
//! | B2 | FftBinMac | 频点/分区复数 MAC（FFT 形态；按 worker 均分 bin tile） |
//! | B3 | Reduce | 固定 reduce（lane 0->3 顺序，T09 第 7.1 节；FFT 形态为跨 worker 谱累加） |
//! | B3 | DiagFirP | 输出对角 P FIR（factorized 时间域） |
//! | B3 | Ifft | 输出 IFFT（FFT 形态） |
//! | B3 | Protect | 最终保护（finite/clip/limiter 计数，P_post 保护不随 bypass 关闭） |
//! | B3 | Scatter | role->容器通道 scatter/置零 |
//!
//! 任务在 prepare 期一次性静态分配（LPT 最长任务优先、同长按下标升序、平局取低 worker id；
//! 确定性、无 work stealing、不动态均衡）。tile 数不足 worker 数时如实记录 parallel_workers
//! 与 serial_fraction_q16，不 spin 凑忙（core-v1 第 7.2 节）。
//!
//! # taps 与形态准入（core-v1 第 3.3 节 / T04 第 7.2 节）
//!
//! - TapsProfile::for_taps 与 T55 完全同口径：<=4096 Regular；恰 8192/16384 Extended
//!   （必须 extended_taps_admitted=true）；>16384 OfflineOnly（runtime 拒绝，设计工具 offline）；
//!   4097..8191 / 8193..16383 为 NotAdmitted（不得当 extended 放行）。
//! - 每 route 有效支撑按形态计算：factorized = max(L_A,L_G,L_P)；fused（T12 F=P*G 或 T08 全融合）
//!   = L_A+L_G+L_P-2（完整路径，T03 第 3.1 节）；hybrid（矩阵走 FFT、A/P 直算）= max(L_A,L_G,L_P)。
//! - 融合长度超本 profile max_taps 时保留 factorized 图重新准入（fusion_downgraded=true），
//!   禁止静默截尾；profile max_taps 必须是第 7.2 节规范档（4096/8192/16384，offline 可 >16384）。
//! - >16384 只允许 build_offline_schedule（offline_only=true、runtime_admitted=false）。
//!
//! # 热切与 fallback 预算准入（core-v1 第 5.5/7.4 节、T20 口径）
//!
//! - 热切：新旧 plan 必须同 rate/B/N/O/容器、新 plan 声明 crossfade、crossfade_frames == B 且
//!   <=128、逐输出 total_delay_q16 严格相等（D 对齐）；峰值 CPU = 新旧两图每 block 最坏时间之和
//!   （且不低于一个 block 窗口），峰值 RAM = 两图 plan_bytes 之和；任一超可用量 -> HardSwitchMute
//!   （拒绝 crossfade、只允许硬切+安全静音，状态码 ERR_CROSSFADE_BUDGET/RT_E_CROSSFADE_ALIGN）。
//! - fallback：G=I 但保留 P_post 保护（输入 A/P 与保护照常），必须能预分配且单独通过 deadline
//!   预算；否则返回 safety_mute_required=true（core-v1 第 5.5 节：无法安全输出则用预分配静音）。
//!   2 worker 是独立 plan/epoch（T09 WorkerReAdmission 口径），本模块可构建/准入 2 核 profile。
//!
//! # 错误码与集成说明
//!
//! - 配置/容量错误复用 core-v1 第 9.2 节冻结码：ERR_RATE/ERR_BLOCK_SIZE/ERR_CHANNELS/ERR_ROLES/
//!   ERR_TAPS/ERR_PATH_TABLE/ERR_CAPACITY/ERR_ADMISSION_SCHED/ERR_ADMISSION_MEMORY/
//!   ERR_BAD_LAYOUT/ERR_UNSUPPORTED。
//! - ERR_CROSSFADE_BUDGET(123) 与 RT_E_CROSSFADE_ALIGN(309) 由 T20 在 rt-host::swap 定义；
//!   core 不能依赖 rt-host（依赖方向），本模块定义同名同码的 leaf 局部别名（INTEGRATION-TODO：
//!   由 integrator 在 T26 合并为唯一公共 const 时删除本别名）。
//! - 本模块只写 crates/core/src/schedule_plan.rs；公共 lib.rs/mod 注册、CLI、Cargo 由
//!   integrator 串行合入（T06/T26）。扩展 taps（8192/16384）的 FFT/kernel 实现属 T08/T26
//!   follow-up，本任务只交付计划/预算准入；未上板、未实测、无 RT/benchmark/soak（NOT_RUN）。

use crate::abi::{KhaenDiagFirV1, KhaenPathV1, KhaenRoleV1};
use crate::compile::FusionForm;
use crate::rt::{
    gate_ns_floor, max_partitions, t_block_ns_ceil, RtStatus, BLOCK_SIZES, MAX_DIAG_FIRS,
    MAX_INPUT_CHANNELS, MAX_LOW_BAND_PARTICIPANTS, MAX_MATRIX_PATHS, MAX_OUTPUT_CHANNELS,
    MAX_OUTPUT_ROLES, MAX_PARTITIONS, PARALLEL_PHASES, RT_WORKERS, SAMPLE_RATES_HZ,
    TAPS_EXTENDED_MAX, TAPS_NORMAL_MAX,
};
use crate::workers::{
    ack_mask, WORKER_COUNT_FALLBACK, WORKER_COUNT_MAX, WORKER_PHASE_COUNT, WORKER_REDUCE_LANES,
    WORKER_ROLES_MAX,
};

// ---------------------------------------------------------------------------
// 固定容量与模型常量（与 core-v1 / T09 / T20 口径绑定；见文件尾静态断言）
// ---------------------------------------------------------------------------

/// 全图静态任务表容量（最坏 131：B1 <= 20+22、B2 <= 22、B3 <= 22*3+1；512 留扩展余量）。
pub const SCHEDULE_MAX_TASKS: usize = 512;
/// phase 数（= core-v1 PARALLEL_PHASES = B1/B2/B3 = 3）。
pub const SCHEDULE_PHASES: usize = WORKER_PHASE_COUNT;
/// 生产 worker 上限（= core-v1 RT_WORKERS = 4；2 核为独立降级 profile）。
pub const SCHEDULE_WORKERS_MAX: usize = WORKER_COUNT_MAX;
/// 降级 profile worker 数（= core-v1 RT_WORKERS_FALLBACK = 2）。
pub const SCHEDULE_WORKERS_FALLBACK: u8 = WORKER_COUNT_FALLBACK as u8;
/// reduce lane 数（= T09 WORKER_REDUCE_LANES = 4；固定 reduce 顺序 0->1->2->3）。
pub const SCHEDULE_REDUCE_LANES: u8 = WORKER_REDUCE_LANES as u8;
/// stage 种类数（本模块 StageKind 变体数）。
pub const SCHEDULE_STAGE_KINDS: usize = 10;
/// 每个 worker 在 B2 分到的 FFT bin tile 数（每 worker 2 个；4 worker -> 8 tile）。
pub const SCHEDULE_FFT_BIN_TILES_PER_WORKER: u16 = 2;
/// 输入侧 pre（选择/格式/R 类）每源每样本 MAC 当量（保守 1）。
pub const SCHEDULE_PRE_MACS_PER_SAMPLE: u64 = 1;
/// 输出最终保护（finite/clip/limiter 计数）每输出每样本 MAC 当量（保守 2）。
pub const SCHEDULE_PROTECT_MACS_PER_SAMPLE: u64 = 2;
/// crossfade 最大帧数（= core-v1 第 3.4 节 CROSSFADE_MAX_FRAMES = 128；T20 在 swap 定义，此 leaf 别名）。
pub const SCHEDULE_CROSSFADE_MAX_FRAMES: u32 = 128;
/// offline 设计工具硬上限（>16384 仅 offline；再大本计划器拒绝，避免无界内存估计）。
pub const SCHEDULE_OFFLINE_TAPS_MAX: u32 = 262_144;
/// 声明持续算力默认值（每核 f32 MAC/s）：8 GFLOPS 持续 = 4 GMAC/s（CPU-BUDGET 第 1/2 节低端
/// 假设，8-15GFLOPS 持续率下限，不是实测；真实准入由 T45/T46 用实测值覆盖）。
pub const SCHEDULE_DEFAULT_CORE_MACS_PER_SEC: u64 = 4_000_000_000;
/// 1 次复数 MAC 的 FLOP 口径（CPU-BUDGET 第 1 节 / T55 FLOPS_PER_COMPLEX_MAC = 8）。
pub const SCHEDULE_FLOPS_PER_COMPLEX_MAC: u64 = 8;
/// 1 MAC = 2 FLOP（CPU-BUDGET 第 1 节）。
pub const SCHEDULE_FLOPS_PER_MAC: u64 = 2;
/// 1 次复数 MAC 折算的 MAC 当量（= 8 FLOP / 2 FLOP = 4）。
pub const SCHEDULE_MAC_EQ_PER_COMPLEX_MAC: u64 =
    SCHEDULE_FLOPS_PER_COMPLEX_MAC / SCHEDULE_FLOPS_PER_MAC;
/// 默认 deadline 门分子（T_block/2 = 50% deadline）。
pub const SCHEDULE_GATE_NUM: u32 = 1;
/// 默认 deadline 门分母。
pub const SCHEDULE_GATE_DEN: u32 = 2;
/// crossfade 预算拒绝码（= T20 ERR_CROSSFADE_BUDGET，code 123）。
pub const ERR_CROSSFADE_BUDGET: RtStatus = RtStatus::from_code(123);
/// crossfade D 未对齐/跨 rate/N/O/B 拒绝码（= T20 RT_E_CROSSFADE_ALIGN，code 309）。
pub const RT_E_CROSSFADE_ALIGN: RtStatus = RtStatus::from_code(309);

// ---------------------------------------------------------------------------
// taps profile / kernel form / graph class / stage
// ---------------------------------------------------------------------------

/// taps 运行准入分类（与 T55 report::TapsProfile::for_taps 完全同口径；T04 第 7.2 节）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapsProfile {
    /// <= 4096：常规 profile。
    Regular,
    /// 恰 8192 或 16384：显式 extended profile，必须 extended_taps_admitted=true。
    Extended,
    /// > 16384：仅设计工具 offline 输出，runtime 拒绝（ERR_TAPS）。
    OfflineOnly,
    /// 4097..8191 或 8193..16383：第 7.2 节未列出的中间值，不得当 extended 放行。
    NotAdmitted,
}

impl TapsProfile {
    /// 按每 route 有效支撑 taps 分类（T55 同口径）。
    pub fn for_taps(taps: u32) -> Self {
        if taps <= TAPS_NORMAL_MAX {
            TapsProfile::Regular
        } else if taps == 8_192 || taps == TAPS_EXTENDED_MAX {
            TapsProfile::Extended
        } else if taps > TAPS_EXTENDED_MAX {
            TapsProfile::OfflineOnly
        } else {
            TapsProfile::NotAdmitted
        }
    }

    /// 稳定字符串（证据/报告用）。
    pub fn as_str(self) -> &'static str {
        match self {
            TapsProfile::Regular => "regular",
            TapsProfile::Extended => "extended",
            TapsProfile::OfflineOnly => "offline_only",
            TapsProfile::NotAdmitted => "not_admitted",
        }
    }

    /// runtime 是否可能准入（offline/未列出值均不可）。
    pub fn is_runtime_candidate(self) -> bool {
        matches!(self, TapsProfile::Regular | TapsProfile::Extended)
    }
}

/// 全图 kernel 形态（prepare 期静态选择；运行期不改）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelForm {
    /// 全时域 direct：A/G/P 直算（T07/T09 口径；taps <=4096）。
    Direct,
    /// 共享 FFT 全融合（T08 UPOLS 口径：每 route h = p*G*a 全融合；fused taps 走 FFT）。
    SharedFft,
    /// hybrid：A/P 时域直算 + 矩阵 G 走共享 FFT（低带长矩阵与中高带对角 FIR 同图分核再合并）。
    Hybrid,
}

impl KernelForm {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            KernelForm::Direct => "direct",
            KernelForm::SharedFft => "shared_fft",
            KernelForm::Hybrid => "hybrid",
        }
    }

    /// 是否走 FFT 矩阵阶段（SharedFft/Hybrid 需要共享 FFT plan/scratch）。
    pub fn uses_fft(self) -> bool {
        matches!(self, KernelForm::SharedFft | KernelForm::Hybrid)
    }
}

/// 图类别（normal 生产图 / fallback 保护图 / offline 设计图）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphClass {
    /// 正常生产图（含 P/G 目标）。
    Normal,
    /// fallback 保护图：G=I（仅 delta_D 基线），保留 P_post 最终保护（core-v1 第 5.5 节）。
    Fallback,
    /// 设计工具 offline 图（>16384 taps；不进入 runtime 准入）。
    OfflineDesign,
}

impl GraphClass {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            GraphClass::Normal => "normal",
            GraphClass::Fallback => "fallback",
            GraphClass::OfflineDesign => "offline_design",
        }
    }
}

/// 全图 stage 种类（phase 归属固定，见 phase_of）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    /// B1：输入选择/格式/R 类预处理（每源 tile）。
    Pre,
    /// B1：输入侧 A FIR（每输出角色 tile，作用列）。
    InputFirA,
    /// B1：共享输入 FFT（每源 tile；FFT 形态）。
    InputFft,
    /// B2：低带矩阵 G MAC（每输入列 tile）。
    MatrixG,
    /// B2：频点/分区复数 MAC（FFT 形态）。
    FftBinMac,
    /// B3：固定 reduce（lane 0->3 顺序；FFT 形态为跨 worker 谱累加）。
    Reduce,
    /// B3：输出对角 P FIR（factorized 时间域）。
    DiagFirP,
    /// B3：输出 IFFT（FFT 形态）。
    Ifft,
    /// B3：最终保护（finite/clip/limiter；P_post 保护不随 bypass 关闭）。
    Protect,
    /// B3：role->容器通道 scatter/置零。
    Scatter,
}

impl StageKind {
    /// stage 全表（下标稳定，用于证据/stage_work 数组）。
    pub const ALL: [StageKind; SCHEDULE_STAGE_KINDS] = [
        StageKind::Pre,
        StageKind::InputFirA,
        StageKind::InputFft,
        StageKind::MatrixG,
        StageKind::FftBinMac,
        StageKind::Reduce,
        StageKind::DiagFirP,
        StageKind::Ifft,
        StageKind::Protect,
        StageKind::Scatter,
    ];

    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            StageKind::Pre => "pre",
            StageKind::InputFirA => "input_fir_a",
            StageKind::InputFft => "input_fft",
            StageKind::MatrixG => "matrix_g",
            StageKind::FftBinMac => "fft_bin_mac",
            StageKind::Reduce => "reduce",
            StageKind::DiagFirP => "diag_fir_p",
            StageKind::Ifft => "ifft",
            StageKind::Protect => "protect",
            StageKind::Scatter => "scatter",
        }
    }

    /// 稳定下标（0..SCHEDULE_STAGE_KINDS）。
    pub const fn index(self) -> usize {
        match self {
            StageKind::Pre => 0,
            StageKind::InputFirA => 1,
            StageKind::InputFft => 2,
            StageKind::MatrixG => 3,
            StageKind::FftBinMac => 4,
            StageKind::Reduce => 5,
            StageKind::DiagFirP => 6,
            StageKind::Ifft => 7,
            StageKind::Protect => 8,
            StageKind::Scatter => 9,
        }
    }

    /// phase 归属（B1=0 / B2=1 / B3=2）。
    pub const fn phase_of(self) -> u8 {
        match self {
            StageKind::Pre | StageKind::InputFirA | StageKind::InputFft => 0,
            StageKind::MatrixG | StageKind::FftBinMac => 1,
            StageKind::Reduce
            | StageKind::DiagFirP
            | StageKind::Ifft
            | StageKind::Protect
            | StageKind::Scatter => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// profile（prepare 期输入；Copy/POD，无堆分配）
// ---------------------------------------------------------------------------

/// 全图 profile（prepare 期输入；[NONRT]）。
///
/// 与 T12 CompileOptions / T55 BudgetReport 对齐：max_taps 是本 profile 每 route 有效支撑上限
/// （第 7.2 节规范档 4096/8192/16384）；matrix_paths 是低带非零矩阵路径数（<= M*列数、
/// <= MAX_MATRIX_PATHS，不是固定 25）；diag_firs 是输出对角 FIR 数（<= O）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanProfile {
    /// 采样率（48000/96000）。
    pub sample_rate_hz: u32,
    /// block 大小（64/128；生产准入）。
    pub block_size: u16,
    /// 源通道数 N（1..=20）。
    pub n_src: u16,
    /// 输出角色数 O（1..=16）。
    pub n_role: u16,
    /// 播放容器通道数 C（>= O，<=22）。
    pub container_channels: u16,
    /// 低带 participants 数 M（1..=8，<= O）。
    pub m: u16,
    /// 输入侧 A FIR 声明支撑 L_A（>=1）。
    pub l_a: u32,
    /// 低带 g 声明支撑 L_G（>= D+1）。
    pub l_g: u32,
    /// 输出对角 P 声明支撑 L_P（>=1）。
    pub l_p: u32,
    /// 低带公共基线整数延迟 D（样本，计入 L_G 支撑）。
    pub d_samples: u32,
    /// 低带非零矩阵路径数（g 的非零 (j,k) 数；<= n_role*m 且 <=128）。
    pub matrix_paths: u32,
    /// 输出对角 FIR 数（P 非 identity 的输出数；<= O）。
    pub diag_firs: u32,
    /// 本 profile 每 route 有效支撑上限（第 7.2 节规范档 4096/8192/16384）。
    pub max_taps: u32,
    /// extended profile 是否已显式准入（support > 4096 时必须 true）。
    pub extended_taps_admitted: bool,
    /// 期望融合形态（T12：Factorized/FusedGp；SharedFft 强制 FusedGp、Hybrid 强制 Factorized）。
    pub fusion: FusionForm,
    /// 声明 kernel 形态（prepare 期静态选择）。
    pub kernel: KernelForm,
    /// worker 数（1/2/4；1 仅 benchmark/对拍，2 为独立降级 profile，4 为生产）。
    pub workers: u8,
    /// 逐输出最大 total_delay_q16（D 对齐口径；crossfade 要求新旧严格相等；u32 上限）。
    pub total_delay_q16: u64,
    /// stereo strict 标志（plan flags bit0；只选择/零填，不做上混）。
    pub stereo_strict: bool,
    /// 是否允许 crossfade（plan flags bit2；真热切仍需 evaluate_hot_swap 双图预算通过）。
    pub crossfade: bool,
}

impl Default for PlanProfile {
    /// 参考 profile：48k/128、4.1.2（7 角色 / 5 participants / 密 25 路径 / L_G=4096 / L_P=1024）、
    /// 4 worker direct factorized。只是默认构造，不代表已准入。
    fn default() -> Self {
        Self {
            sample_rate_hz: 48_000,
            block_size: 128,
            n_src: 7,
            n_role: 7,
            container_channels: 8,
            m: 5,
            l_a: 1,
            l_g: 4_096,
            l_p: 1_024,
            d_samples: 0,
            matrix_paths: 25,
            diag_firs: 7,
            max_taps: TAPS_NORMAL_MAX,
            extended_taps_admitted: false,
            fusion: FusionForm::Factorized,
            kernel: KernelForm::Direct,
            workers: RT_WORKERS,
            total_delay_q16: 512 << 16,
            stereo_strict: false,
            crossfade: false,
        }
    }
}

impl PlanProfile {
    /// 单 block 名义周期（ns，向上取整）。
    pub fn block_ns(&self) -> u64 {
        t_block_ns_ceil(self.sample_rate_hz, self.block_size)
    }

    /// 默认 50% deadline 验收门（ns）。
    pub fn gate_ns(&self) -> u64 {
        gate_ns_floor(self.sample_rate_hz, self.block_size)
    }

    /// 是否 FFT 形态。
    pub fn uses_fft(&self) -> bool {
        self.kernel.uses_fft()
    }

    /// 2 核降级 profile（独立 plan/epoch；其余字段不变）。
    pub fn with_workers(mut self, workers: u8) -> Self {
        self.workers = workers;
        self
    }

    /// 同一 profile 的其它 kernel 形态（预算对比/证据用）。
    pub fn with_kernel(mut self, kernel: KernelForm) -> Self {
        self.kernel = kernel;
        self
    }
}

// ---------------------------------------------------------------------------
// 数学/容量 helper（checked；溢出 -> ERR_CAPACITY，不 panic）
// ---------------------------------------------------------------------------

fn checked_mul(a: u64, b: u64) -> Result<u64, RtStatus> {
    a.checked_mul(b).ok_or(RtStatus::ERR_CAPACITY)
}

fn checked_add(a: u64, b: u64) -> Result<u64, RtStatus> {
    a.checked_add(b).ok_or(RtStatus::ERR_CAPACITY)
}

fn checked_prod(parts: &[u64]) -> Result<u64, RtStatus> {
    let mut acc = 1u64;
    for &part in parts {
        acc = checked_mul(acc, part)?;
    }
    Ok(acc)
}

fn align64(bytes: u64) -> Result<u64, RtStatus> {
    let rem = bytes % 64;
    if rem == 0 {
        Ok(bytes)
    } else {
        bytes.checked_add(64 - rem).ok_or(RtStatus::ERR_CAPACITY)
    }
}

/// 不小于 n 的最小 2 的幂。
fn next_pow2_u32(n: u32) -> u32 {
    if n <= 1 {
        return 1;
    }
    let mut p = 1u32;
    while p < n {
        p <<= 1;
    }
    p
}

/// 共享 FFT 长度 K（= 2B；B 取 64/128 时恰为 2B）。
pub fn fft_size_for(profile: &PlanProfile) -> u32 {
    next_pow2_u32(2 * profile.block_size as u32)
}

/// 一次实 FFT/IFFT 的 FLOP（2.5*K*log2(K)；CPU-BUDGET 第 1 节 / T55 FFT_MODEL_COEFFICIENT）。
pub fn fft_transform_flops(fft_size: u32) -> u64 {
    let k = fft_size as u64;
    let log2 = fft_size.trailing_zeros() as u64;
    5 * k * log2 / 2
}

/// 一次实 FFT/IFFT 的 MAC 当量（FLOP/2 = 1.25*K*log2(K)；K 为 2 的幂时整除）。
pub fn fft_transform_macs(fft_size: u32) -> u64 {
    fft_transform_flops(fft_size) / SCHEDULE_FLOPS_PER_MAC
}

/// 逐列路径数（把 matrix_paths 按 n_role 列均分；余数给低列；确定性）。
fn column_paths(profile: &PlanProfile, column: usize) -> u32 {
    let columns = profile.n_role as usize;
    if columns == 0 {
        return 0;
    }
    let paths = profile.matrix_paths as usize;
    let base = paths / columns;
    let rem = paths % columns;
    if column < rem {
        (base + 1) as u32
    } else {
        base as u32
    }
}

/// 每 route 有效支撑（按 kernel/fusion 口径；core-v1 第 3.3 节 / T03 第 3.1 节）。
///
/// - Direct + Factorized：max(L_A,L_G,L_P)（三段各自独立）；
/// - Direct + FusedGp：L_A+L_G+L_P-2（完整路径；A 仍单独前置，但按完整链计准入）；
/// - SharedFft：L_A+L_G+L_P-2（T08 全融合路由）；
/// - Hybrid：max(L_A,L_G,L_P)（A/P 直算、G 分区 FFT，各自支撑独立）。
pub fn route_support(profile: &PlanProfile) -> Result<u32, RtStatus> {
    let sum = checked_add(
        checked_add(profile.l_a as u64, profile.l_g as u64)?,
        profile.l_p as u64,
    )?;
    let full = sum.checked_sub(2).ok_or(RtStatus::ERR_TAPS)?;
    if full == 0 {
        return Err(RtStatus::ERR_TAPS);
    }
    let support = match (profile.kernel, profile.fusion) {
        (KernelForm::Direct, FusionForm::Factorized) => {
            profile.l_a.max(profile.l_g).max(profile.l_p) as u64
        }
        (KernelForm::Direct, FusionForm::FusedGp) => full,
        (KernelForm::SharedFft, _) => full,
        (KernelForm::Hybrid, _) => profile.l_a.max(profile.l_g).max(profile.l_p) as u64,
    };
    if support > u32::MAX as u64 {
        return Err(RtStatus::ERR_TAPS);
    }
    Ok(support as u32)
}

/// profile 的 taps 分类（按 route support；T55 同口径）。
pub fn taps_profile_of(profile: &PlanProfile) -> Result<TapsProfile, RtStatus> {
    Ok(TapsProfile::for_taps(route_support(profile)?))
}

/// 融合长度 L_F = L_G + L_P - 1（T12/T03；仅 FusedGp/SharedFft 有意义）。
pub fn fused_len(profile: &PlanProfile) -> Result<u32, RtStatus> {
    let sum = checked_add(profile.l_g as u64, profile.l_p as u64)?;
    let l_f = sum.checked_sub(1).ok_or(RtStatus::ERR_TAPS)?;
    if l_f == 0 || l_f > u32::MAX as u64 {
        return Err(RtStatus::ERR_TAPS);
    }
    Ok(l_f as u32)
}

/// 完整路径长度 = L_A + L_G + L_P - 2（T03 第 3.1 节；T12 full_path_len）。
pub fn full_path_len(profile: &PlanProfile) -> Result<u32, RtStatus> {
    let sum = checked_add(
        checked_add(profile.l_a as u64, profile.l_g as u64)?,
        profile.l_p as u64,
    )?;
    let len = sum.checked_sub(2).ok_or(RtStatus::ERR_TAPS)?;
    if len == 0 || len > u32::MAX as u64 {
        return Err(RtStatus::ERR_TAPS);
    }
    Ok(len as u32)
}

/// 推荐 kernel 形态（纯静态、不参与预算裁决）：direct 支撑 <=4096 -> Direct；否则 hybrid 支撑可准入
/// -> Hybrid；再否则 -> SharedFft（调用方仍需 build_schedule/admit_deadline 准入）。
pub fn recommended_kernel(profile: &PlanProfile) -> KernelForm {
    let direct = PlanProfile {
        kernel: KernelForm::Direct,
        fusion: FusionForm::Factorized,
        ..*profile
    };
    if let Ok(support) = route_support(&direct) {
        if support <= TAPS_NORMAL_MAX {
            return KernelForm::Direct;
        }
    }
    let hybrid = PlanProfile {
        kernel: KernelForm::Hybrid,
        fusion: FusionForm::Factorized,
        ..*profile
    };
    if let Ok(support) = route_support(&hybrid) {
        if TapsProfile::for_taps(support).is_runtime_candidate() {
            return KernelForm::Hybrid;
        }
    }
    KernelForm::SharedFft
}

// ---------------------------------------------------------------------------
// 静态任务与计划（POD；无堆分配）
// ---------------------------------------------------------------------------

/// 一条静态任务（tile）：phase 内固定 owner、固定工作量，RT 只读。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleTask {
    /// phase（0=B1 / 1=B2 / 2=B3）。
    pub phase: u8,
    /// stage 种类。
    pub kind: StageKind,
    /// 固定 owner worker（0..workers-1）。
    pub owner: u8,
    /// 本 block 工作量（f32 MAC 当量）。
    pub work_macs: u64,
    /// 通道/角色/列/路径组下标（按 stage 语义）。
    pub index: u16,
    /// 附加下标（FFT bin tile 序号等；无则 0）。
    pub aux: u16,
}

/// 全图静态 hybrid DAG 计划（prepare 期构建；运行期只读、无 work stealing）。
#[derive(Clone, Debug, PartialEq)]
pub struct SchedulePlan {
    /// 输入 profile（Copy）。
    pub profile: PlanProfile,
    /// taps 准入分类（按最终 route support）。
    pub taps_profile: TapsProfile,
    /// 生效 kernel 形态。
    pub kernel_form: KernelForm,
    /// 图类别。
    pub graph_class: GraphClass,
    /// 生效融合形态（可能因长度超限从 FusedGp 降为 Factorized）。
    pub fusion_effective: FusionForm,
    /// 是否因 fused 长度/taps 超限降级为 factorized（禁止静默截尾）。
    pub fusion_downgraded: bool,
    /// 是否仅 offline 设计图。
    pub offline_only: bool,
    /// runtime admission 是否通过（仅 admit_runtime 置 true；build 后为 false）。
    pub runtime_admitted: bool,
    /// 静态任务表（前 task_count 项有效）。
    pub tasks: [ScheduleTask; SCHEDULE_MAX_TASKS],
    /// 有效任务数。
    pub task_count: u16,
    /// 每 phase 任务数。
    pub phase_task_count: [u16; SCHEDULE_PHASES],
    /// 每 phase 工作量合计（MAC 当量/block）。
    pub phase_work_total: [u64; SCHEDULE_PHASES],
    /// 每 phase 每 worker 任务数。
    pub worker_task_count: [[u16; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
    /// 每 phase 每 worker 工作量（MAC 当量/block）。
    pub worker_work: [[u64; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
    /// 每 worker 全 phase 工作量合计。
    pub worker_work_total: [u64; SCHEDULE_WORKERS_MAX],
    /// 每 stage 工作量合计（下标 = StageKind::index）。
    pub stage_work: [u64; SCHEDULE_STAGE_KINDS],
    /// 固定 reduce 顺序（= [0,1,2,3]；2 worker 只前 2 项有效）。
    pub reduce_order: [u8; SCHEDULE_WORKERS_MAX],
    /// reduce lane 数（固定 4）。
    pub reduce_lanes: u8,
    /// 参数化 all-ack 位图 ACK_MASK(workers)（2->0x3、4->0xF；禁止硬写 0xF）。
    pub ack_mask: u32,
    /// B2 矩阵/频点 tile 数（direct/hybrid = 列 tile 或 bin tile）。
    pub matrix_tiles: u16,
    /// 低带非零矩阵路径数（= profile.matrix_paths；fallback 为 delta 行数）。
    pub matrix_paths: u32,
    /// 输出对角 P tile 数（factorized 时为 diag_firs，fused/SharedFft 为 0）。
    pub diag_tiles: u16,
    /// 每 block FFT 变换数（SharedFft/Hybrid；否则 0）。
    pub fft_transforms: u16,
    /// 最终 route support（taps 准入依据）。
    pub route_support: u32,
    /// 每路径矩阵阶段每样本 MAC 当量（normal = L_G 或 fused L_F；fallback = 1 仅 delta）。
    pub matrix_work_taps: u32,
    /// 每 block 全图工作量（MAC 当量）。
    pub work_per_block: u64,
    /// 等效持续 MAC/s（= work_per_block * fs / B，整型截断；FFT 项为均摊）。
    pub macs_per_second: u64,
    /// 等效持续 FLOP/s（= 2 * macs_per_second）。
    pub flops_per_second: u64,
    /// 矩阵阶段每样本 MAC 当量（direct 口径 = matrix_paths * matrix_work_taps；FFT 形态为 0）。
    pub matrix_macs_per_sample: u64,
    /// 输入侧 A 阶段每样本 MAC 当量（Direct/Hybrid 直算时非零）。
    pub input_macs_per_sample: u64,
    /// 输出侧 P 阶段每样本 MAC 当量（factorized 时非零）。
    pub output_macs_per_sample: u64,
    /// f32 系数/谱字节。
    pub coeff_bytes: u64,
    /// plan 表字节（header/path/diag/role/ctl_meta，64B 对齐）。
    pub table_bytes: u64,
    /// b-p 历史/状态字节。
    pub history_bytes: u64,
    /// worker 私有 scratch/partial 字节。
    pub scratch_bytes: u64,
    /// 单图 plan 侧总字节（coeff+tables+history+scratch，64B 对齐合计）。
    pub plan_bytes: u64,
    /// 双图峰值 RAM 下界（= 2 * plan_bytes；crossfade 用）。
    pub dual_graph_bytes: u64,
    /// FFT 长度 K（2B；非 FFT 形态也记录以便审计）。
    pub fft_size: u16,
    /// 矩阵/融合分区数 Q（非 FFT 形态为 0）。
    pub partitions: u16,
    /// 每 phase 串行比例 Q16：max(0, tasks-W)/tasks（T09 serial_fraction_q16 同口径）。
    pub serial_fraction_q16: [u32; SCHEDULE_PHASES],
    /// 每 phase 实际并行 worker 数（= min(tasks, W)）。
    pub parallel_workers: [u8; SCHEDULE_PHASES],
}

impl SchedulePlan {
    /// 生效 worker 数。
    pub const fn workers(&self) -> u8 {
        self.profile.workers
    }

    /// 每 block 工作量的 phase 和（应等于 work_per_block；证据自检用）。
    pub fn phase_work_sum(&self) -> u64 {
        let mut sum = 0u64;
        let mut phase = 0usize;
        while phase < SCHEDULE_PHASES {
            sum = sum.saturating_add(self.phase_work_total[phase]);
            phase += 1;
        }
        sum
    }

    /// 每 worker 工作量之和（应等于 work_per_block）。
    pub fn worker_work_sum(&self) -> u64 {
        let mut sum = 0u64;
        let mut worker = 0usize;
        while worker < self.workers() as usize {
            sum = sum.saturating_add(self.worker_work_total[worker]);
            worker += 1;
        }
        sum
    }

    /// 证据用摘要（人类可读；每行一条；NONRT 允许分配）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "schedule_plan: class={} kernel={} fusion={} downgraded={} offline={} runtime_admitted={}",
            self.graph_class.as_str(),
            self.kernel_form.as_str(),
            match self.fusion_effective {
                FusionForm::Factorized => "factorized",
                FusionForm::FusedGp => "fused_gp",
            },
            self.fusion_downgraded,
            self.offline_only,
            self.runtime_admitted
        ));
        out.push(format!(
            "profile: fs={} B={} N={} O={} C={} M={} workers={} L_A={} L_G={} L_P={} D={} paths={} diag={} max_taps={} ext_admitted={}",
            self.profile.sample_rate_hz,
            self.profile.block_size,
            self.profile.n_src,
            self.profile.n_role,
            self.profile.container_channels,
            self.profile.m,
            self.workers(),
            self.profile.l_a,
            self.profile.l_g,
            self.profile.l_p,
            self.profile.d_samples,
            self.matrix_paths,
            self.diag_tiles,
            self.profile.max_taps,
            self.profile.extended_taps_admitted
        ));
        out.push(format!(
            "taps: class={} support={} fused_len={} fft_size={} partitions={} ack_mask=0x{:X} reduce_lanes={}",
            self.taps_profile.as_str(),
            self.route_support,
            fused_len(&self.profile).unwrap_or(0),
            self.fft_size,
            self.partitions,
            self.ack_mask,
            self.reduce_lanes
        ));
        out.push(format!(
            "dag: tasks={} phases={:?} matrix_tiles={} diag_tiles={} fft_transforms={} work_per_block={} mac_eq",
            self.task_count,
            self.phase_task_count,
            self.matrix_tiles,
            self.diag_tiles,
            self.fft_transforms,
            self.work_per_block
        ));
        out.push(format!(
            "cost: matrix={} input={} output={} mac_eq/sample; mac_eq/s={} flop/s={}",
            self.matrix_macs_per_sample,
            self.input_macs_per_sample,
            self.output_macs_per_sample,
            self.macs_per_second,
            self.flops_per_second
        ));
        out.push(format!(
            "memory: coeff={} tables={} history={} scratch={} plan={} dual_graph={}",
            self.coeff_bytes,
            self.table_bytes,
            self.history_bytes,
            self.scratch_bytes,
            self.plan_bytes,
            self.dual_graph_bytes
        ));
        out.push(format!(
            "phase: work={:?} worker_work={:?} worker_tasks={:?} parallel={:?} serial_q16={:?}",
            self.phase_work_total,
            self.worker_work,
            self.worker_task_count,
            self.parallel_workers,
            self.serial_fraction_q16
        ));
        out
    }
}

// ---------------------------------------------------------------------------
// profile 校验 + build
// ---------------------------------------------------------------------------

fn validate_common(profile: &PlanProfile) -> Result<(), RtStatus> {
    if !SAMPLE_RATES_HZ.contains(&profile.sample_rate_hz) {
        return Err(RtStatus::ERR_RATE);
    }
    if !BLOCK_SIZES.contains(&profile.block_size) {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    if profile.workers != 1
        && profile.workers != SCHEDULE_WORKERS_FALLBACK
        && profile.workers != RT_WORKERS
    {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    if profile.n_src == 0 || profile.n_src as usize > MAX_INPUT_CHANNELS {
        return Err(RtStatus::ERR_CHANNELS);
    }
    if profile.n_role == 0 || profile.n_role as usize > MAX_OUTPUT_ROLES {
        return Err(RtStatus::ERR_ROLES);
    }
    if profile.n_role as usize > WORKER_ROLES_MAX {
        return Err(RtStatus::ERR_ROLES);
    }
    if profile.container_channels < profile.n_role
        || profile.container_channels == 0
        || profile.container_channels as usize > MAX_OUTPUT_CHANNELS
    {
        return Err(RtStatus::ERR_CHANNELS);
    }
    if profile.m == 0
        || profile.m as usize > MAX_LOW_BAND_PARTICIPANTS
        || profile.m > profile.n_role
    {
        return Err(RtStatus::ERR_ROLES);
    }
    if profile.l_a == 0 || profile.l_g == 0 || profile.l_p == 0 {
        return Err(RtStatus::ERR_TAPS);
    }
    // D 必须计入 L_G 支撑（T03 第 3.1 节：L_G >= D + 1）。
    if profile.d_samples as u64 >= profile.l_g as u64 {
        return Err(RtStatus::ERR_TAPS);
    }
    // 每 route 支撑上限不能低于单段声明支撑。
    let min_support = profile.l_a.max(profile.l_g).max(profile.l_p);
    if profile.max_taps < min_support {
        return Err(RtStatus::ERR_TAPS);
    }
    if profile.max_taps == 0 || profile.max_taps > SCHEDULE_OFFLINE_TAPS_MAX {
        return Err(RtStatus::ERR_TAPS);
    }
    // 矩阵路径：非零 g 的 (j,k) 数 <= n_role*m 且 <= MAX_MATRIX_PATHS。
    if profile.matrix_paths as usize > MAX_MATRIX_PATHS {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    let max_paths = profile.n_role as u64 * profile.m as u64;
    if profile.matrix_paths as u64 > max_paths {
        return Err(RtStatus::ERR_PATH_TABLE);
    }
    if profile.diag_firs as usize > MAX_DIAG_FIRS || profile.diag_firs > profile.n_role as u32 {
        return Err(RtStatus::ERR_ROLES);
    }
    // total_delay_q16 是 ABI u32 字段（core-v1 第 11.10 节注 6，T20 口径），且应包含基线 D。
    if profile.total_delay_q16 > u32::MAX as u64 {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    if profile.total_delay_q16 < ((profile.d_samples as u64) << 16) {
        return Err(RtStatus::ERR_BAD_LAYOUT);
    }
    Ok(())
}

/// 计算生效 fusion 与实际 route support；含 fused 长度超限时的合法降级。
fn resolve_form(profile: &PlanProfile, offline: bool) -> Result<(FusionForm, u32, bool), RtStatus> {
    // SharedFft 是 T08 全融合口径；Hybrid 是 factorized（G 走 FFT、A/P 直算）。
    let forced = match profile.kernel {
        KernelForm::Direct => None,
        KernelForm::SharedFft => Some(FusionForm::FusedGp),
        KernelForm::Hybrid => Some(FusionForm::Factorized),
    };
    if let Some(required) = forced {
        if profile.fusion != required {
            return Err(RtStatus::ERR_UNSUPPORTED);
        }
    }
    let direct_probe = PlanProfile {
        fusion: FusionForm::Factorized,
        ..*profile
    };
    let direct_support = route_support(&direct_probe)?;
    let requested_support = route_support(profile)?;
    let cap = profile.max_taps;
    let mut downgraded = false;
    let (fusion, support) = if profile.fusion == FusionForm::FusedGp {
        let fused_class = TapsProfile::for_taps(requested_support);
        let fused_ok = if offline {
            fused_class != TapsProfile::NotAdmitted && requested_support <= cap
        } else {
            fused_class.is_runtime_candidate() && requested_support <= cap
        };
        if fused_ok {
            (FusionForm::FusedGp, requested_support)
        } else if profile.kernel == KernelForm::Direct {
            // 禁止静默截尾：保留 factorized 图重新准入（T12 FusedOverTapLimit 同口径）。
            downgraded = true;
            (FusionForm::Factorized, direct_support)
        } else {
            return Err(RtStatus::ERR_TAPS);
        }
    } else {
        (FusionForm::Factorized, requested_support)
    };
    Ok((fusion, support, downgraded))
}

fn validate_taps(
    profile: &PlanProfile,
    support: u32,
    offline: bool,
) -> Result<TapsProfile, RtStatus> {
    // profile max_taps 必须是第 7.2 节规范档；offline 允许 >16384。
    let cap_class = TapsProfile::for_taps(profile.max_taps);
    let cap_ok = match cap_class {
        TapsProfile::Regular | TapsProfile::Extended => true,
        TapsProfile::OfflineOnly => offline,
        TapsProfile::NotAdmitted => false,
    };
    if !cap_ok {
        return Err(RtStatus::ERR_TAPS);
    }
    let class = TapsProfile::for_taps(support);
    if class == TapsProfile::NotAdmitted {
        return Err(RtStatus::ERR_TAPS);
    }
    if support > profile.max_taps {
        return Err(RtStatus::ERR_TAPS);
    }
    if !offline {
        if class == TapsProfile::OfflineOnly {
            return Err(RtStatus::ERR_TAPS);
        }
        if class == TapsProfile::Extended && !profile.extended_taps_admitted {
            return Err(RtStatus::ERR_TAPS);
        }
    }
    Ok(class)
}

/// 构建全图静态 DAG（runtime profile；>16384 或未准入档返回 ERR_TAPS）。
pub fn build_schedule(profile: &PlanProfile) -> Result<SchedulePlan, RtStatus> {
    build(profile, GraphClass::Normal, false)
}

/// 构建 offline 设计图（允许 >16384 taps；offline_only=true，永不 runtime 准入）。
pub fn build_offline_schedule(profile: &PlanProfile) -> Result<SchedulePlan, RtStatus> {
    build(profile, GraphClass::OfflineDesign, true)
}

fn build(
    profile: &PlanProfile,
    graph_class: GraphClass,
    offline: bool,
) -> Result<SchedulePlan, RtStatus> {
    validate_common(profile)?;
    let (fusion_effective, support, fusion_downgraded) = resolve_form(profile, offline)?;
    let taps_class = validate_taps(profile, support, offline)?;
    let kernel = profile.kernel;
    let offline_only = graph_class == GraphClass::OfflineDesign;

    // 分区/FFT 容量检查（T08 口径：Q = ceil(taps/B) <= MAX_PARTITIONS）。
    let mut partitions = 0u32;
    if kernel.uses_fft() {
        let matrix_taps = match kernel {
            KernelForm::SharedFft => support,
            KernelForm::Hybrid => profile.l_g,
            KernelForm::Direct => 0,
        };
        partitions = max_partitions(matrix_taps, profile.block_size);
        if partitions == 0 || partitions > MAX_PARTITIONS {
            return Err(RtStatus::ERR_TAPS);
        }
    }

    // 任务数上界预检（避免中途容量错误）。
    let est_tasks = task_capacity_bound(profile, kernel, fusion_effective)?;
    if est_tasks as usize > SCHEDULE_MAX_TASKS {
        return Err(RtStatus::ERR_CAPACITY);
    }

    let mut plan = SchedulePlan {
        profile: *profile,
        taps_profile: taps_class,
        kernel_form: kernel,
        graph_class,
        fusion_effective,
        fusion_downgraded,
        offline_only,
        runtime_admitted: false,
        tasks: [ScheduleTask {
            phase: 0,
            kind: StageKind::Pre,
            owner: 0,
            work_macs: 0,
            index: 0,
            aux: 0,
        }; SCHEDULE_MAX_TASKS],
        task_count: 0,
        phase_task_count: [0; SCHEDULE_PHASES],
        phase_work_total: [0; SCHEDULE_PHASES],
        worker_task_count: [[0; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
        worker_work: [[0; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
        worker_work_total: [0; SCHEDULE_WORKERS_MAX],
        stage_work: [0; SCHEDULE_STAGE_KINDS],
        reduce_order: [0, 1, 2, 3],
        reduce_lanes: SCHEDULE_REDUCE_LANES,
        ack_mask: ack_mask(profile.workers),
        matrix_tiles: 0,
        matrix_paths: profile.matrix_paths,
        diag_tiles: 0,
        fft_transforms: 0,
        route_support: support,
        matrix_work_taps: 0,
        work_per_block: 0,
        macs_per_second: 0,
        flops_per_second: 0,
        matrix_macs_per_sample: 0,
        input_macs_per_sample: 0,
        output_macs_per_sample: 0,
        coeff_bytes: 0,
        table_bytes: 0,
        history_bytes: 0,
        scratch_bytes: 0,
        plan_bytes: 0,
        dual_graph_bytes: 0,
        fft_size: fft_size_for(profile) as u16,
        partitions: partitions as u16,
        serial_fraction_q16: [0; SCHEDULE_PHASES],
        parallel_workers: [0; SCHEDULE_PHASES],
    };

    emit_stage_tasks(&mut plan)?;
    assign_tasks(&mut plan)?;
    compute_costs(&mut plan)?;
    compute_memory(&mut plan)?;
    finalize_phase_stats(&mut plan);

    debug_assert_eq!(plan.phase_work_sum(), plan.work_per_block);
    debug_assert_eq!(plan.worker_work_sum(), plan.work_per_block);
    Ok(plan)
}

fn task_capacity_bound(
    profile: &PlanProfile,
    kernel: KernelForm,
    fusion: FusionForm,
) -> Result<u32, RtStatus> {
    let n_src = profile.n_src as u64;
    let n_role = profile.n_role as u64;
    let mut count = n_src; // Pre
    if kernel == KernelForm::Direct || kernel == KernelForm::Hybrid {
        count = checked_add(count, n_role)?; // InputFirA
    }
    if kernel.uses_fft() {
        count = checked_add(count, n_src)?; // InputFft
        let bin_tiles = profile.workers as u64 * SCHEDULE_FFT_BIN_TILES_PER_WORKER as u64;
        count = checked_add(count, bin_tiles)?; // FftBinMac
    } else if profile.matrix_paths > 0 {
        count = checked_add(count, n_role)?; // MatrixG 列 tile
    }
    let mut b3 = n_role; // Reduce
    if kernel.uses_fft() {
        b3 = checked_add(b3, n_role)?; // Ifft
    }
    if fusion == FusionForm::Factorized {
        b3 = checked_add(b3, profile.diag_firs as u64)?; // DiagFirP
    }
    b3 = checked_add(b3, n_role)?; // Protect
    b3 = checked_add(b3, 1)?; // Scatter
    count = checked_add(count, b3)?;
    if count > u32::MAX as u64 {
        return Err(RtStatus::ERR_CAPACITY);
    }
    Ok(count as u32)
}

fn push_task(
    plan: &mut SchedulePlan,
    kind: StageKind,
    index: u16,
    aux: u16,
    work_macs: u64,
) -> Result<(), RtStatus> {
    let slot = plan.task_count as usize;
    if slot >= SCHEDULE_MAX_TASKS {
        return Err(RtStatus::ERR_CAPACITY);
    }
    plan.tasks[slot] = ScheduleTask {
        phase: kind.phase_of(),
        kind,
        owner: 0,
        work_macs,
        index,
        aux,
    };
    plan.task_count += 1;
    Ok(())
}

fn emit_stage_tasks(plan: &mut SchedulePlan) -> Result<(), RtStatus> {
    let profile = plan.profile;
    let block = profile.block_size as u64;
    let workers = profile.workers as u16;
    let n_src = profile.n_src as u16;
    let n_role = profile.n_role as u16;
    let kernel = plan.kernel_form;
    let fusion = plan.fusion_effective;
    let is_fallback = plan.graph_class == GraphClass::Fallback;

    // ---- B1: Pre（每源）----
    let pre_work = checked_mul(SCHEDULE_PRE_MACS_PER_SAMPLE, block)?;
    let mut src = 0u16;
    while src < n_src {
        push_task(plan, StageKind::Pre, src, 0, pre_work)?;
        src += 1;
    }

    // ---- B1: InputFirA（每输出角色；Direct/Hybrid 时 A 时域直算）----
    if kernel == KernelForm::Direct || kernel == KernelForm::Hybrid {
        let a_work = checked_mul(profile.l_a as u64, block)?;
        let mut role = 0u16;
        while role < n_role {
            push_task(plan, StageKind::InputFirA, role, 0, a_work)?;
            role += 1;
        }
    }

    // ---- B1: InputFft（每源共享输入 FFT；SharedFft/Hybrid）----
    if kernel.uses_fft() {
        let fft_work = fft_transform_macs(plan.fft_size as u32);
        let mut src = 0u16;
        while src < n_src {
            push_task(plan, StageKind::InputFft, src, 0, fft_work)?;
            src += 1;
        }
        plan.fft_transforms = plan.fft_transforms.saturating_add(n_src);
    }

    // ---- B2: 矩阵 G 列 tile / FFT 频点 MAC tile ----
    let matrix_tiles: u16;
    let matrix_work_taps: u32;
    let matrix_path_count: u64 = if is_fallback {
        n_role as u64
    } else {
        profile.matrix_paths as u64
    };
    if kernel == KernelForm::Direct {
        matrix_work_taps = if is_fallback {
            // fallback：G=I（仅 delta_D 基线）；支撑按 D+1 计内存，但每样本只 1 个非零 tap。
            1
        } else if fusion == FusionForm::FusedGp {
            fused_len(&profile)?
        } else {
            profile.l_g
        };
        if matrix_path_count > 0 {
            let mut column = 0usize;
            while column < n_role as usize {
                let col_paths = if is_fallback {
                    1
                } else {
                    column_paths(&profile, column)
                };
                let work = checked_prod(&[col_paths as u64, matrix_work_taps as u64, block])?;
                push_task(plan, StageKind::MatrixG, column as u16, 0, work)?;
                column += 1;
            }
            matrix_tiles = n_role;
        } else {
            matrix_tiles = 0;
        }
    } else {
        // FFT 形态：矩阵内容工作量 = 路径 x 分区 Q x (B+1) 复数 MAC；复数 MAC = 4 MAC 当量。
        let route_taps: u64 = if is_fallback {
            1
        } else if kernel == KernelForm::SharedFft {
            plan.route_support as u64
        } else {
            profile.l_g as u64
        };
        let q = max_partitions(route_taps as u32, profile.block_size) as u64;
        let f = profile.block_size as u64 + 1;
        let complex_macs = checked_prod(&[matrix_path_count, q, f])?;
        let bin_work = checked_mul(complex_macs, SCHEDULE_MAC_EQ_PER_COMPLEX_MAC)?;
        let tiles = workers
            .saturating_mul(SCHEDULE_FFT_BIN_TILES_PER_WORKER)
            .max(1);
        let base = bin_work / tiles as u64;
        let rem = bin_work % tiles as u64;
        let mut tile = 0u16;
        while tile < tiles {
            let work = base + if (tile as u64) < rem { 1 } else { 0 };
            push_task(plan, StageKind::FftBinMac, tile, q as u16, work)?;
            tile += 1;
        }
        matrix_tiles = tiles;
        matrix_work_taps = route_taps as u32;
        plan.partitions = q as u16;
    }

    // ---- B3: Reduce（每输出角色；固定 lane 0->3 顺序）----
    let reduce_work = checked_mul((SCHEDULE_REDUCE_LANES - 1) as u64, block)?;
    let mut role = 0u16;
    while role < n_role {
        push_task(plan, StageKind::Reduce, role, 0, reduce_work)?;
        role += 1;
    }

    // ---- B3: Ifft（每输出角色；FFT 形态）----
    if kernel.uses_fft() {
        let fft_work = fft_transform_macs(plan.fft_size as u32);
        let mut role = 0u16;
        while role < n_role {
            push_task(plan, StageKind::Ifft, role, 0, fft_work)?;
            role += 1;
        }
        plan.fft_transforms = plan.fft_transforms.saturating_add(n_role);
    }

    // ---- B3: DiagFirP（factorized 时间域；fused 已并入矩阵 F）----
    if fusion == FusionForm::Factorized {
        let p_work = checked_mul(profile.l_p as u64, block)?;
        let mut role = 0u16;
        while role < profile.diag_firs as u16 {
            push_task(plan, StageKind::DiagFirP, role, 0, p_work)?;
            role += 1;
        }
        plan.diag_tiles = profile.diag_firs as u16;
    }

    // ---- B3: Protect（每输出角色；P_post 最终保护不随 bypass 关闭）----
    let protect_work = checked_mul(SCHEDULE_PROTECT_MACS_PER_SAMPLE, block)?;
    let mut role = 0u16;
    while role < n_role {
        push_task(plan, StageKind::Protect, role, 0, protect_work)?;
        role += 1;
    }

    // ---- B3: Scatter（role->容器通道 + 未用容器置零）----
    let scatter_work = checked_mul(n_role as u64, block)?;
    push_task(plan, StageKind::Scatter, 0, 0, scatter_work)?;

    plan.matrix_tiles = matrix_tiles;
    plan.matrix_work_taps = matrix_work_taps;
    Ok(())
}

fn assign_tasks(plan: &mut SchedulePlan) -> Result<(), RtStatus> {
    let workers = plan.workers() as usize;
    if workers == 0 || workers > SCHEDULE_WORKERS_MAX {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    let mut phase = 0usize;
    while phase < SCHEDULE_PHASES {
        let mut order = [0u16; SCHEDULE_MAX_TASKS];
        let mut count = 0usize;
        let mut task = 0usize;
        while task < plan.task_count as usize {
            if plan.tasks[task].phase as usize == phase {
                order[count] = task as u16;
                count += 1;
            }
            task += 1;
        }
        // 插入排序：work 降序、kind 升序、index 升序（确定性、无分配）。
        let mut i = 1usize;
        while i < count {
            let key = order[i];
            let key_task = plan.tasks[key as usize];
            let mut j = i;
            while j > 0 {
                let probe = plan.tasks[order[j - 1] as usize];
                let key_first = key_task.work_macs > probe.work_macs
                    || (key_task.work_macs == probe.work_macs
                        && (key_task.kind.index() < probe.kind.index()
                            || (key_task.kind.index() == probe.kind.index()
                                && key_task.index < probe.index)));
                if key_first {
                    order[j] = order[j - 1];
                    j -= 1;
                } else {
                    break;
                }
            }
            order[j] = key;
            i += 1;
        }
        let mut loads = [0u64; SCHEDULE_WORKERS_MAX];
        let mut index = 0usize;
        while index < count {
            let slot = order[index] as usize;
            let work = plan.tasks[slot].work_macs;
            let mut best = 0usize;
            let mut best_load = loads[0];
            let mut worker = 1usize;
            while worker < workers {
                if loads[worker] < best_load {
                    best = worker;
                    best_load = loads[worker];
                }
                worker += 1;
            }
            plan.tasks[slot].owner = best as u8;
            loads[best] = checked_add(loads[best], work)?;
            index += 1;
        }
        phase += 1;
    }
    Ok(())
}

fn compute_costs(plan: &mut SchedulePlan) -> Result<(), RtStatus> {
    let profile = plan.profile;
    let block = profile.block_size as u64;
    let fs = profile.sample_rate_hz as u64;
    let mut stage_work = [0u64; SCHEDULE_STAGE_KINDS];
    let mut phase_work = [0u64; SCHEDULE_PHASES];
    let mut worker_work = [[0u64; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES];
    let mut worker_tasks = [[0u16; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES];
    let mut phase_tasks = [0u16; SCHEDULE_PHASES];
    let mut task = 0usize;
    while task < plan.task_count as usize {
        let entry = plan.tasks[task];
        let phase = entry.phase as usize;
        let worker = entry.owner as usize;
        let slot = entry.kind.index();
        stage_work[slot] = checked_add(stage_work[slot], entry.work_macs)?;
        phase_work[phase] = checked_add(phase_work[phase], entry.work_macs)?;
        worker_work[phase][worker] = checked_add(worker_work[phase][worker], entry.work_macs)?;
        worker_tasks[phase][worker] = worker_tasks[phase][worker].saturating_add(1);
        phase_tasks[phase] = phase_tasks[phase].saturating_add(1);
        task += 1;
    }
    let mut total = 0u64;
    let mut worker_total = [0u64; SCHEDULE_WORKERS_MAX];
    let mut phase = 0usize;
    while phase < SCHEDULE_PHASES {
        total = checked_add(total, phase_work[phase])?;
        let mut worker = 0usize;
        while worker < plan.workers() as usize {
            worker_total[worker] = checked_add(worker_total[worker], worker_work[phase][worker])?;
            worker += 1;
        }
        phase += 1;
    }
    plan.stage_work = stage_work;
    plan.phase_work_total = phase_work;
    plan.phase_task_count = phase_tasks;
    plan.worker_work = worker_work;
    plan.worker_task_count = worker_tasks;
    plan.worker_work_total = worker_total;
    plan.work_per_block = total;
    // 等效持续率：work/block * fs / B（FFT 项为每 block 均摊，整型截断见模块文档）。
    plan.macs_per_second = checked_prod(&[total, fs])? / block;
    plan.flops_per_second = checked_mul(plan.macs_per_second, SCHEDULE_FLOPS_PER_MAC)?;
    plan.matrix_macs_per_sample = if plan.kernel_form == KernelForm::Direct {
        let paths = if plan.graph_class == GraphClass::Fallback {
            profile.n_role as u64
        } else {
            profile.matrix_paths as u64
        };
        if plan.matrix_tiles == 0 {
            0
        } else {
            checked_prod(&[paths, plan.matrix_work_taps as u64])?
        }
    } else {
        0
    };
    plan.input_macs_per_sample =
        if plan.kernel_form == KernelForm::Direct || plan.kernel_form == KernelForm::Hybrid {
            checked_mul(profile.l_a as u64, profile.n_role as u64)?
        } else {
            0
        };
    plan.output_macs_per_sample = if plan.fusion_effective == FusionForm::Factorized {
        checked_mul(profile.l_p as u64, profile.diag_firs as u64)?
    } else {
        0
    };
    Ok(())
}

fn compute_memory(plan: &mut SchedulePlan) -> Result<(), RtStatus> {
    let profile = plan.profile;
    let block = profile.block_size as u64;
    let workers = profile.workers as u64;
    let n_src = profile.n_src as u64;
    let n_role = profile.n_role as u64;
    let k = plan.fft_size as u64;
    let is_fallback = plan.graph_class == GraphClass::Fallback;
    let paths = if is_fallback {
        n_role
    } else {
        profile.matrix_paths as u64
    };
    let matrix_taps = plan.matrix_work_taps as u64;

    // ---- 系数/谱 ----
    let coeff_bytes = match plan.kernel_form {
        KernelForm::Direct => {
            let a_bytes = checked_mul(n_src, profile.l_a as u64)?;
            let matrix_bytes = if is_fallback {
                checked_mul(n_role, profile.d_samples as u64 + 1)?
            } else {
                checked_mul(paths, matrix_taps)?
            };
            let p_bytes = if plan.fusion_effective == FusionForm::Factorized {
                checked_mul(profile.diag_firs as u64, profile.l_p as u64)?
            } else {
                0
            };
            checked_mul(
                checked_add(checked_add(a_bytes, matrix_bytes)?, p_bytes)?,
                4,
            )?
        }
        KernelForm::Hybrid => {
            // G 谱（twiddle K + routes x Q x 2K）+ A dense + P dense。
            let q = plan.partitions as u64;
            let spectrum_samples = checked_add(k, checked_prod(&[paths, q, k, 2])?)?;
            let spectra_bytes = checked_mul(spectrum_samples, 4)?;
            let a_bytes = checked_mul(checked_mul(n_src, profile.l_a as u64)?, 4)?;
            let p_bytes = if plan.fusion_effective == FusionForm::Factorized {
                checked_mul(
                    checked_mul(profile.diag_firs as u64, profile.l_p as u64)?,
                    4,
                )?
            } else {
                0
            };
            checked_add(spectra_bytes, checked_add(a_bytes, p_bytes)?)?
        }
        KernelForm::SharedFft => {
            let q = plan.partitions as u64;
            let spectrum_samples = checked_add(k, checked_prod(&[paths, q, k, 2])?)?;
            checked_mul(spectrum_samples, 4)?
        }
    };

    // ---- plan 表（header/path/diag/role；64B 对齐；ctl_meta=0）----
    let path_rows = if is_fallback { n_role } else { paths };
    let mut table_bytes = align64(128)?;
    table_bytes = checked_add(
        table_bytes,
        align64(checked_mul(
            path_rows,
            core::mem::size_of::<KhaenPathV1>() as u64,
        )?)?,
    )?;
    table_bytes = checked_add(
        table_bytes,
        align64(checked_mul(
            plan.diag_tiles as u64,
            core::mem::size_of::<KhaenDiagFirV1>() as u64,
        )?)?,
    )?;
    table_bytes = checked_add(
        table_bytes,
        align64(checked_mul(
            n_role,
            core::mem::size_of::<KhaenRoleV1>() as u64,
        )?)?,
    )?;

    // ---- 历史/状态 ----
    let history_bytes = match plan.kernel_form {
        KernelForm::Direct => {
            let x_hist = checked_mul(n_src, profile.l_a as u64 - 1)?;
            let route_hist = checked_mul(
                n_role,
                matrix_taps.saturating_sub(1)
                    + if plan.fusion_effective == FusionForm::Factorized {
                        profile.l_p as u64 - 1
                    } else {
                        0
                    },
            )?;
            checked_mul(checked_add(x_hist, route_hist)?, 4)?
        }
        KernelForm::Hybrid | KernelForm::SharedFft => {
            let q = plan.partitions as u64;
            // T08 PartitionedLayout 口径：x_prev(n_src*B) + x_hist(n_src*Q*2K) + work(4K) + slot(1)。
            let state_samples = checked_add(
                checked_add(checked_mul(n_src, block)?, checked_prod(&[n_src, q, k, 2])?)?,
                checked_add(checked_mul(4, k)?, 1)?,
            )?;
            checked_mul(state_samples, 4)?
        }
    };

    // ---- scratch：worker 私有 partial（B x O x f32）+ FFT scratch（4K x f32/worker）----
    let partial_bytes = checked_prod(&[workers, block, n_role, 4])?;
    let fft_scratch = if plan.kernel_form.uses_fft() {
        checked_prod(&[workers, 4, k, 4])?
    } else {
        0
    };
    let scratch_bytes = checked_add(partial_bytes, fft_scratch)?;

    let mut plan_bytes = align64(coeff_bytes)?;
    plan_bytes = checked_add(plan_bytes, align64(table_bytes)?)?;
    plan_bytes = checked_add(plan_bytes, align64(history_bytes)?)?;
    plan_bytes = checked_add(plan_bytes, align64(scratch_bytes)?)?;

    plan.coeff_bytes = coeff_bytes;
    plan.table_bytes = table_bytes;
    plan.history_bytes = history_bytes;
    plan.scratch_bytes = scratch_bytes;
    plan.plan_bytes = plan_bytes;
    plan.dual_graph_bytes = checked_mul(plan_bytes, 2)?;
    Ok(())
}

fn finalize_phase_stats(plan: &mut SchedulePlan) {
    let workers = plan.workers() as u64;
    let mut phase = 0usize;
    while phase < SCHEDULE_PHASES {
        let tasks = plan.phase_task_count[phase] as u64;
        plan.parallel_workers[phase] = if tasks < workers {
            tasks as u8
        } else {
            workers as u8
        };
        plan.serial_fraction_q16[phase] = if tasks <= workers || tasks == 0 {
            0
        } else {
            (((tasks - workers) * 65_536) / tasks) as u32
        };
        phase += 1;
    }
}

// ---------------------------------------------------------------------------
// deadline 预算准入
// ---------------------------------------------------------------------------

/// deadline 预算（prepare 期声明；[NONRT]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeadlineBudgets {
    /// 每核声明持续算力（f32 MAC/s；1 MAC = 2 FLOP）。不得为 0。
    pub core_macs_per_second: u64,
    /// 验收门分子（默认 1）。
    pub gate_num: u32,
    /// 验收门分母（默认 2 = 50% deadline）。
    pub gate_den: u32,
}

impl Default for DeadlineBudgets {
    fn default() -> Self {
        Self {
            core_macs_per_second: SCHEDULE_DEFAULT_CORE_MACS_PER_SEC,
            gate_num: SCHEDULE_GATE_NUM,
            gate_den: SCHEDULE_GATE_DEN,
        }
    }
}

impl DeadlineBudgets {
    /// 以 8 GFLOPS 持续率假设构造（默认）。
    pub const fn conservative() -> Self {
        Self {
            core_macs_per_second: SCHEDULE_DEFAULT_CORE_MACS_PER_SEC,
            gate_num: SCHEDULE_GATE_NUM,
            gate_den: SCHEDULE_GATE_DEN,
        }
    }

    /// 显式算力构造（T45/T46 实测值覆盖默认假设时用）。
    pub const fn with_core_rate(core_macs_per_second: u64) -> Self {
        Self {
            core_macs_per_second,
            gate_num: SCHEDULE_GATE_NUM,
            gate_den: SCHEDULE_GATE_DEN,
        }
    }

    fn validate(&self) -> Result<(), RtStatus> {
        if self.core_macs_per_second == 0 || self.gate_den == 0 || self.gate_num == 0 {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        if self.gate_num > self.gate_den {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        Ok(())
    }

    fn gate_ns(&self, profile: &PlanProfile) -> u64 {
        // gate = T_block * num / den = B * 1e9 * num / (fs * den)（向下取整，不放宽门）。
        let num = self.gate_num as u128;
        let den = self.gate_den as u128;
        if num == 0 || den == 0 || profile.sample_rate_hz == 0 {
            return 0;
        }
        let bytes = profile.block_size as u128 * 1_000_000_000u128 * num;
        (bytes / (profile.sample_rate_hz as u128 * den)) as u64
    }
}

/// deadline 预算准入结果（[NONRT]；供控制面导出/证据断言）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeadlineAdmission {
    /// 每块验收门（ns）。
    pub gate_ns: u64,
    /// 每 phase 每 worker 估计时间（ns）。
    pub phase_worker_ns: [[u64; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
    /// 每 phase 最坏 worker 时间（ns）。
    pub phase_worst_ns: [u64; SCHEDULE_PHASES],
    /// 每 worker 全 phase 时间合计（ns）。
    pub worker_block_ns: [u64; SCHEDULE_WORKERS_MAX],
    /// 整块最坏完成时间 = sum phase 最坏（ns）。
    pub total_worst_ns: u64,
    /// 最坏 worker（= argmax worker_block_ns；无任务为 0）。
    pub worst_worker: u8,
    /// 是否严格小于 gate（50% deadline 门）。
    pub admitted: bool,
    /// 是否 overflow（估计超出 u64 -> 视为不可准入）。
    pub overflow: bool,
}

/// 计算 deadline 预算准入（纯算术；不做任何降级）。
pub fn admit_deadline(plan: &SchedulePlan, budgets: &DeadlineBudgets) -> DeadlineAdmission {
    let mut result = DeadlineAdmission {
        gate_ns: budgets.gate_ns(&plan.profile),
        phase_worker_ns: [[0; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
        phase_worst_ns: [0; SCHEDULE_PHASES],
        worker_block_ns: [0; SCHEDULE_WORKERS_MAX],
        total_worst_ns: 0,
        worst_worker: 0,
        admitted: false,
        overflow: false,
    };
    if budgets.validate().is_err() {
        return result;
    }
    let workers = plan.workers() as usize;
    let core = budgets.core_macs_per_second as u128;
    let mut phase = 0usize;
    while phase < SCHEDULE_PHASES {
        let mut worker = 0usize;
        let mut worst = 0u64;
        while worker < workers {
            let work = plan.worker_work[phase][worker];
            let ns128 = (work as u128) * 1_000_000_000u128 / core;
            let ns = if ns128 > u64::MAX as u128 {
                result.overflow = true;
                u64::MAX
            } else {
                ns128 as u64
            };
            result.phase_worker_ns[phase][worker] = ns;
            result.worker_block_ns[worker] = result.worker_block_ns[worker].saturating_add(ns);
            if ns > worst {
                worst = ns;
            }
            worker += 1;
        }
        result.phase_worst_ns[phase] = worst;
        result.total_worst_ns = result.total_worst_ns.saturating_add(worst);
        phase += 1;
    }
    let mut worst_worker = 0usize;
    let mut worst_block = 0u64;
    let mut worker = 0usize;
    while worker < workers {
        if result.worker_block_ns[worker] > worst_block {
            worst_block = result.worker_block_ns[worker];
            worst_worker = worker;
        }
        worker += 1;
    }
    result.worst_worker = worst_worker as u8;
    result.admitted = !result.overflow && result.total_worst_ns < result.gate_ns;
    result
}

// ---------------------------------------------------------------------------
// 热切（crossfade）预算准入
// ---------------------------------------------------------------------------

/// 热切预算声明（[NONRT]；T20 KhaenCrossfadeBudgetV1 的 prepare 期输入侧口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotSwapSpec {
    /// 旧 plan 版本（echo 到结果；版本标签由 T20 registry 校验）。
    pub old_plan_version: u32,
    /// 新 plan 版本（echo 到结果）。
    pub new_plan_version: u32,
    /// crossfade 帧数（必须 == 新 plan block_size 且 1..=128）。
    pub crossfade_frames: u32,
    /// 双图峰值 CPU 可用量（ns/block；0 = 未声明 -> 拒绝）。
    pub available_cpu_ns: u64,
    /// 双图峰值 RAM 可用量（字节；0 = 未声明 -> 拒绝）。
    pub available_ram_bytes: u64,
    /// 峰值 CPU 估计所用每核持续算力（0 = 用默认 4 GMAC/s）。
    pub core_macs_per_second: u64,
}

impl Default for HotSwapSpec {
    fn default() -> Self {
        Self {
            old_plan_version: 0,
            new_plan_version: 0,
            crossfade_frames: 0,
            available_cpu_ns: 0,
            available_ram_bytes: 0,
            core_macs_per_second: 0,
        }
    }
}

/// 热切处置（不兼容/超预算时只允许硬切+静音或新 epoch）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotSwapDisposition {
    /// 允许 crossfade（同 D、同形态、双图峰值均在预算内）。
    Crossfade,
    /// 不允许 crossfade：只允许硬切 + 安全静音（同 rate/B/N/O 但 D 未对齐或峰值超预算）。
    HardSwitchMute,
    /// 跨 rate/B/N/O 不兼容：必须停流建新 epoch，不得 crossfade。
    NewEpoch,
    /// 新 plan 未声明 crossfade（plan flags bit2=0）：只允许硬切。
    NotRequested,
}

impl HotSwapDisposition {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            HotSwapDisposition::Crossfade => "crossfade",
            HotSwapDisposition::HardSwitchMute => "hard_switch_mute",
            HotSwapDisposition::NewEpoch => "new_epoch",
            HotSwapDisposition::NotRequested => "not_requested",
        }
    }
}

/// 热切预算准入结果（[NONRT]；供控制面导出/证据断言）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotSwapAdmission {
    /// 处置结论。
    pub disposition: HotSwapDisposition,
    /// 是否允许 crossfade（= disposition == Crossfade）。
    pub admitted: bool,
    /// 旧 plan 版本（echo）。
    pub old_plan_version: u32,
    /// 新 plan 版本（echo）。
    pub new_plan_version: u32,
    /// 新旧 total_delay_q16 是否严格相等（D 对齐）。
    pub d_aligned: bool,
    /// 旧 plan total_delay_q16（逐输出最大值口径）。
    pub old_total_delay_q16: u64,
    /// 新 plan total_delay_q16。
    pub new_total_delay_q16: u64,
    /// crossfade 帧数。
    pub crossfade_frames: u32,
    /// 双图峰值 CPU 估计（ns/block；= max(一个 block 窗口, 新旧两图 total_worst 之和)）。
    pub peak_cpu_ns: u64,
    /// 双图峰值 RAM 估计（字节；= 两图 plan_bytes 之和）。
    pub peak_ram_bytes: u64,
    /// 声明的可用 CPU（ns）。
    pub available_cpu_ns: u64,
    /// 声明的可用 RAM（字节）。
    pub available_ram_bytes: u64,
    /// 结论状态码（OK / ERR_CROSSFADE_BUDGET / RT_E_CROSSFADE_ALIGN / ERR_UNSUPPORTED）。
    pub status: RtStatus,
}

impl HotSwapAdmission {
    fn rejected(disposition: HotSwapDisposition, status: RtStatus, spec: &HotSwapSpec) -> Self {
        Self {
            disposition,
            admitted: false,
            old_plan_version: spec.old_plan_version,
            new_plan_version: spec.new_plan_version,
            d_aligned: false,
            old_total_delay_q16: 0,
            new_total_delay_q16: 0,
            crossfade_frames: spec.crossfade_frames,
            peak_cpu_ns: 0,
            peak_ram_bytes: 0,
            available_cpu_ns: spec.available_cpu_ns,
            available_ram_bytes: spec.available_ram_bytes,
            status,
        }
    }
}

/// 评估热切预算准入（永不返回 Err；失败用 disposition/status 表达，便于控制面改走硬切/静音）。
pub fn evaluate_hot_swap(
    old: &SchedulePlan,
    new: &SchedulePlan,
    spec: &HotSwapSpec,
) -> HotSwapAdmission {
    if !new.profile.crossfade {
        return HotSwapAdmission::rejected(HotSwapDisposition::NotRequested, RtStatus::OK, spec);
    }
    // 跨 rate/B/N/O/容器变化禁止 crossfade（core-v1 第 5.3 节：必须新 epoch）。
    if old.profile.sample_rate_hz != new.profile.sample_rate_hz
        || old.profile.block_size != new.profile.block_size
        || old.profile.n_src != new.profile.n_src
        || old.profile.n_role != new.profile.n_role
        || old.profile.container_channels != new.profile.container_channels
    {
        return HotSwapAdmission::rejected(
            HotSwapDisposition::NewEpoch,
            RT_E_CROSSFADE_ALIGN,
            spec,
        );
    }
    let old_delay = old.profile.total_delay_q16;
    let new_delay = new.profile.total_delay_q16;
    let d_aligned = old_delay == new_delay;
    let frames_ok = spec.crossfade_frames == new.profile.block_size as u32
        && spec.crossfade_frames >= 1
        && spec.crossfade_frames <= SCHEDULE_CROSSFADE_MAX_FRAMES;

    let rate = if spec.core_macs_per_second == 0 {
        SCHEDULE_DEFAULT_CORE_MACS_PER_SEC
    } else {
        spec.core_macs_per_second
    };
    let rate_budgets = DeadlineBudgets::with_core_rate(rate);
    let old_deadline = admit_deadline(old, &rate_budgets);
    let new_deadline = admit_deadline(new, &rate_budgets);
    let dual_ns = old_deadline
        .total_worst_ns
        .saturating_add(new_deadline.total_worst_ns);
    // 双图峰值不能低于一个 block 窗口（T20 下界口径）。
    let block_window = t_block_ns_ceil(new.profile.sample_rate_hz, new.profile.block_size);
    let peak_cpu_ns = dual_ns.max(block_window);
    let peak_ram_bytes = old.plan_bytes.saturating_add(new.plan_bytes);

    if !d_aligned || !frames_ok {
        return HotSwapAdmission {
            disposition: HotSwapDisposition::HardSwitchMute,
            admitted: false,
            old_plan_version: spec.old_plan_version,
            new_plan_version: spec.new_plan_version,
            d_aligned,
            old_total_delay_q16: old_delay,
            new_total_delay_q16: new_delay,
            crossfade_frames: spec.crossfade_frames,
            peak_cpu_ns,
            peak_ram_bytes,
            available_cpu_ns: spec.available_cpu_ns,
            available_ram_bytes: spec.available_ram_bytes,
            status: RT_E_CROSSFADE_ALIGN,
        };
    }
    if spec.available_cpu_ns == 0
        || spec.available_ram_bytes == 0
        || peak_cpu_ns > spec.available_cpu_ns
        || peak_ram_bytes > spec.available_ram_bytes
    {
        return HotSwapAdmission {
            disposition: HotSwapDisposition::HardSwitchMute,
            admitted: false,
            old_plan_version: spec.old_plan_version,
            new_plan_version: spec.new_plan_version,
            d_aligned,
            old_total_delay_q16: old_delay,
            new_total_delay_q16: new_delay,
            crossfade_frames: spec.crossfade_frames,
            peak_cpu_ns,
            peak_ram_bytes,
            available_cpu_ns: spec.available_cpu_ns,
            available_ram_bytes: spec.available_ram_bytes,
            status: ERR_CROSSFADE_BUDGET,
        };
    }
    HotSwapAdmission {
        disposition: HotSwapDisposition::Crossfade,
        admitted: true,
        old_plan_version: spec.old_plan_version,
        new_plan_version: spec.new_plan_version,
        d_aligned,
        old_total_delay_q16: old_delay,
        new_total_delay_q16: new_delay,
        crossfade_frames: spec.crossfade_frames,
        peak_cpu_ns,
        peak_ram_bytes,
        available_cpu_ns: spec.available_cpu_ns,
        available_ram_bytes: spec.available_ram_bytes,
        status: RtStatus::OK,
    }
}

/// 要求 crossfade 的热切准入：不满足时返回失败码（调用方可改用 evaluate_hot_swap 走硬切/静音）。
pub fn admit_hot_swap(
    old: &SchedulePlan,
    new: &SchedulePlan,
    spec: &HotSwapSpec,
) -> Result<HotSwapAdmission, RtStatus> {
    let verdict = evaluate_hot_swap(old, new, spec);
    if verdict.admitted {
        Ok(verdict)
    } else if verdict.status.is_ok() {
        Err(RtStatus::ERR_UNSUPPORTED)
    } else {
        Err(verdict.status)
    }
}

// ---------------------------------------------------------------------------
// fallback 图与预算准入
// ---------------------------------------------------------------------------

/// fallback 预算（[NONRT]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackBudgets {
    /// deadline 预算（core 持续算力假设）。
    pub deadline: DeadlineBudgets,
    /// 预分配可用 RAM（字节；fallback 图必须能预分配）。
    pub available_ram_bytes: u64,
    /// fallback 图是否已在 admission 期预分配（core-v1 第 5.5 节：未预分配 -> 只允许安全 mute）。
    pub preallocated: bool,
    /// fallback 图 worker 数（4 = 生产同构；2 = 独立降级 profile）。
    pub workers: u8,
}

impl Default for FallbackBudgets {
    fn default() -> Self {
        Self {
            deadline: DeadlineBudgets::default(),
            available_ram_bytes: 0,
            preallocated: false,
            workers: RT_WORKERS,
        }
    }
}

/// fallback 准入失败原因（证据用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// fallback 图准入通过（可预分配、可时限完成）。
    Admitted,
    /// 未声明预分配 -> 只允许安全 mute。
    NotPreallocated,
    /// RAM 不足 -> 只允许安全 mute。
    RamOverBudget,
    /// deadline 预算不足 -> 只允许安全 mute。
    DeadlineOverBudget,
    /// fallback 图无法构建（支撑不落在规范档等）-> 只允许安全 mute。
    Unbuildable,
    /// 预算结构非法（core rate/gate 为 0）。
    InvalidBudgets,
}

impl FallbackReason {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::Admitted => "admitted",
            FallbackReason::NotPreallocated => "not_preallocated",
            FallbackReason::RamOverBudget => "ram_over_budget",
            FallbackReason::DeadlineOverBudget => "deadline_over_budget",
            FallbackReason::Unbuildable => "unbuildable",
            FallbackReason::InvalidBudgets => "invalid_budgets",
        }
    }
}

/// fallback 预算准入结果（[NONRT]；safety_mute_required=true 时只能用预分配静音安全输出）。
#[derive(Clone, Debug, PartialEq)]
pub struct FallbackAdmission {
    /// fallback 图（G=I 保 P_post 保护；独立 plan）；无法构建时为 None（禁止使用）。
    pub plan: Option<SchedulePlan>,
    /// fallback 图 deadline 预算结果。
    pub deadline: DeadlineAdmission,
    /// fallback 图 plan 侧字节（无法构建为 0）。
    pub plan_bytes: u64,
    /// RAM 是否在预算内。
    pub ram_ok: bool,
    /// 是否已预分配。
    pub preallocated: bool,
    /// fallback 图是否准入（可预分配 + 时限通过）。
    pub admitted: bool,
    /// 是否必须改用预分配安全静音（core-v1 第 5.5 节）。
    pub safety_mute_required: bool,
    /// 失败原因/结论。
    pub reason: FallbackReason,
}

fn empty_deadline_admission() -> DeadlineAdmission {
    DeadlineAdmission {
        gate_ns: 0,
        phase_worker_ns: [[0; SCHEDULE_WORKERS_MAX]; SCHEDULE_PHASES],
        phase_worst_ns: [0; SCHEDULE_PHASES],
        worker_block_ns: [0; SCHEDULE_WORKERS_MAX],
        total_worst_ns: u64::MAX,
        worst_worker: 0,
        admitted: false,
        overflow: true,
    }
}

/// 构建 fallback 图：G=I（仅 delta_D 基线），保留输入 A/输出 P 与最终保护；可指定 worker 数
/// （2 核降级是独立 plan/epoch，T09 WorkerReAdmission 口径）。
pub fn build_fallback_schedule(
    normal: &SchedulePlan,
    workers: u8,
) -> Result<SchedulePlan, RtStatus> {
    let mut profile = normal.profile;
    profile.workers = workers;
    profile.kernel = KernelForm::Direct;
    profile.fusion = FusionForm::Factorized;
    profile.crossfade = false;
    // fallback G=I：每输出 1 条 delta_D 路径；L_G 收缩为 D+1（只保留基线支撑）。
    profile.matrix_paths = profile.n_role as u32;
    profile.l_g = profile.d_samples + 1;
    // fallback 支撑 = max(L_A, D+1, L_P)；按第 7.2 节规范档重新分类，不继承 extended 声明。
    let support = profile.l_a.max(profile.l_g).max(profile.l_p);
    profile.max_taps = if support <= TAPS_NORMAL_MAX {
        TAPS_NORMAL_MAX
    } else if support == 8_192 || support == TAPS_EXTENDED_MAX {
        support
    } else {
        return Err(RtStatus::ERR_TAPS);
    };
    profile.extended_taps_admitted = profile.max_taps > TAPS_NORMAL_MAX;
    let mut plan = build(&profile, GraphClass::Fallback, false)?;
    plan.fusion_downgraded = false;
    Ok(plan)
}

/// 评估 fallback 预算准入（永不返回 Err；失败用 safety_mute_required 表达）。
pub fn evaluate_fallback(normal: &SchedulePlan, budgets: &FallbackBudgets) -> FallbackAdmission {
    let plan = match build_fallback_schedule(normal, budgets.workers) {
        Ok(value) => value,
        Err(_) => {
            return FallbackAdmission {
                plan: None,
                deadline: empty_deadline_admission(),
                plan_bytes: 0,
                ram_ok: false,
                preallocated: budgets.preallocated,
                admitted: false,
                safety_mute_required: true,
                reason: FallbackReason::Unbuildable,
            };
        }
    };
    let deadline = admit_deadline(&plan, &budgets.deadline);
    let plan_bytes = plan.plan_bytes;
    let ram_ok = budgets.available_ram_bytes != 0 && plan_bytes <= budgets.available_ram_bytes;
    let budget_valid = budgets.deadline.validate().is_ok();
    let admitted = budget_valid && budgets.preallocated && ram_ok && deadline.admitted;
    let reason = if !budget_valid {
        FallbackReason::InvalidBudgets
    } else if !budgets.preallocated {
        FallbackReason::NotPreallocated
    } else if !ram_ok {
        FallbackReason::RamOverBudget
    } else if !deadline.admitted {
        FallbackReason::DeadlineOverBudget
    } else {
        FallbackReason::Admitted
    };
    FallbackAdmission {
        plan: Some(plan),
        deadline,
        plan_bytes,
        ram_ok,
        preallocated: budgets.preallocated,
        admitted,
        safety_mute_required: !admitted,
        reason,
    }
}

// ---------------------------------------------------------------------------
// runtime 端到端预算准入（normal + fallback + 热切能力）
// ---------------------------------------------------------------------------

/// runtime 预算声明（[NONRT]；T56 端到端入口）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeBudgets {
    /// deadline 预算（core 持续算力假设）。
    pub deadline: DeadlineBudgets,
    /// 正常图 + fallback 图 plan 侧 RAM 预算（字节）。
    pub available_ram_bytes: u64,
    /// 单图 f32 系数/谱预算（字节；第 7.2 节每 route 4*taps）。
    pub available_coeff_bytes: u64,
    /// 双图 crossfade 峰值 CPU 可用量（ns/block；0 = 不声明 -> 热切拒绝）。
    pub available_crossfade_cpu_ns: u64,
    /// 双图 crossfade 峰值 RAM 可用量（字节；0 = 不声明 -> 热切拒绝）。
    pub available_crossfade_ram_bytes: u64,
    /// crossfade 帧数（必须 == B 且 <=128）。
    pub crossfade_frames: u32,
    /// 本次 admission 的 plan 版本（用于热切/证据；0 = 未声明）。
    pub plan_version: u32,
    /// fallback 图的 worker 数（4 = 生产同构；2 = 独立降级 profile）。
    pub fallback_workers: u8,
}

impl Default for RuntimeBudgets {
    fn default() -> Self {
        Self {
            deadline: DeadlineBudgets::default(),
            available_ram_bytes: 0,
            available_coeff_bytes: 0,
            available_crossfade_cpu_ns: 0,
            available_crossfade_ram_bytes: 0,
            crossfade_frames: 0,
            plan_version: 0,
            fallback_workers: RT_WORKERS,
        }
    }
}

impl RuntimeBudgets {
    /// 基线预算（证据/测试用）：64 MiB 图 RAM、4 MiB 系数、双图 CPU = 2*gate、双图 RAM 64 MiB、
    /// crossfade 帧数 = B、fallback 同 4 worker。是可复算的工程基线，不是板端实测。
    pub fn baseline(profile: &PlanProfile) -> Self {
        let block_ns = profile.block_ns();
        Self {
            deadline: DeadlineBudgets::default(),
            available_ram_bytes: 64 * 1024 * 1024,
            available_coeff_bytes: 4 * 1024 * 1024,
            // 双图 crossfade 峰值至少覆盖一个 block 窗口，留到 2 个 block（T20 下界口径）。
            available_crossfade_cpu_ns: block_ns.saturating_mul(2),
            available_crossfade_ram_bytes: 64 * 1024 * 1024,
            crossfade_frames: profile.block_size as u32,
            plan_version: 2,
            fallback_workers: RT_WORKERS,
        }
    }
}

/// runtime 端到端预算准入结果（[NONRT]；normal+fallback+热切能力一起裁决）。
#[derive(Clone, Debug)]
pub struct RuntimeAdmission {
    /// 正常生产图。
    pub normal: SchedulePlan,
    /// fallback 保护图准入（含 safety mute 结论）。
    pub fallback: FallbackAdmission,
    /// crossfade 能力评估（profile.crossfade=false 时 disposition=NotRequested）。
    pub hot_swap: HotSwapAdmission,
    /// 正常图 deadline 预算结果。
    pub deadline: DeadlineAdmission,
    /// 正常图系数是否在预算内。
    pub coeff_ok: bool,
    /// 图侧 RAM（normal + fallback plan_bytes）是否在预算内。
    pub ram_ok: bool,
    /// 端到端是否准入。
    pub admitted: bool,
    /// 结论状态码（OK / ERR_ADMISSION_SCHED / ERR_ADMISSION_MEMORY / 配置错误）。
    pub status: RtStatus,
}

/// runtime 端到端预算准入：build + deadline + 系数/RAM + 热切能力 + fallback 准入。
///
/// 返回 Err 仅表示 profile/budgets 配置非法（不可构造计划）；预算不足用 admitted=false +
/// status 表达（不抛错、不静默降级）。
pub fn admit_runtime(
    profile: &PlanProfile,
    budgets: &RuntimeBudgets,
) -> Result<RuntimeAdmission, RtStatus> {
    if budgets.deadline.validate().is_err() {
        return Err(RtStatus::ERR_ADMISSION_SCHED);
    }
    let mut normal = build_schedule(profile)?;
    let deadline = admit_deadline(&normal, &budgets.deadline);
    let coeff_ok =
        budgets.available_coeff_bytes != 0 && normal.coeff_bytes <= budgets.available_coeff_bytes;
    let fallback_budgets = FallbackBudgets {
        deadline: budgets.deadline,
        available_ram_bytes: budgets.available_ram_bytes,
        preallocated: budgets.available_ram_bytes != 0,
        workers: budgets.fallback_workers,
    };
    let fallback = evaluate_fallback(&normal, &fallback_budgets);
    let fallback_bytes = match &fallback.plan {
        Some(plan) => plan.plan_bytes,
        None => 0,
    };
    let ram_total = normal.plan_bytes.saturating_add(fallback_bytes);
    let ram_ok = budgets.available_ram_bytes != 0 && ram_total <= budgets.available_ram_bytes;
    let hot_swap = if profile.crossfade {
        let spec = HotSwapSpec {
            old_plan_version: budgets.plan_version.saturating_sub(1),
            new_plan_version: budgets.plan_version,
            crossfade_frames: budgets.crossfade_frames,
            available_cpu_ns: budgets.available_crossfade_cpu_ns,
            available_ram_bytes: budgets.available_crossfade_ram_bytes,
            core_macs_per_second: budgets.deadline.core_macs_per_second,
        };
        // 能力上界：以同一正常图的两份 arena 估计（真实 swap 由 T20 用新旧 plan 复算）。
        evaluate_hot_swap(&normal, &normal, &spec)
    } else {
        HotSwapAdmission::rejected(
            HotSwapDisposition::NotRequested,
            RtStatus::OK,
            &HotSwapSpec::default(),
        )
    };
    let mut status = RtStatus::OK;
    let mut admitted = true;
    if !deadline.admitted {
        admitted = false;
        status = RtStatus::ERR_ADMISSION_SCHED;
    } else if !coeff_ok || !ram_ok {
        admitted = false;
        status = RtStatus::ERR_ADMISSION_MEMORY;
    }
    normal.runtime_admitted = admitted;
    Ok(RuntimeAdmission {
        normal,
        fallback,
        hot_swap,
        deadline,
        coeff_ok,
        ram_ok,
        admitted,
        status,
    })
}

// ---------------------------------------------------------------------------
// 静态断言（只做常量/公式绑定，不引入 RT 语义）
// ---------------------------------------------------------------------------

const _: () = {
    // phase/stage/worker 口径与 core-v1/T09 绑定。
    assert!(SCHEDULE_PHASES == 3);
    assert!(SCHEDULE_WORKERS_MAX == 4);
    assert!(SCHEDULE_WORKERS_FALLBACK == 2);
    assert!(SCHEDULE_REDUCE_LANES == 4);
    assert!(PARALLEL_PHASES as usize == SCHEDULE_PHASES);
    assert!(SCHEDULE_STAGE_KINDS == StageKind::ALL.len());
    assert!(StageKind::Pre.phase_of() == 0);
    assert!(StageKind::MatrixG.phase_of() == 1);
    assert!(StageKind::Scatter.phase_of() == 2);
    assert!(StageKind::FftBinMac.index() == 4);
    // taps 规范档（T04 第 7.2 节 / T55）。
    assert!(TAPS_NORMAL_MAX == 4096);
    assert!(TAPS_EXTENDED_MAX == 16384);
    assert!(TAPS_EXTENDED_MAX > TAPS_NORMAL_MAX);
    // FFT/成本口径（CPU-BUDGET 第 1 节 / T55）。
    assert!(SCHEDULE_FLOPS_PER_COMPLEX_MAC == 8);
    assert!(SCHEDULE_FLOPS_PER_MAC == 2);
    assert!(SCHEDULE_MAC_EQ_PER_COMPLEX_MAC == 4);
    // 4.1.2 4096 direct 矩阵成本（CPU-BUDGET 第 2 节）：25*4096=102400 MAC/sample，
    // 2*102400*48000 = 9,830,400,000 FLOP/s；8192 翻倍。
    assert!(25u64 * 4096 == 102_400);
    assert!(2u64 * 102_400 * 48_000 == 9_830_400_000);
    assert!(2u64 * (25u64 * 8192) * 48_000 == 19_660_800_000);
    // 融合长度（T04 V14）：4096+1024-1=5119；8192+4096-1=12287。
    assert!(4096u32 + 1024 - 1 == 5119);
    assert!(8192u32 + 4096 - 1 == 12287);
    // 128@48k gate = 1,333,333 ns；64@48k = 666,666 ns（t_block/2 向下取整）。
    assert!(t_block_ns_ceil(48_000, 128) / 2 == 1_333_333);
    assert!(gate_ns_floor(48_000, 128) == 1_333_333);
    assert!(gate_ns_floor(48_000, 64) == 666_666);
    // 任务表容量 > 最坏 131。
    assert!(SCHEDULE_MAX_TASKS >= 256);
    // offline 上限大于 extended。
    assert!(SCHEDULE_OFFLINE_TAPS_MAX > TAPS_EXTENDED_MAX);
};
