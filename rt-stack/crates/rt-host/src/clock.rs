//! 时钟域映射 / tick 源 / ppm 估计 / 唯一 servo / discontinuity -> 新 epoch（core-v1 §6.4/§11.4）——WBS T19 写域。
//!
//! 本模块覆盖 runtime 侧时钟（不是设计期 T49 的 render/capture 时轴重建）：
//!
//! - 三个时间域分别保留（DSP-HOST-DECISION §5 时间域）：MOTU/ADC sample frame、
//!   AoIP/PTP timestamp、CLOCK_MONOTONIC ns。三者用仿射映射关联，禁止把 PTP 已锁
//!   等同于 USB 同钟，也禁止把某一域的 ppm 复制成另一域的 ppm。
//! - 有理数映射：t(frame) = start_mono_ns + frame * (1e9 - frame_ppm_ppb) / fs
//!   （core-v1 §6.4 要求有理数精确计算、禁止累积浮点误差）。frame_ppm_ppb > 0
//!   表示 MOTU frame 时钟快于 mono（每帧实际纳秒更少）；PTP 域同理
//!   ptp_ppm_ppb > 0 表示 PTP 钟快于 mono。所有正向换算用 u128/i128 一次乘除完成
//!   （floor），反向用最近邻圆整，避免"每块先转 ns 再累加"的漂移。
//! - ppm/uncertainty：PpmEstimator 对固定容量窗口做最小二乘，输出
//!   PpmEstimate{ppm_ppb, sigma_ppb, residual_rms_ns, significant, ...}；不确定度
//!   来自斜率标准误 + 残差，另加下限防止过度自信。所有估计只覆盖最近
//!   ESTIMATOR_CAPACITY 个观测点，不分配、不保存无限历史。
//! - 唯一 servo（DSP-HOST-DECISION §5、compat A15/K44、core-v1 §12.3/§6.4）：
//!   ClockServo 用 owner + generation grant 保证同一时刻只有一个闭环 owner
//!   （MotuFrame 或 Ptp）。第二个 claim 返回 ServoAlreadyOwned（403
//!   RT_E_INVARIANT），旧 generation 的 grant 失效。被 owner 的域才更新 ppm；
//!   另一域只测量、不被第二个环再调（ClockRuntime 只把 update 应用到 owner
//!   对应域）。core 的有理多速率不再开第二个 clock 闭环。
//! - 失锁 -> 新 epoch（core-v1 §5.2：PTP/clock discontinuity 必须新建 epoch）：
//!   ClockServo 在样本超时（PtpTimeout）、检测到时钟 step（PtpStep）、质量连续
//!   不达标（Quality）时报告 lock lost；ClockRuntime 立刻推进 EpochClock
//!   （epoch_id 进程内单调、永不复用）、清空 mapping 并要求 reanchor。旧 mapping
//!   快照仍可只读查询（带旧 epoch_id），但 runtime 不再返回；禁止跨 epoch 拼接历史。
//! - 预算内 tick：rt_tick() 读原始计数器（x86_64 _rdtsc、aarch64 cntvct_el0，
//!   其余平台退化为 mono ns），mono_now_ns() 读 CLOCK_MONOTONIC（Linux
//!   clock_gettime；Windows/macOS 开发用进程内 Instant 锚点，仅开发/CI）。
//!   calibrate_tick_source 在 admission（NONRT）标定 tick 频率，ticks_to_ns 用
//!   标定频率换算；TickBudget 强制每 block <= 3 次预算内 tick（core-v1 §3.4 I04；
//!   纯 worker 0 次）。probe_clock_support() 给 T18 admission 的 clock_source_ok
//!   提供真实探测结果。
//!
//! ## 错误映射（core-v1 §9.2 冻结码；不新增码）
//!
//! | ClockError | RtStatus | 语义/动作 |
//! |---|---|---|
//! | InvalidSampleRate | 104 ERR_RATE | 非 48k/96k |
//! | TimestampOverflow / FrameOverflow / TimestampBeforeEpoch | 103 ERR_BAD_LAYOUT | 映射输入越界/负数时间 |
//! | NotStarted / EpochAlreadyActive / AlreadyAnchored | 120 ERR_SWITCH_BUSY | 时钟状态机非法迁移 |
//! | EpochMismatch / StaleServoToken | 204 RT_E_EPOCH_MISMATCH | 跨 epoch / 旧 grant |
//! | ServoAlreadyOwned / ServoNotOwned / EpochIdExhausted | 403 RT_E_INVARIANT | 唯一 servo/epoch 不变量被破坏 |
//! | PpmOutOfRange / InvalidUncertainty / InsufficientSamples / TickCalibrationFailed | 116 ERR_ADMISSION_CLOCK | 时钟质量不足，不得进 RT 主模式 |
//! | NotLocked | 407 RT_E_DEVICE_LOST | PTP 未锁/失锁（需新 epoch） |
//! | TickBudgetExhausted | 405 RT_E_DEADLINE_HARD | 每 block tick 预算被破坏 |
//!
//! 本文件是混合模块（[RT] rt_tick / TickBudget；[NONRT]/[IO] mapping、servo、
//! 估计器、标定）。README §6 明确把 rt-host/src/clock.rs 列为混合模块：由代码
//! 评审 + drop-audit 类型清单覆盖，不做文件级 F-01..F-44 扫描。本模块的所有类型
//! 都是 POD/Copy 或固定数组，无 Drop、无堆分配、无锁、无日志；运行期热路径
//! （ClockMap 查询、TickBudget::read）不分配。
//!
//! 未测项：真实 MOTU/AoIP/PTP 硬件（T43/T74）、板端 clock_gettime 精度与
//! _rdtsc/cntvct 标定、USB/PTP 双域实流、2 核重准入下的新 epoch 均由板端窗口
//! 验收；本模块只保证离线模型/合成漂移/失锁语义。

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use khaen_core::abi::TspiEpochInfoV1;
use khaen_core::rt::{RtStatus, SAMPLE_RATES_HZ};
use khaen_core::workers::TspiRtControlV1;

// ---------------------------------------------------------------------------
// 常量（core-v1 §3.4/§6.4/§12.3；T19 运行期阈值在此冻结）
// ---------------------------------------------------------------------------

/// 1 秒的纳秒数。
pub const NS_PER_SEC: u64 = 1_000_000_000;
/// 1 ppm 的 ppb 数（本模块内部统一用 ppb 整数计算）。
pub const PPB_PER_PPM: i64 = 1_000;
/// core-v1 §3.4 I04：master 每 block 预算内 tick 读取上限（B1/B3 各 1，hard cutoff 可选 1）。
pub const SPEC_TICK_READS_PER_BLOCK_MAX: u32 = 3;
/// ppm 估计器保留的最近观测数（固定容量，无分配）。
pub const ESTIMATOR_CAPACITY: usize = 64;
/// 最小二乘所需最少点数（2 自由度 + 2）。
pub const MIN_FIT_SAMPLES: usize = 4;
/// ppm 合理性上限（1000 ppm；超出即视为坏估计/坏 mapping）。
pub const PPM_SANITY_LIMIT_PPB: i64 = 1_000_000;
/// 不确定度合理性上限（1000 ppm）。
pub const UNCERTAINTY_SANITY_LIMIT_PPB: u64 = 1_000_000;
/// 斜率标准误下限（ppb），避免"点少残差小"的过度自信。
pub const SIGMA_FLOOR_PPB: u64 = 50;
/// 显著性判据倍数（|ppm| > SIGMA_FACTOR * sigma）。
pub const SIGMA_FACTOR: f64 = 3.0;
/// 拟合残差 RMS 上限（1 ms；PTP/调度抖动超过即视为质量不足）。
pub const RESIDUAL_RMS_LIMIT_NS: u64 = 1_000_000;
/// tick 标定单轮最小窗口（1 ms）。
pub const MIN_CALIBRATION_WINDOW_NS: u64 = 1_000_000;
/// tick 标定轮数（取中位数）。
pub const CALIBRATION_ROUNDS: usize = 3;
/// tick 频率合理性下限（1 kHz）。
pub const MIN_TICK_FREQ_HZ: u64 = 1_000;
/// tick 频率合理性上限（10 GHz）。
pub const MAX_TICK_FREQ_HZ: u64 = 10_000_000_000;
/// tick 标定轮间 spread 上限（ppm；超过拒绝标定）。
pub const TICK_CALIBRATION_MAX_SPREAD_PPM: u32 = 50_000;
/// mono 探针自旋时长（100 us）。
pub const PROBE_SPIN_NS: u64 = 100_000;

// ---------------------------------------------------------------------------
// 错误码（core-v1 §9.2 冻结码；不新增码）
// ---------------------------------------------------------------------------

/// 时钟域相关错误（Copy，无 Drop；不跨 RT 边界传 String）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockError {
    /// 采样率不在 {48000, 96000}。
    InvalidSampleRate,
    /// 正向映射结果超出 u64。
    TimestampOverflow,
    /// frame 乘加溢出 u128。
    FrameOverflow,
    /// 查询时间早于 epoch 起点（frame 0 之前）。
    TimestampBeforeEpoch,
    /// 尚未开流/尚无 epoch。
    NotStarted,
    /// 已有活动 epoch（重复 start）。
    EpochAlreadyActive,
    /// 该 epoch 已有 mapping（重复 reanchor）。
    AlreadyAnchored,
    /// 跨 epoch 查询/旧 epoch mapping。
    EpochMismatch,
    /// 已被其它 owner 占用（第二个 clock 闭环）。
    ServoAlreadyOwned,
    /// 尚未被任何 owner 占用。
    ServoNotOwned,
    /// grant 的 generation 已过期（release/reclaim 之后）。
    StaleServoToken,
    /// ppm 超出合理性上限。
    PpmOutOfRange,
    /// 不确定度超出合理性上限。
    InvalidUncertainty,
    /// 估计点数不足。
    InsufficientSamples,
    /// 时钟未锁（PTP 未锁/失锁）。
    NotLocked,
    /// tick 频率标定失败/频率非法。
    TickCalibrationFailed,
    /// 每 block tick 预算耗尽。
    TickBudgetExhausted,
    /// epoch_id 耗尽（u64 回绕；实际不可达，fail closed）。
    EpochIdExhausted,
}

impl ClockError {
    /// 机器名（日志/证据用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            ClockError::InvalidSampleRate => "invalid_sample_rate",
            ClockError::TimestampOverflow => "timestamp_overflow",
            ClockError::FrameOverflow => "frame_overflow",
            ClockError::TimestampBeforeEpoch => "timestamp_before_epoch",
            ClockError::NotStarted => "not_started",
            ClockError::EpochAlreadyActive => "epoch_already_active",
            ClockError::AlreadyAnchored => "already_anchored",
            ClockError::EpochMismatch => "epoch_mismatch",
            ClockError::ServoAlreadyOwned => "servo_already_owned",
            ClockError::ServoNotOwned => "servo_not_owned",
            ClockError::StaleServoToken => "stale_servo_token",
            ClockError::PpmOutOfRange => "ppm_out_of_range",
            ClockError::InvalidUncertainty => "invalid_uncertainty",
            ClockError::InsufficientSamples => "insufficient_samples",
            ClockError::NotLocked => "not_locked",
            ClockError::TickCalibrationFailed => "tick_calibration_failed",
            ClockError::TickBudgetExhausted => "tick_budget_exhausted",
            ClockError::EpochIdExhausted => "epoch_id_exhausted",
        }
    }

    /// 映射到 core-v1 §9.2 冻结 RtStatus（不新增码）。
    pub const fn code(self) -> RtStatus {
        match self {
            ClockError::InvalidSampleRate => RtStatus::ERR_RATE,
            ClockError::TimestampOverflow
            | ClockError::FrameOverflow
            | ClockError::TimestampBeforeEpoch => RtStatus::ERR_BAD_LAYOUT,
            ClockError::NotStarted
            | ClockError::EpochAlreadyActive
            | ClockError::AlreadyAnchored => RtStatus::ERR_SWITCH_BUSY,
            ClockError::EpochMismatch | ClockError::StaleServoToken => {
                RtStatus::RT_E_EPOCH_MISMATCH
            }
            ClockError::ServoAlreadyOwned
            | ClockError::ServoNotOwned
            | ClockError::EpochIdExhausted => RtStatus::RT_E_INVARIANT,
            ClockError::PpmOutOfRange
            | ClockError::InvalidUncertainty
            | ClockError::InsufficientSamples
            | ClockError::TickCalibrationFailed => RtStatus::ERR_ADMISSION_CLOCK,
            ClockError::NotLocked => RtStatus::RT_E_DEVICE_LOST,
            ClockError::TickBudgetExhausted => RtStatus::RT_E_DEADLINE_HARD,
        }
    }
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// 时间域
// ---------------------------------------------------------------------------

/// 三个独立保留的时间域（DSP-HOST-DECISION §5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeDomain {
    /// CLOCK_MONOTONIC 纳秒。
    Mono,
    /// MOTU/ADC sample frame（设备采样帧计数）。
    MotuFrame,
    /// AoIP/PTP 时间戳纳秒。
    Ptp,
}

impl TimeDomain {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            TimeDomain::Mono => "mono",
            TimeDomain::MotuFrame => "motu_frame",
            TimeDomain::Ptp => "ptp",
        }
    }
}

// ---------------------------------------------------------------------------
// 平台 tick 源：mono_now_ns / rt_tick / ticks_to_ns / 标定
// ---------------------------------------------------------------------------

/// 本 target 的 rt_tick() 是否读硬件原始计数器（false = 退化到 mono ns）。
pub const fn uses_raw_tick_counter() -> bool {
    cfg!(any(target_arch = "x86_64", target_arch = "aarch64"))
}

/// 进程内 Instant 锚点（仅 Windows/macOS 开发与 Linux clock_gettime 失败回退）。
fn fallback_mono_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;

    static ANCHOR: OnceLock<Instant> = OnceLock::new();
    let anchor = ANCHOR.get_or_init(Instant::now);
    anchor.elapsed().as_nanos() as u64
}

/// Linux clock_gettime(CLOCK_MONOTONIC) 绑定（不新增 crate 依赖；std 已链接 libc）。
#[cfg(target_os = "linux")]
mod linux_clock {
    /// Linux struct timespec（LP64 布局：i64 tv_sec + i64 tv_nsec）。
    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    extern "C" {
        fn clock_gettime(clock_id: i32, tp: *mut Timespec) -> i32;
    }

    /// CLOCK_MONOTONIC（Linux UAPI：1）。
    const CLOCK_MONOTONIC: i32 = 1;
    const NS_PER_SEC_U64: u64 = 1_000_000_000;

    /// 读 CLOCK_MONOTONIC；失败返回 None（由调用方回退）。
    pub(super) fn clock_gettime_mono_ns() -> Option<u64> {
        let mut ts = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: tp 指向本栈帧内有效、可写的 Timespec；clock_id 为合法常量；
        // clock_gettime 不保留该指针。返回码检查后才读取字段。
        let rc = unsafe { clock_gettime(CLOCK_MONOTONIC, &mut ts) };
        if rc != 0 || ts.tv_sec < 0 || !(0..1_000_000_000).contains(&ts.tv_nsec) {
            return None;
        }
        Some(ts.tv_sec as u64 * NS_PER_SEC_U64 + ts.tv_nsec as u64)
    }
}

/// CLOCK_MONOTONIC 纳秒（[IO]/[NONRT]，进程内单调不回退）。
///
/// Linux 生产 target 直接 clock_gettime(CLOCK_MONOTONIC)（vDSO）；其它平台用
/// 进程内 Instant 锚点（仅开发/CI，绝对值与 CLOCK_MONOTONIC 不同，但单调）。
pub fn mono_now_ns() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Some(ns) = linux_clock::clock_gettime_mono_ns() {
            return ns;
        }
    }
    fallback_mono_ns()
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn raw_tick() -> u64 {
    // SAFETY: RDTSC 只读时间戳计数器，无内存副作用；x86_64 baseline 恒可用。
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn raw_tick() -> u64 {
    let value: u64;
    // SAFETY: CNTVCT_EL0 是用户态可读的虚拟计数器，无内存副作用。
    unsafe {
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nostack, preserves_flags));
    }
    value
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline(always)]
fn raw_tick() -> u64 {
    mono_now_ns()
}

/// [RT] 预算内 tick 读数（仅 master；每 block <= 3 次，纯 worker 0 次）。
///
/// 返回原始计数器值（compute_ticks = t_exit - t_entry，core-v1 §6.4）；换算为 ns
/// 用 ticks_to_ns 或 ctl.tick_freq_hz。本函数不做 syscall、不分配、不锁；它不自行
/// 计数，每 block 的预算由调用方（master）用 TickBudget 执行。
#[inline]
pub fn rt_tick() -> u64 {
    raw_tick()
}

/// core-v1 §12.3 冻结签名：rt_tick_budgeted(ctl) 的预算上限来自 ctl.tick_read_budget。
///
/// T06 scaffold 只冻结了无参 rt_tick()。按规范签名补此入口；具体"本 block 已读
/// 几次"的计数状态属于 master 热路径（不放进共享 ctl，避免给 ctl 增加未冻结字段），
/// 由 TickBudget 维护。预算非法（0）时按 SPEC_TICK_READS_PER_BLOCK_MAX=3 解释。
#[inline]
pub fn rt_tick_budgeted(ctl: &TspiRtControlV1) -> u64 {
    let _ = tick_read_budget(ctl);
    raw_tick()
}

/// 解析每 block tick 预算：ctl.tick_read_budget 合法时用它，0/超界时用规范上限 3。
pub const fn tick_read_budget(ctl: &TspiRtControlV1) -> u32 {
    let configured = ctl.tick_read_budget;
    if configured == 0 || configured > SPEC_TICK_READS_PER_BLOCK_MAX {
        SPEC_TICK_READS_PER_BLOCK_MAX
    } else {
        configured
    }
}

/// [RT] 每 block tick 读取预算计数器（master 私有；纯值，无 Drop）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickBudget {
    max_reads: u32,
    reads: u32,
}

impl TickBudget {
    /// 按显式上限构造（0 或 > 3 会被钳到规范上限 3）。
    pub const fn new(max_reads: u32) -> Self {
        let capped = if max_reads == 0 || max_reads > SPEC_TICK_READS_PER_BLOCK_MAX {
            SPEC_TICK_READS_PER_BLOCK_MAX
        } else {
            max_reads
        };
        Self {
            max_reads: capped,
            reads: 0,
        }
    }

    /// 按 ctl.tick_read_budget 构造。
    pub const fn from_control(ctl: &TspiRtControlV1) -> Self {
        Self::new(tick_read_budget(ctl))
    }

    /// 本 block 已读次数。
    pub const fn reads(&self) -> u32 {
        self.reads
    }

    /// 上限。
    pub const fn max_reads(&self) -> u32 {
        self.max_reads
    }

    /// 新 block 开始时清零。
    pub fn reset(&mut self) {
        self.reads = 0;
    }

    /// 读一次 tick；超出预算返回 TickBudgetExhausted（405，fail closed）。
    pub fn read(&mut self, ctl: &TspiRtControlV1) -> Result<u64, ClockError> {
        if self.reads >= self.max_reads {
            return Err(ClockError::TickBudgetExhausted);
        }
        let tick = rt_tick_budgeted(ctl);
        self.reads += 1;
        Ok(tick)
    }
}

/// tick 源标定结果（NONRT admission 证据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickCalibration {
    /// 标定出的 tick 频率（Hz，中位数）。
    pub freq_hz: u64,
    /// 轮间 spread 换算的 ppm 不确定度（相对中位数）。
    pub uncertainty_ppm: u32,
    /// 所有轮的总测量窗口（ns）。
    pub window_ns: u64,
    /// 轮数。
    pub rounds: u8,
}

static TICK_FREQ_HZ: AtomicU64 = AtomicU64::new(0);

/// 当前已安装的 tick 频率（未标定 = None）。
pub fn tick_frequency_hz() -> Option<u64> {
    let freq = TICK_FREQ_HZ.load(Ordering::Acquire);
    (freq != 0).then_some(freq)
}

/// [NONRT] admission 安装标定频率（T26 从 AdmissionReport.tick_freq_hz 回填）。
pub fn install_tick_frequency_hz(freq_hz: u64) -> Result<(), ClockError> {
    if !(MIN_TICK_FREQ_HZ..=MAX_TICK_FREQ_HZ).contains(&freq_hz) {
        return Err(ClockError::TickCalibrationFailed);
    }
    TICK_FREQ_HZ.store(freq_hz, Ordering::Release);
    Ok(())
}

/// 用显式频率把 tick 差换算为 ns（无全局状态，纯函数）。
pub fn ticks_to_ns_with_freq(ticks: u64, freq_hz: u64) -> Result<u64, ClockError> {
    if !(MIN_TICK_FREQ_HZ..=MAX_TICK_FREQ_HZ).contains(&freq_hz) {
        return Err(ClockError::TickCalibrationFailed);
    }
    let ns = (ticks as u128)
        .checked_mul(NS_PER_SEC as u128)
        .ok_or(ClockError::TimestampOverflow)?
        / freq_hz as u128;
    u64::try_from(ns).map_err(|_| ClockError::TimestampOverflow)
}

/// [NONRT] 把 tick 差换算为 ns（admission 标定的频率）。
///
/// 未标定时退回 T06 scaffold 的恒等语义（ticks 当 ns），只为兼容旧 scaffold；
/// T26 生产入口必须先 calibrate_tick_source/install_tick_frequency_hz，否则
/// admission 的 clock 项不得置 OK。
pub fn ticks_to_ns(ticks: u64) -> u64 {
    match tick_frequency_hz() {
        Some(freq) => ticks_to_ns_with_freq(ticks, freq).unwrap_or(u64::MAX),
        None => ticks,
    }
}

/// 自旋等待指定 ns（NONRT 标定/探测辅助）。
fn spin_for_ns(ns: u64) {
    let start = mono_now_ns();
    while mono_now_ns().saturating_sub(start) < ns {
        core::hint::spin_loop();
    }
}

/// [NONRT] admission 标定 tick 频率：CALIBRATION_ROUNDS 轮、每轮 >= min_window_ns。
///
/// 每轮同时读 mono_now_ns() 与 rt_tick()，用 tick 差 / 时间差求频率；取中位数并
/// 安装到全局。spread 过大会拒绝（不静默接受坏标定）。
pub fn calibrate_tick_source(min_window_ns: u64) -> Result<TickCalibration, ClockError> {
    let window = min_window_ns.max(MIN_CALIBRATION_WINDOW_NS);
    let mut freqs = [0u64; CALIBRATION_ROUNDS];
    let mut window_ns = 0u64;
    for slot in freqs.iter_mut() {
        let mono0 = mono_now_ns();
        let tick0 = raw_tick();
        spin_for_ns(window);
        let mono1 = mono_now_ns();
        let tick1 = raw_tick();
        let elapsed = mono1
            .checked_sub(mono0)
            .ok_or(ClockError::TickCalibrationFailed)?;
        let ticks = tick1
            .checked_sub(tick0)
            .ok_or(ClockError::TickCalibrationFailed)?;
        if elapsed == 0 || ticks == 0 {
            return Err(ClockError::TickCalibrationFailed);
        }
        let freq = (ticks as u128)
            .checked_mul(NS_PER_SEC as u128)
            .ok_or(ClockError::TickCalibrationFailed)?
            / elapsed as u128;
        let freq = u64::try_from(freq).map_err(|_| ClockError::TickCalibrationFailed)?;
        if !(MIN_TICK_FREQ_HZ..=MAX_TICK_FREQ_HZ).contains(&freq) {
            return Err(ClockError::TickCalibrationFailed);
        }
        *slot = freq;
        window_ns = window_ns.saturating_add(elapsed);
    }
    let mut sorted = freqs;
    sorted.sort_unstable();
    let median = sorted[CALIBRATION_ROUNDS / 2];
    let spread = sorted[CALIBRATION_ROUNDS - 1] - sorted[0];
    let spread_ppm = ((spread as u128) * 1_000_000 / median as u128).min(u32::MAX as u128) as u32;
    if spread_ppm > TICK_CALIBRATION_MAX_SPREAD_PPM {
        return Err(ClockError::TickCalibrationFailed);
    }
    install_tick_frequency_hz(median)?;
    Ok(TickCalibration {
        freq_hz: median,
        uncertainty_ppm: spread_ppm,
        window_ns,
        rounds: CALIBRATION_ROUNDS as u8,
    })
}

/// 时钟源探测结果（T18 AdmissionEnv.clock_source_ok 的真实输入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSupport {
    /// mono_now_ns 可读且单调。
    pub mono_clock_ok: bool,
    /// rt_tick 可读且单调（退化平台 = mono ns）。
    pub raw_tick_ok: bool,
    /// 已安装的 tick 频率（未标定 = None）。
    pub tick_freq_hz: Option<u64>,
    /// 生产前是否必须标定（本 target 用原始计数器且尚未标定）。
    pub needs_calibration: bool,
}

impl ClockSupport {
    /// T18 AdmissionEnv.clock_source_ok 的判据：mono/tick 可读且标定已完成。
    pub const fn rt_ready(&self) -> bool {
        self.mono_clock_ok && self.raw_tick_ok && !self.needs_calibration
    }
}

/// [NONRT] admission 探测：mono 可读性/单调性 + tick 可读性 + 标定状态。
pub fn probe_clock_support() -> ClockSupport {
    let mono0 = mono_now_ns();
    let tick0 = raw_tick();
    spin_for_ns(PROBE_SPIN_NS);
    let mono1 = mono_now_ns();
    let tick1 = raw_tick();
    let freq = tick_frequency_hz();
    ClockSupport {
        mono_clock_ok: mono1 >= mono0 && mono1 > 0,
        raw_tick_ok: tick1 >= tick0,
        tick_freq_hz: freq,
        needs_calibration: freq.is_none() && uses_raw_tick_counter(),
    }
}

// ---------------------------------------------------------------------------
// epoch：单调、永不复用（core-v1 §5.2/§11.4）
// ---------------------------------------------------------------------------

/// discontinuity 原因（决定新 epoch 的审计字段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiscontinuityReason {
    /// PTP/外部时钟样本超时，holdover 用尽。
    PtpTimeout,
    /// PTP/设备时钟发生 step（超过 step 门限）。
    PtpStep,
    /// 连续质量不达标（ppm/不确定度/residual）。
    Quality,
    /// 设备重开/拔插恢复（T17 状态机要求新 epoch）。
    DeviceReopen,
    /// control 显式请求（如不可恢复 xrun/队列溢出策略）。
    Explicit,
}

impl DiscontinuityReason {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            DiscontinuityReason::PtpTimeout => "ptp_timeout",
            DiscontinuityReason::PtpStep => "ptp_step",
            DiscontinuityReason::Quality => "quality",
            DiscontinuityReason::DeviceReopen => "device_reopen",
            DiscontinuityReason::Explicit => "explicit",
        }
    }
}

/// 一次 discontinuity -> 新 epoch 的审计记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Discontinuity {
    /// 旧 epoch。
    pub old_epoch_id: u64,
    /// 新 epoch（= old + 1；进程内单调、不复用）。
    pub new_epoch_id: u64,
    /// 旧 mapping 的 ppm（无 mapping = None）。
    pub old_ppm_ppb: Option<i64>,
    /// 新 epoch 重估前的 ppm（reanchor 后由 servo 更新）。
    pub new_ppm_ppb: Option<i64>,
    /// 原因。
    pub reason: DiscontinuityReason,
    /// 发生时刻（mono ns）。
    pub at_mono_ns: u64,
}

/// 一个 epoch 的身份（对应 core-v1 §11.4 的 start_mono_ns/flags）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochStamp {
    /// 进程内单调 epoch_id（从 1 起，永不复用）。
    pub epoch_id: u64,
    /// 是否 discontinuity 之后的首个 epoch（§11.4 flags bit0）。
    pub first_after_discontinuity: bool,
    /// frame 0 的 mono 时刻。
    pub start_mono_ns: u64,
}

/// epoch 计数器：epoch_id 进程内单调、永不复用（core-v1 §5.2/INV-L7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochClock {
    next_epoch_id: u64,
    current: Option<EpochStamp>,
    discontinuities: u64,
    epochs_started: u64,
}

impl Default for EpochClock {
    fn default() -> Self {
        Self::new()
    }
}

impl EpochClock {
    /// 空 epoch 计数器（首个 epoch_id = 1，0 保留给"未开始"）。
    pub const fn new() -> Self {
        Self {
            next_epoch_id: 1,
            current: None,
            discontinuities: 0,
            epochs_started: 0,
        }
    }

    /// 当前 epoch。
    pub const fn current(&self) -> Option<EpochStamp> {
        self.current
    }

    /// 已启动 epoch 数。
    pub const fn epochs_started(&self) -> u64 {
        self.epochs_started
    }

    /// discontinuity 次数。
    pub const fn discontinuities(&self) -> u64 {
        self.discontinuities
    }

    /// 开始首个 epoch（要求当前没有活动 epoch）。
    pub fn begin(&mut self, start_mono_ns: u64) -> Result<EpochStamp, ClockError> {
        if self.current.is_some() {
            return Err(ClockError::EpochAlreadyActive);
        }
        let stamp = EpochStamp {
            epoch_id: self.next_epoch_id,
            first_after_discontinuity: false,
            start_mono_ns,
        };
        self.next_epoch_id = self
            .next_epoch_id
            .checked_add(1)
            .ok_or(ClockError::EpochIdExhausted)?;
        self.current = Some(stamp);
        self.epochs_started = self.epochs_started.saturating_add(1);
        Ok(stamp)
    }

    /// discontinuity：旧 epoch -> 新 epoch（frame 0 = at_mono_ns）。
    pub fn on_discontinuity(
        &mut self,
        reason: DiscontinuityReason,
        at_mono_ns: u64,
        old_ppm_ppb: Option<i64>,
        new_ppm_ppb: Option<i64>,
    ) -> Result<Discontinuity, ClockError> {
        let old = self.current.ok_or(ClockError::NotStarted)?;
        let next_after_new = self
            .next_epoch_id
            .checked_add(1)
            .ok_or(ClockError::EpochIdExhausted)?;
        let stamp = EpochStamp {
            epoch_id: self.next_epoch_id,
            first_after_discontinuity: true,
            start_mono_ns: at_mono_ns,
        };
        self.next_epoch_id = next_after_new;
        self.current = Some(stamp);
        self.discontinuities = self.discontinuities.saturating_add(1);
        Ok(Discontinuity {
            old_epoch_id: old.epoch_id,
            new_epoch_id: stamp.epoch_id,
            old_ppm_ppb,
            new_ppm_ppb,
            reason,
            at_mono_ns,
        })
    }
}

// ---------------------------------------------------------------------------
// 三域映射（有理数、单次乘除）
// ---------------------------------------------------------------------------

const fn u128_to_u64(value: u128) -> Result<u64, ClockError> {
    if value > u64::MAX as u128 {
        Err(ClockError::TimestampOverflow)
    } else {
        Ok(value as u64)
    }
}

/// floor(a * b / c)；checked_mul 失败返回 FrameOverflow。
fn mul_div_floor(a: u128, b: u128, c: u128) -> Result<u128, ClockError> {
    if c == 0 {
        return Err(ClockError::FrameOverflow);
    }
    let product = a.checked_mul(b).ok_or(ClockError::FrameOverflow)?;
    Ok(product / c)
}

/// round(a * b / c)（最近邻；c/2 加到分子）。
fn mul_div_round(a: u128, b: u128, c: u128) -> Result<u128, ClockError> {
    if c == 0 {
        return Err(ClockError::FrameOverflow);
    }
    let product = a.checked_mul(b).ok_or(ClockError::FrameOverflow)?;
    Ok(product.saturating_add(c / 2) / c)
}

/// 一个 epoch 的 MOTU frame / PTP / mono 仿射映射（不可变快照）。
///
/// 映射以 frame 0 为锚点（core-v1 §11.4 start_mono_ns = frame 0 时刻）：
///
///   mono(frame) = mono_start + floor(frame * (1e9 - frame_ppm_ppb) / fs)
///   ptp(frame)  = ptp_start  + floor(frame * (1e9 - frame_ppm_ppb) * (1e9 + ptp_ppm_ppb) / (fs * 1e9))
///
/// frame_ppm_ppb > 0 = MOTU frame 钟快于 mono；ptp_ppm_ppb > 0 = PTP 钟快于 mono。
/// 反向查询（mono/ptp -> frame）四舍五入到最近帧；frame -> mono/ptp 为 floor，
/// 因此 frame_of_mono(mono_of_frame(f)) == f，且对任意 t 的往返误差 < 1 帧。
///
/// PTP 未锁（ptp_locked == false）时所有涉及 PTP 的查询返回 NotLocked——PTP 锁
/// 不等于 USB 同钟：ptp_locked 只表示 PTP 域自身可用，不代表 MOTU frame 域和
/// mono 域没有独立 ppm。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockMap {
    epoch_id: u64,
    sample_rate_hz: u32,
    mono_start_ns: u64,
    ptp_start_ns: u64,
    frame_ppm_ppb: i64,
    ptp_ppm_ppb: i64,
    frame_uncertainty_ppb: u64,
    ptp_uncertainty_ppb: u64,
    anchor_uncertainty_ns: u64,
    ptp_locked: bool,
}

impl ClockMap {
    /// 新建一个未调速 mapping（frame 0 = mono_start_ns，PTP 锚点可选）。
    pub fn new(
        epoch_id: u64,
        sample_rate_hz: u32,
        mono_start_ns: u64,
        ptp_start_ns: u64,
    ) -> Result<Self, ClockError> {
        if !SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            return Err(ClockError::InvalidSampleRate);
        }
        Ok(Self {
            epoch_id,
            sample_rate_hz,
            mono_start_ns,
            ptp_start_ns,
            frame_ppm_ppb: 0,
            ptp_ppm_ppb: 0,
            frame_uncertainty_ppb: 0,
            ptp_uncertainty_ppb: 0,
            anchor_uncertainty_ns: 0,
            ptp_locked: false,
        })
    }

    /// 设置两域 ppm（|ppm| > 1000 ppm 拒绝）。
    pub fn with_rates(mut self, frame_ppm_ppb: i64, ptp_ppm_ppb: i64) -> Result<Self, ClockError> {
        if frame_ppm_ppb.unsigned_abs() > PPM_SANITY_LIMIT_PPB as u64
            || ptp_ppm_ppb.unsigned_abs() > PPM_SANITY_LIMIT_PPB as u64
        {
            return Err(ClockError::PpmOutOfRange);
        }
        self.frame_ppm_ppb = frame_ppm_ppb;
        self.ptp_ppm_ppb = ptp_ppm_ppb;
        Ok(self)
    }

    /// 设置两域 1σ 不确定度（ppb）与锚点不确定度（ns）。
    pub fn with_uncertainties(
        mut self,
        frame_uncertainty_ppb: u64,
        ptp_uncertainty_ppb: u64,
        anchor_uncertainty_ns: u64,
    ) -> Result<Self, ClockError> {
        if frame_uncertainty_ppb > UNCERTAINTY_SANITY_LIMIT_PPB
            || ptp_uncertainty_ppb > UNCERTAINTY_SANITY_LIMIT_PPB
        {
            return Err(ClockError::InvalidUncertainty);
        }
        self.frame_uncertainty_ppb = frame_uncertainty_ppb;
        self.ptp_uncertainty_ppb = ptp_uncertainty_ppb;
        self.anchor_uncertainty_ns = anchor_uncertainty_ns;
        Ok(self)
    }

    /// 标记 PTP 域是否已锁。
    pub const fn with_ptp_locked(mut self, locked: bool) -> Self {
        self.ptp_locked = locked;
        self
    }

    /// epoch_id。
    pub const fn epoch_id(&self) -> u64 {
        self.epoch_id
    }

    /// 采样率。
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 校验 mapping 所属 epoch；跨 epoch 使用返回 EpochMismatch（204）。
    pub fn ensure_epoch(&self, expected_epoch_id: u64) -> Result<(), ClockError> {
        if self.epoch_id == expected_epoch_id {
            Ok(())
        } else {
            Err(ClockError::EpochMismatch)
        }
    }

    /// frame 0 的 mono 时刻。
    pub const fn mono_start_ns(&self) -> u64 {
        self.mono_start_ns
    }

    /// frame 0 的 PTP 时刻。
    pub const fn ptp_start_ns(&self) -> u64 {
        self.ptp_start_ns
    }

    /// MOTU frame 域 ppm（>0 = 快于 mono）。
    pub const fn frame_ppm_ppb(&self) -> i64 {
        self.frame_ppm_ppb
    }

    /// PTP 域 ppm（>0 = 快于 mono）。
    pub const fn ptp_ppm_ppb(&self) -> i64 {
        self.ptp_ppm_ppb
    }

    /// MOTU frame 域 1σ 不确定度（ppb）。
    pub const fn frame_uncertainty_ppb(&self) -> u64 {
        self.frame_uncertainty_ppb
    }

    /// PTP 域 1σ 不确定度（ppb）。
    pub const fn ptp_uncertainty_ppb(&self) -> u64 {
        self.ptp_uncertainty_ppb
    }

    /// 锚点不确定度（ns；两域共用同一 mono 观测）。
    pub const fn anchor_uncertainty_ns(&self) -> u64 {
        self.anchor_uncertainty_ns
    }

    /// PTP 是否已锁。
    pub const fn ptp_locked(&self) -> bool {
        self.ptp_locked
    }

    /// MOTU frame 相对 PTP 的 ppm（一阶近似 ptp - frame；<1000 ppm 时误差 <1e-6 相对）。
    pub const fn frame_ptp_relative_ppm_ppb(&self) -> i64 {
        self.ptp_ppm_ppb - self.frame_ppm_ppb
    }

    fn frame_ns_num(&self) -> u128 {
        // 已验证 |frame_ppm_ppb| <= 1e6，故 1e9 - ppm ∈ [999e6, 1001e6]，恒正。
        (NS_PER_SEC as i128 - self.frame_ppm_ppb as i128) as u128
    }

    fn ptp_ns_num(&self) -> u128 {
        (NS_PER_SEC as i128 + self.ptp_ppm_ppb as i128) as u128
    }

    fn fs(&self) -> u128 {
        self.sample_rate_hz as u128
    }

    /// MOTU frame -> mono ns。
    pub fn mono_of_frame(&self, frame: u64) -> Result<u64, ClockError> {
        let delta = mul_div_floor(frame as u128, self.frame_ns_num(), self.fs())?;
        self.mono_start_ns
            .checked_add(u128_to_u64(delta)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// mono ns -> MOTU frame（最近邻；早于 epoch 起点返回 TimestampBeforeEpoch）。
    pub fn frame_of_mono(&self, mono_ns: u64) -> Result<u64, ClockError> {
        let delta = mono_ns
            .checked_sub(self.mono_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let frame = mul_div_round(delta as u128, self.fs(), self.frame_ns_num())?;
        u128_to_u64(frame)
    }

    /// MOTU frame -> PTP ns。
    pub fn ptp_of_frame(&self, frame: u64) -> Result<u64, ClockError> {
        if !self.ptp_locked {
            return Err(ClockError::NotLocked);
        }
        let product = (frame as u128)
            .checked_mul(self.frame_ns_num())
            .ok_or(ClockError::FrameOverflow)?
            .checked_mul(self.ptp_ns_num())
            .ok_or(ClockError::FrameOverflow)?;
        let denom = self.fs() * NS_PER_SEC as u128;
        let delta = product / denom;
        self.ptp_start_ns
            .checked_add(u128_to_u64(delta)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// PTP ns -> MOTU frame（最近邻）。
    pub fn frame_of_ptp(&self, ptp_ns: u64) -> Result<u64, ClockError> {
        if !self.ptp_locked {
            return Err(ClockError::NotLocked);
        }
        let delta = ptp_ns
            .checked_sub(self.ptp_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let denom = self
            .frame_ns_num()
            .checked_mul(self.ptp_ns_num())
            .ok_or(ClockError::FrameOverflow)?;
        let frame = mul_div_round(delta as u128, self.fs() * NS_PER_SEC as u128, denom)?;
        u128_to_u64(frame)
    }

    /// mono ns -> PTP ns。
    pub fn ptp_of_mono(&self, mono_ns: u64) -> Result<u64, ClockError> {
        if !self.ptp_locked {
            return Err(ClockError::NotLocked);
        }
        let delta = mono_ns
            .checked_sub(self.mono_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let ptp_delta = mul_div_floor(delta as u128, self.ptp_ns_num(), NS_PER_SEC as u128)?;
        self.ptp_start_ns
            .checked_add(u128_to_u64(ptp_delta)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// PTP ns -> mono ns（最近邻）。
    pub fn mono_of_ptp(&self, ptp_ns: u64) -> Result<u64, ClockError> {
        if !self.ptp_locked {
            return Err(ClockError::NotLocked);
        }
        let delta = ptp_ns
            .checked_sub(self.ptp_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let mono_delta = mul_div_round(delta as u128, NS_PER_SEC as u128, self.ptp_ns_num())?;
        self.mono_start_ns
            .checked_add(u128_to_u64(mono_delta)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// 在 frame 处的 mono 域 1σ 不确定度（ns）：锚点 + span * frame_ppm_uncertainty。
    pub fn mono_uncertainty_ns(&self, frame: u64) -> Result<u64, ClockError> {
        let span = self
            .mono_of_frame(frame)?
            .checked_sub(self.mono_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let drift = mul_div_floor(
            span as u128,
            self.frame_uncertainty_ppb as u128,
            NS_PER_SEC as u128,
        )?;
        self.anchor_uncertainty_ns
            .checked_add(u128_to_u64(drift)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// 在 frame 处的 PTP 域 1σ 不确定度（ns）：锚点 + span * (frame_unc + ptp_unc)。
    pub fn ptp_uncertainty_ns(&self, frame: u64) -> Result<u64, ClockError> {
        let span = self
            .ptp_of_frame(frame)?
            .checked_sub(self.ptp_start_ns)
            .ok_or(ClockError::TimestampBeforeEpoch)?;
        let combined = self
            .frame_uncertainty_ppb
            .saturating_add(self.ptp_uncertainty_ppb);
        let drift = mul_div_floor(span as u128, combined as u128, NS_PER_SEC as u128)?;
        self.anchor_uncertainty_ns
            .checked_add(u128_to_u64(drift)?)
            .ok_or(ClockError::TimestampOverflow)
    }

    /// 生成 core-v1 §11.4 TspiEpochInfoV1（flags bit0 = first_after_discontinuity）。
    pub fn epoch_info(
        &self,
        plan_version: u32,
        block_size: u16,
        n_in: u16,
        n_out: u16,
        mode: u16,
        first_after_discontinuity: bool,
    ) -> TspiEpochInfoV1 {
        TspiEpochInfoV1 {
            epoch_id: self.epoch_id,
            plan_version,
            sample_rate: self.sample_rate_hz,
            block_size,
            n_in,
            n_out,
            mode,
            flags: u64::from(first_after_discontinuity),
            start_mono_ns: self.mono_start_ns,
            reserved: [0; 3],
        }
    }
}

// ---------------------------------------------------------------------------
// ppm 估计器（固定容量最小二乘；[NONRT]/[IO]）
// ---------------------------------------------------------------------------

/// 一次三域观测（同一时刻读到的 frame/mono/ptp；PTP 未锁时 ptp_valid=false）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockObservation {
    /// MOTU/ADC 采样帧。
    pub frame: u64,
    /// 同一时刻的 mono ns。
    pub mono_ns: u64,
    /// 同一时刻的 PTP ns（ptp_valid=false 时忽略）。
    pub ptp_ns: u64,
    /// PTP 读数是否有效。
    pub ptp_valid: bool,
}

impl ClockObservation {
    /// 只含 frame/mono 的观测（USB-only）。
    pub const fn motu(frame: u64, mono_ns: u64) -> Self {
        Self {
            frame,
            mono_ns,
            ptp_ns: 0,
            ptp_valid: false,
        }
    }

    /// 三域观测。
    pub const fn three_domain(frame: u64, mono_ns: u64, ptp_ns: u64) -> Self {
        Self {
            frame,
            mono_ns,
            ptp_ns,
            ptp_valid: true,
        }
    }
}

/// 一次 ppm 估计结果（ppb 整数 + 1σ + 残差 + 有效性/显著性）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PpmEstimate {
    /// MOTU frame 域 ppm（>0 = 快于 mono）。
    pub frame_ppm_ppb: i64,
    /// MOTU frame 域斜率标准误（ppb，含下限）。
    pub frame_sigma_ppb: u64,
    /// MOTU frame 拟合残差 RMS（ns）。
    pub frame_residual_rms_ns: u64,
    /// PTP 域 ppm（>0 = 快于 mono）；无有效 PTP 点时保持 0/无效。
    pub ptp_ppm_ppb: i64,
    /// PTP 域斜率标准误（ppb）。
    pub ptp_sigma_ppb: u64,
    /// PTP 拟合残差 RMS（ns）。
    pub ptp_residual_rms_ns: u64,
    /// 参与 MOTU 拟合的点数。
    pub samples: usize,
    /// 参与 PTP 拟合的点数。
    pub ptp_samples: usize,
    /// MOTU 估计是否在合理范围/有有限不确定度。
    pub frame_valid: bool,
    /// PTP 估计是否有效。
    pub ptp_valid: bool,
    /// |ppm| > 3σ（漂移显著，不是噪声）。
    pub significant: bool,
}

struct LinearFit {
    slope: f64,
    sigma: f64,
    residual_rms: f64,
}

/// 对 n 个点做 y = a + b x 最小二乘（x/y 用闭包随机访问，无分配）。
fn linear_fit(
    n: usize,
    mut x: impl FnMut(usize) -> f64,
    mut y: impl FnMut(usize) -> f64,
) -> Option<LinearFit> {
    if n < MIN_FIT_SAMPLES {
        return None;
    }
    let x0 = x(0);
    let y0 = y(0);
    let count = n as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    for i in 0..n {
        sum_x += x(i) - x0;
        sum_y += y(i) - y0;
    }
    let mean_x = sum_x / count;
    let mean_y = sum_y / count;
    let mut sxx = 0.0;
    let mut sxy = 0.0;
    for i in 0..n {
        let dx = (x(i) - x0) - mean_x;
        let dy = (y(i) - y0) - mean_y;
        sxx += dx * dx;
        sxy += dx * dy;
    }
    if !(sxx > 0.0) || !sxx.is_finite() || !sxy.is_finite() {
        return None;
    }
    let slope = sxy / sxx;
    if !slope.is_finite() || slope <= 0.0 {
        return None;
    }
    let mut sse = 0.0;
    for i in 0..n {
        let u = (x(i) - x0) - mean_x;
        let v = (y(i) - y0) - mean_y;
        let residual = v - slope * u;
        sse += residual * residual;
    }
    let residual_rms = (sse / count).sqrt();
    let dof = (count - 2.0).max(1.0);
    let sigma = (sse / dof).sqrt() / sxx.sqrt();
    Some(LinearFit {
        slope,
        sigma,
        residual_rms,
    })
}

fn f64_to_u64_saturating(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        0
    } else if !value.is_finite() || value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value.round() as u64
    }
}

fn f64_to_i64_saturating(value: f64) -> i64 {
    if value.is_nan() {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        value.round() as i64
    }
}

/// 固定容量滑动窗口 ppm 估计器（[NONRT]/[IO]；无分配、无锁）。
#[derive(Clone, Debug)]
pub struct PpmEstimator {
    frames: [u64; ESTIMATOR_CAPACITY],
    monos: [u64; ESTIMATOR_CAPACITY],
    ptps: [u64; ESTIMATOR_CAPACITY],
    ptp_valid: [bool; ESTIMATOR_CAPACITY],
    len: usize,
    dropped: u64,
}

impl Default for PpmEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl PpmEstimator {
    /// 空估计器。
    pub const fn new() -> Self {
        Self {
            frames: [0; ESTIMATOR_CAPACITY],
            monos: [0; ESTIMATOR_CAPACITY],
            ptps: [0; ESTIMATOR_CAPACITY],
            ptp_valid: [false; ESTIMATOR_CAPACITY],
            len: 0,
            dropped: 0,
        }
    }

    /// 当前窗口点数。
    pub const fn len(&self) -> usize {
        self.len
    }

    /// 窗口是否为空。
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 因窗口满被丢弃的最旧点数（诊断用）。
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 清空窗口。
    pub fn reset(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }

    /// 压入一个观测；窗口满时丢最旧点（O(N) 移动，N<=64，非 RT 热路径）。
    pub fn push(&mut self, observation: ClockObservation) {
        if self.len == ESTIMATOR_CAPACITY {
            for i in 1..ESTIMATOR_CAPACITY {
                self.frames[i - 1] = self.frames[i];
                self.monos[i - 1] = self.monos[i];
                self.ptps[i - 1] = self.ptps[i];
                self.ptp_valid[i - 1] = self.ptp_valid[i];
            }
            self.len -= 1;
            self.dropped = self.dropped.saturating_add(1);
        }
        let index = self.len;
        self.frames[index] = observation.frame;
        self.monos[index] = observation.mono_ns;
        self.ptps[index] = observation.ptp_ns;
        self.ptp_valid[index] = observation.ptp_valid && observation.ptp_ns != 0;
        self.len += 1;
    }

    /// 拟合当前窗口（MOTU frame 与 PTP 两个独立斜率）。
    pub fn fit(&self, sample_rate_hz: u32) -> Option<PpmEstimate> {
        if self.len < MIN_FIT_SAMPLES || sample_rate_hz == 0 {
            return None;
        }
        let n = self.len;
        let frame_fit = linear_fit(n, |i| self.frames[i] as f64, |i| self.monos[i] as f64)?;
        let nominal_ns_per_frame = NS_PER_SEC as f64 / sample_rate_hz as f64;
        // frame 快于 mono => 每帧实际 ns 更少 => slope < nominal => ratio > 1。
        let ratio = nominal_ns_per_frame / frame_fit.slope;
        let frame_ppm = (ratio - 1.0) * 1e9;
        let frame_sigma = if frame_fit.slope > 0.0 {
            nominal_ns_per_frame / (frame_fit.slope * frame_fit.slope) * frame_fit.sigma * 1e9
        } else {
            f64::INFINITY
        };
        let frame_out_of_range =
            !frame_ppm.is_finite() || frame_ppm.abs() > PPM_SANITY_LIMIT_PPB as f64;
        let frame_sigma_rounded = f64_to_u64_saturating(frame_sigma).max(SIGMA_FLOOR_PPB);
        let frame_valid = !frame_out_of_range
            && frame_sigma_rounded <= UNCERTAINTY_SANITY_LIMIT_PPB
            && f64_to_u64_saturating(frame_fit.residual_rms) <= RESIDUAL_RMS_LIMIT_NS;

        // PTP：只取 ptp_valid 的点；用下标映射。
        let mut indices = [0usize; ESTIMATOR_CAPACITY];
        let mut count = 0usize;
        for i in 0..n {
            if self.ptp_valid[i] {
                indices[count] = i;
                count += 1;
            }
        }
        let mut ptp_ppm = 0i64;
        let mut ptp_sigma = 0u64;
        let mut ptp_residual = 0u64;
        let mut ptp_valid_estimate = false;
        if count >= MIN_FIT_SAMPLES {
            if let Some(ptp_fit) = linear_fit(
                count,
                |j| self.monos[indices[j]] as f64,
                |j| self.ptps[indices[j]] as f64,
            ) {
                // ptp 快于 mono => d(ptp)/d(mono) > 1。
                let ppm = (ptp_fit.slope - 1.0) * 1e9;
                let sigma = ptp_fit.sigma * 1e9;
                ptp_ppm = f64_to_i64_saturating(ppm);
                ptp_sigma = f64_to_u64_saturating(sigma).max(SIGMA_FLOOR_PPB);
                ptp_residual = f64_to_u64_saturating(ptp_fit.residual_rms);
                ptp_valid_estimate = ppm.is_finite()
                    && ppm.abs() <= PPM_SANITY_LIMIT_PPB as f64
                    && ptp_sigma <= UNCERTAINTY_SANITY_LIMIT_PPB
                    && ptp_residual <= RESIDUAL_RMS_LIMIT_NS;
            }
        }
        let frame_ppm_ppb = f64_to_i64_saturating(frame_ppm);
        let significant = frame_valid
            && (frame_ppm_ppb.unsigned_abs() as f64) > SIGMA_FACTOR * frame_sigma_rounded as f64;
        Some(PpmEstimate {
            frame_ppm_ppb,
            frame_sigma_ppb: frame_sigma_rounded,
            frame_residual_rms_ns: f64_to_u64_saturating(frame_fit.residual_rms),
            ptp_ppm_ppb: ptp_ppm,
            ptp_sigma_ppb: ptp_sigma,
            ptp_residual_rms_ns: ptp_residual,
            samples: n,
            ptp_samples: count,
            frame_valid,
            ptp_valid: ptp_valid_estimate,
            significant,
        })
    }
}

// ---------------------------------------------------------------------------
// 唯一 clock servo（claim/grant + 锁定状态机）
// ---------------------------------------------------------------------------

/// clock servo 的闭环 owner（每个 mode 唯一）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServoOwner {
    /// MOTU frame 域相对 mono 的 async servo（USB 模式）。
    MotuFrame,
    /// PTP 域相对 mono 的 async servo（AoIP 模式；不得再开第二个闭环）。
    Ptp,
}

impl ServoOwner {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            ServoOwner::MotuFrame => "motu_frame",
            ServoOwner::Ptp => "ptp",
        }
    }
}

/// servo ownership grant（Copy；generation 过期即失效）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServoGrant {
    /// owner。
    pub owner: ServoOwner,
    /// claim 时递增的 generation。
    pub generation: u64,
}

/// servo 锁定状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockState {
    /// 未锁（无样本或失锁后）。
    Unlocked,
    /// 正在积累好样本。
    Locking,
    /// 已锁（ppm 可用）。
    Locked,
    /// 样本超时，处于 holdover（保持最后 ppm，不再更新）。
    Holdover,
}

impl LockState {
    /// 机器名。
    pub const fn as_str(self) -> &'static str {
        match self {
            LockState::Unlocked => "unlocked",
            LockState::Locking => "locking",
            LockState::Locked => "locked",
            LockState::Holdover => "holdover",
        }
    }
}

/// servo 阈值（T19 定义；core-v1 §5.2 只要求"判定为 discontinuity 即新 epoch"）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServoConfig {
    /// 锁定所需的连续好样本数。
    pub lock_samples: u32,
    /// 失锁所需的连续坏样本数。
    pub unlock_samples: u32,
    /// 锁定所需的最大 1σ 不确定度（ppb）。
    pub lock_uncertainty_ppb: u64,
    /// 锁定所需的 ppm 合理性上限（ppb）。
    pub lock_ppm_limit_ppb: i64,
    /// 样本超时（ns）：Locked -> Holdover。
    pub sample_timeout_ns: u64,
    /// holdover 上限（ns）：Holdover -> Unlocked（lock lost，PtpTimeout）。
    pub holdover_ns: u64,
    /// step 判定门限（ns）：owner 域的实际增量与预测增量之差。
    pub step_limit_ns: u64,
    /// 判定 step 所需的连续样本数。
    pub step_unlock_samples: u32,
}

impl Default for ServoConfig {
    fn default() -> Self {
        Self {
            lock_samples: 8,
            unlock_samples: 3,
            lock_uncertainty_ppb: 5_000,
            lock_ppm_limit_ppb: 100_000,
            sample_timeout_ns: 250_000_000,
            holdover_ns: 2_000_000_000,
            step_limit_ns: 1_000_000,
            step_unlock_samples: 1,
        }
    }
}

/// 一次 servo 观测/轮询的更新结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServoUpdate {
    /// owner。
    pub owner: ServoOwner,
    /// grant generation。
    pub generation: u64,
    /// 更新后的锁定状态。
    pub state: LockState,
    /// 当前 ppm（最近一次被接受估计；无 = 0）。
    pub ppm_ppb: i64,
    /// 该 ppm 的 1σ（ppb）。
    pub uncertainty_ppb: u64,
    /// 参与拟合的点数。
    pub samples: usize,
    /// 本次是否刚获得锁定。
    pub lock_acquired: bool,
    /// 本次是否失锁（Some 时 runtime 必须新 epoch）。
    pub lock_lost: Option<DiscontinuityReason>,
    /// 本样本是否被质量门接受。
    pub accepted: bool,
}

/// 唯一 clock servo：claim/grant + 锁定状态机 + 估计器。
///
/// 不变量：同一时刻最多一个 owner；被 owner 的域才产生 ppm 更新。第二个 claim
/// 返回 ServoAlreadyOwned，旧 grant 的 generation 不复用。
#[derive(Clone, Debug)]
pub struct ClockServo {
    config: ServoConfig,
    sample_rate_hz: u32,
    estimator: PpmEstimator,
    owner: Option<ServoOwner>,
    generation: u64,
    lock: LockState,
    good_samples: u32,
    bad_samples: u32,
    step_streak: u32,
    last_ppm_ppb: Option<i64>,
    last_uncertainty_ppb: u64,
    last_estimate: Option<PpmEstimate>,
    last_sample_mono_ns: Option<u64>,
    last_frame: Option<u64>,
    last_mono: Option<u64>,
    last_ptp: Option<u64>,
    samples_seen: u64,
    samples_accepted: u64,
    samples_rejected: u64,
    lock_losses: u64,
}

impl ClockServo {
    /// 新建 servo（未 claim）。
    pub fn new(config: ServoConfig, sample_rate_hz: u32) -> Self {
        Self {
            config,
            sample_rate_hz,
            estimator: PpmEstimator::new(),
            owner: None,
            generation: 0,
            lock: LockState::Unlocked,
            good_samples: 0,
            bad_samples: 0,
            step_streak: 0,
            last_ppm_ppb: None,
            last_uncertainty_ppb: 0,
            last_estimate: None,
            last_sample_mono_ns: None,
            last_frame: None,
            last_mono: None,
            last_ptp: None,
            samples_seen: 0,
            samples_accepted: 0,
            samples_rejected: 0,
            lock_losses: 0,
        }
    }

    /// 配置。
    pub const fn config(&self) -> &ServoConfig {
        &self.config
    }

    /// 当前 owner。
    pub const fn owner(&self) -> Option<ServoOwner> {
        self.owner
    }

    /// 当前锁定状态。
    pub const fn lock_state(&self) -> LockState {
        self.lock
    }

    /// 最近一次 ppm。
    pub const fn last_ppm_ppb(&self) -> Option<i64> {
        self.last_ppm_ppb
    }

    /// 最近一次估计（诊断用）。
    pub const fn last_estimate(&self) -> Option<PpmEstimate> {
        self.last_estimate
    }

    /// (observed, accepted, rejected, lock_losses) 计数。
    pub const fn counters(&self) -> (u64, u64, u64, u64) {
        (
            self.samples_seen,
            self.samples_accepted,
            self.samples_rejected,
            self.lock_losses,
        )
    }

    /// claim 唯一 owner；已被占用返回 ServoAlreadyOwned（403）。
    pub fn claim(&mut self, owner: ServoOwner) -> Result<ServoGrant, ClockError> {
        if self.owner.is_some() {
            return Err(ClockError::ServoAlreadyOwned);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ClockError::EpochIdExhausted)?;
        self.owner = Some(owner);
        self.reset_locking();
        Ok(ServoGrant {
            owner,
            generation: self.generation,
        })
    }

    /// 释放 owner（旧 grant 立即失效）。
    pub fn release(&mut self, grant: ServoGrant) -> Result<(), ClockError> {
        self.validate_grant(grant)?;
        self.owner = None;
        self.lock = LockState::Unlocked;
        self.estimator.reset();
        Ok(())
    }

    /// 失锁/新 epoch 后重置锁定历史（保留 owner/generation，grant 继续有效）。
    pub fn reset_locking(&mut self) {
        self.lock = LockState::Unlocked;
        self.good_samples = 0;
        self.bad_samples = 0;
        self.step_streak = 0;
        self.last_ppm_ppb = None;
        self.last_uncertainty_ppb = 0;
        self.last_estimate = None;
        self.last_sample_mono_ns = None;
        self.last_frame = None;
        self.last_mono = None;
        self.last_ptp = None;
        self.estimator.reset();
    }

    fn validate_grant(&self, grant: ServoGrant) -> Result<(), ClockError> {
        match self.owner {
            None => Err(ClockError::ServoNotOwned),
            Some(owner) if owner != grant.owner || self.generation != grant.generation => {
                Err(ClockError::StaleServoToken)
            }
            Some(_) => Ok(()),
        }
    }

    fn owner_measurement(&self, estimate: &PpmEstimate) -> (i64, u64, bool) {
        match self.owner {
            Some(ServoOwner::MotuFrame) => (
                estimate.frame_ppm_ppb,
                estimate.frame_sigma_ppb,
                estimate.frame_valid,
            ),
            Some(ServoOwner::Ptp) => (
                estimate.ptp_ppm_ppb,
                estimate.ptp_sigma_ppb,
                estimate.ptp_valid,
            ),
            None => (0, u64::MAX, false),
        }
    }

    fn step_detected(&mut self, observation: ClockObservation) -> bool {
        let mut jumped = false;
        if matches!(self.lock, LockState::Locked | LockState::Holdover) {
            match self.owner {
                Some(ServoOwner::MotuFrame) => {
                    if let (Some(prev_frame), Some(prev_mono)) = (self.last_frame, self.last_mono) {
                        if let Some(frames) = observation.frame.checked_sub(prev_frame) {
                            if frames > 0 {
                                let expected = (frames as u128) * NS_PER_SEC as u128
                                    / self.sample_rate_hz.max(1) as u128;
                                let actual = observation.mono_ns.saturating_sub(prev_mono) as u128;
                                if actual.abs_diff(expected) > self.config.step_limit_ns as u128 {
                                    jumped = true;
                                }
                            }
                        }
                    }
                }
                Some(ServoOwner::Ptp) => {
                    if let (Some(prev_ptp), Some(prev_mono)) = (self.last_ptp, self.last_mono) {
                        if observation.ptp_valid {
                            let expected = observation.mono_ns.saturating_sub(prev_mono) as i128;
                            let actual = observation.ptp_ns as i128 - prev_ptp as i128;
                            if (actual - expected).unsigned_abs()
                                > self.config.step_limit_ns as u128
                            {
                                jumped = true;
                            }
                        }
                    }
                }
                None => {}
            }
        }
        self.last_frame = Some(observation.frame);
        self.last_mono = Some(observation.mono_ns);
        if observation.ptp_valid {
            self.last_ptp = Some(observation.ptp_ns);
        }
        if jumped {
            self.step_streak = self.step_streak.saturating_add(1);
        } else {
            self.step_streak = 0;
        }
        jumped && self.step_streak >= self.config.step_unlock_samples.max(1)
    }

    fn lose_lock(&mut self, reason: DiscontinuityReason) -> DiscontinuityReason {
        self.lock = LockState::Unlocked;
        self.good_samples = 0;
        self.bad_samples = 0;
        self.step_streak = 0;
        self.estimator.reset();
        self.last_frame = None;
        self.last_mono = None;
        self.last_ptp = None;
        self.lock_losses = self.lock_losses.saturating_add(1);
        reason
    }

    fn make_update(
        &self,
        accepted: bool,
        lock_acquired: bool,
        lock_lost: Option<DiscontinuityReason>,
        estimate: Option<&PpmEstimate>,
    ) -> ServoUpdate {
        let (ppm, uncertainty) = match estimate {
            Some(fit) => {
                let (measured, sigma, _) = self.owner_measurement(fit);
                (measured, sigma)
            }
            None => (self.last_ppm_ppb.unwrap_or(0), self.last_uncertainty_ppb),
        };
        let (ppm, uncertainty) = match self.last_ppm_ppb {
            Some(current) => (current, self.last_uncertainty_ppb),
            None => (ppm, uncertainty),
        };
        ServoUpdate {
            owner: self.owner.unwrap_or(ServoOwner::MotuFrame),
            generation: self.generation,
            state: self.lock,
            ppm_ppb: ppm,
            uncertainty_ppb: uncertainty,
            samples: estimate.map(|fit| fit.samples).unwrap_or(0),
            lock_acquired,
            lock_lost,
            accepted,
        }
    }

    /// 观测一个三域样本并推进锁定状态机。
    pub fn observe(
        &mut self,
        grant: ServoGrant,
        observation: ClockObservation,
    ) -> Result<ServoUpdate, ClockError> {
        self.validate_grant(grant)?;
        self.samples_seen = self.samples_seen.saturating_add(1);
        self.last_sample_mono_ns = Some(observation.mono_ns);
        if let Some(reason) = self.step_loss(observation) {
            self.samples_rejected = self.samples_rejected.saturating_add(1);
            return Ok(self.make_update(false, false, Some(reason), None));
        }
        self.estimator.push(observation);
        let estimate = self.estimator.fit(self.sample_rate_hz);
        let Some(fit) = estimate else {
            self.samples_rejected = self.samples_rejected.saturating_add(1);
            return Ok(self.make_update(false, false, None, None));
        };
        let (ppm, sigma, valid) = self.owner_measurement(&fit);
        let quality_ok = valid
            && sigma <= self.config.lock_uncertainty_ppb
            && ppm.unsigned_abs() <= self.config.lock_ppm_limit_ppb as u64;
        let mut lock_acquired = false;
        let mut lock_lost = None;
        let mut accepted = false;
        match self.lock {
            LockState::Unlocked | LockState::Locking => {
                if quality_ok {
                    accepted = true;
                    self.samples_accepted = self.samples_accepted.saturating_add(1);
                    self.good_samples = self.good_samples.saturating_add(1);
                    self.bad_samples = 0;
                    self.last_ppm_ppb = Some(ppm);
                    self.last_uncertainty_ppb = sigma;
                    if self.good_samples >= self.config.lock_samples.max(1) {
                        self.lock = LockState::Locked;
                        lock_acquired = true;
                    } else {
                        self.lock = LockState::Locking;
                    }
                } else {
                    self.samples_rejected = self.samples_rejected.saturating_add(1);
                    self.bad_samples = self.bad_samples.saturating_add(1);
                    if self.bad_samples >= self.config.unlock_samples.max(1) {
                        self.good_samples = 0;
                        self.bad_samples = 0;
                        self.lock = LockState::Unlocked;
                    }
                }
            }
            LockState::Locked | LockState::Holdover => {
                if quality_ok {
                    accepted = true;
                    self.samples_accepted = self.samples_accepted.saturating_add(1);
                    self.bad_samples = 0;
                    self.last_ppm_ppb = Some(ppm);
                    self.last_uncertainty_ppb = sigma;
                    if self.lock == LockState::Holdover {
                        self.lock = LockState::Locked;
                        lock_acquired = true;
                    }
                } else {
                    self.samples_rejected = self.samples_rejected.saturating_add(1);
                    self.bad_samples = self.bad_samples.saturating_add(1);
                    if self.bad_samples >= self.config.unlock_samples.max(1) {
                        lock_lost = Some(self.lose_lock(DiscontinuityReason::Quality));
                    }
                }
            }
        }
        self.last_estimate = Some(fit);
        Ok(self.make_update(
            accepted,
            lock_acquired,
            lock_lost,
            self.last_estimate.as_ref(),
        ))
    }

    /// step 检测；达到连续门限时失锁（PtpStep）。
    fn step_loss(&mut self, observation: ClockObservation) -> Option<DiscontinuityReason> {
        if self.step_detected(observation) {
            Some(self.lose_lock(DiscontinuityReason::PtpStep))
        } else {
            None
        }
    }

    /// [NONRT] 轮询超时：Locked -> Holdover；Holdover 超时 -> 失锁（PtpTimeout）。
    pub fn poll(
        &mut self,
        grant: ServoGrant,
        now_mono_ns: u64,
    ) -> Result<Option<ServoUpdate>, ClockError> {
        self.validate_grant(grant)?;
        let Some(last) = self.last_sample_mono_ns else {
            return Ok(None);
        };
        let elapsed = now_mono_ns.saturating_sub(last);
        match self.lock {
            LockState::Locked if elapsed >= self.config.sample_timeout_ns => {
                self.lock = LockState::Holdover;
                Ok(Some(self.make_update(false, false, None, None)))
            }
            LockState::Holdover if elapsed >= self.config.holdover_ns => {
                let reason = self.lose_lock(DiscontinuityReason::PtpTimeout);
                Ok(Some(self.make_update(false, false, Some(reason), None)))
            }
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// runtime 集成：epoch + mapping + 唯一 servo
// ---------------------------------------------------------------------------

/// runtime 时钟计数（NONRT 导出）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockCounters {
    /// 启动 epoch 数。
    pub epochs_started: u64,
    /// discontinuity 次数（= 新 epoch 数）。
    pub discontinuities: u64,
    /// servo 锁定次数。
    pub lock_acquired: u64,
    /// servo 失锁次数。
    pub lock_lost: u64,
    /// servo 更新并写入 mapping 的次数。
    pub updates_applied: u64,
    /// 被质量门拒绝的样本数。
    pub samples_rejected: u64,
}

/// 一次 runtime observe/poll 的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockUpdate {
    /// servo 更新（含状态/ppm/失锁原因）。
    pub servo: ServoUpdate,
    /// 处理后的当前 epoch（未开流 = None）。
    pub epoch_id: Option<u64>,
    /// 本次触发的 discontinuity（Some 时必须 reanchor）。
    pub discontinuity: Option<Discontinuity>,
    /// 本次写入 mapping 的 ppm（只对 owner 域；未写入 = None）。
    pub applied_ppm_ppb: Option<i64>,
}

/// runtime 时钟：epoch 计数器 + 当前 mapping + 唯一 servo + 计数。
///
/// 调用顺序（T26）：start_stream（epoch 1，mapping）→ claim_servo →
/// observe/poll（锁定时只把 owner 域的 ppm 写回 mapping）→ 失锁时自动新 epoch
/// 并清空 mapping → reanchor_after_discontinuity 建新 epoch mapping
/// （TspiEpochInfoV1.flags bit0=1）。
#[derive(Clone, Debug)]
pub struct ClockRuntime {
    sample_rate_hz: u32,
    epoch: EpochClock,
    map: Option<ClockMap>,
    servo: ClockServo,
    counters: ClockCounters,
    last_discontinuity: Option<Discontinuity>,
}

impl ClockRuntime {
    /// 新建 runtime（不自动开流）。
    pub fn new(sample_rate_hz: u32, config: ServoConfig) -> Result<Self, ClockError> {
        if !SAMPLE_RATES_HZ.contains(&sample_rate_hz) {
            return Err(ClockError::InvalidSampleRate);
        }
        Ok(Self {
            sample_rate_hz,
            epoch: EpochClock::new(),
            map: None,
            servo: ClockServo::new(config, sample_rate_hz),
            counters: ClockCounters::default(),
            last_discontinuity: None,
        })
    }

    /// 采样率。
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// 当前 epoch。
    pub const fn epoch(&self) -> Option<EpochStamp> {
        self.epoch.current()
    }

    /// 当前 mapping（失锁后到 reanchor 前 = None）。
    pub fn map(&self) -> Option<&ClockMap> {
        self.map.as_ref()
    }

    /// 取当前 mapping；无 mapping = NotStarted。
    pub fn required_map(&self) -> Result<&ClockMap, ClockError> {
        self.map.as_ref().ok_or(ClockError::NotStarted)
    }

    /// 按 epoch 取 mapping：无 mapping = NotStarted；epoch 不符 = EpochMismatch（204）。
    ///
    /// 这是"禁止跨 epoch 拼接历史"的显式闸门（core-v1 §5.2/INV-L8）。
    pub fn map_for_epoch(&self, epoch_id: u64) -> Result<&ClockMap, ClockError> {
        let map = self.map.as_ref().ok_or(ClockError::NotStarted)?;
        map.ensure_epoch(epoch_id)?;
        Ok(map)
    }

    /// servo（只读，诊断用）。
    pub const fn servo(&self) -> &ClockServo {
        &self.servo
    }

    /// 计数。
    pub const fn counters(&self) -> ClockCounters {
        self.counters
    }

    /// 最近一次 discontinuity。
    pub const fn last_discontinuity(&self) -> Option<Discontinuity> {
        self.last_discontinuity
    }

    fn build_map(
        &mut self,
        now_mono_ns: u64,
        ptp_start_ns: u64,
        ptp_locked: bool,
    ) -> Result<ClockMap, ClockError> {
        let stamp = self.epoch.current().ok_or(ClockError::NotStarted)?;
        let map = ClockMap::new(
            stamp.epoch_id,
            self.sample_rate_hz,
            now_mono_ns,
            ptp_start_ns,
        )?
        .with_ptp_locked(ptp_locked);
        self.map = Some(map);
        Ok(map)
    }

    /// 启动首个 epoch 并建立 mapping（frame 0 = now_mono_ns）。
    pub fn start_stream(
        &mut self,
        now_mono_ns: u64,
        ptp_start_ns: u64,
        ptp_locked: bool,
    ) -> Result<ClockMap, ClockError> {
        self.epoch.begin(now_mono_ns)?;
        self.counters.epochs_started = self.counters.epochs_started.saturating_add(1);
        self.build_map(now_mono_ns, ptp_start_ns, ptp_locked)
    }

    /// discontinuity 后重建 mapping（要求当前 epoch 是 discontinuity 之后的首个 epoch）。
    pub fn reanchor_after_discontinuity(
        &mut self,
        now_mono_ns: u64,
        ptp_start_ns: u64,
        ptp_locked: bool,
    ) -> Result<ClockMap, ClockError> {
        if self.map.is_some() {
            return Err(ClockError::AlreadyAnchored);
        }
        let stamp = self.epoch.current().ok_or(ClockError::NotStarted)?;
        if !stamp.first_after_discontinuity {
            return Err(ClockError::AlreadyAnchored);
        }
        self.build_map(now_mono_ns, ptp_start_ns, ptp_locked)
    }

    /// claim 唯一 servo owner。
    pub fn claim_servo(&mut self, owner: ServoOwner) -> Result<ServoGrant, ClockError> {
        self.servo.claim(owner)
    }

    /// 释放 servo owner。
    pub fn release_servo(&mut self, grant: ServoGrant) -> Result<(), ClockError> {
        self.servo.release(grant)
    }

    fn apply_update_to_map(&mut self, update: &ServoUpdate) -> Result<Option<i64>, ClockError> {
        if !update.accepted || update.state != LockState::Locked || update.lock_lost.is_some() {
            return Ok(None);
        }
        let Some(map) = self.map.take() else {
            return Ok(None);
        };
        let (frame_ppm, frame_unc) = match update.owner {
            ServoOwner::MotuFrame => (update.ppm_ppb, update.uncertainty_ppb),
            ServoOwner::Ptp => (map.frame_ppm_ppb(), map.frame_uncertainty_ppb()),
        };
        let (ptp_ppm, ptp_unc) = match update.owner {
            ServoOwner::Ptp => (update.ppm_ppb, update.uncertainty_ppb),
            ServoOwner::MotuFrame => (map.ptp_ppm_ppb(), map.ptp_uncertainty_ppb()),
        };
        let new_map = map.with_rates(frame_ppm, ptp_ppm)?.with_uncertainties(
            frame_unc,
            ptp_unc,
            map.anchor_uncertainty_ns(),
        )?;
        let applied = update.ppm_ppb;
        self.map = Some(new_map);
        self.counters.updates_applied = self.counters.updates_applied.saturating_add(1);
        Ok(Some(applied))
    }

    fn apply_discontinuity(
        &mut self,
        reason: DiscontinuityReason,
        at_mono_ns: u64,
    ) -> Result<Discontinuity, ClockError> {
        let old_ppm = self.map.as_ref().map(|map| map.frame_ppm_ppb());
        let discontinuity = self
            .epoch
            .on_discontinuity(reason, at_mono_ns, old_ppm, None)?;
        self.map = None;
        self.servo.reset_locking();
        self.counters.discontinuities = self.counters.discontinuities.saturating_add(1);
        self.last_discontinuity = Some(discontinuity);
        Ok(discontinuity)
    }

    /// 观测一个样本：推进 servo；失锁则自动新 epoch；锁定时只写 owner 域 ppm。
    pub fn observe(
        &mut self,
        grant: ServoGrant,
        observation: ClockObservation,
    ) -> Result<ClockUpdate, ClockError> {
        let update = self.servo.observe(grant, observation)?;
        let mut discontinuity = None;
        let mut applied_ppm_ppb = None;
        if let Some(reason) = update.lock_lost {
            self.counters.lock_lost = self.counters.lock_lost.saturating_add(1);
            discontinuity = Some(self.apply_discontinuity(reason, observation.mono_ns)?);
        } else {
            if update.lock_acquired {
                self.counters.lock_acquired = self.counters.lock_acquired.saturating_add(1);
            }
            if !update.accepted {
                self.counters.samples_rejected = self.counters.samples_rejected.saturating_add(1);
            }
            applied_ppm_ppb = self.apply_update_to_map(&update)?;
        }
        Ok(ClockUpdate {
            servo: update,
            epoch_id: self.epoch.current().map(|stamp| stamp.epoch_id),
            discontinuity,
            applied_ppm_ppb,
        })
    }

    /// 轮询超时（NONRT 定时器）：Holdover/失锁，失锁自动新 epoch。
    pub fn poll(
        &mut self,
        grant: ServoGrant,
        now_mono_ns: u64,
    ) -> Result<Option<ClockUpdate>, ClockError> {
        let Some(update) = self.servo.poll(grant, now_mono_ns)? else {
            return Ok(None);
        };
        let mut discontinuity = None;
        if let Some(reason) = update.lock_lost {
            self.counters.lock_lost = self.counters.lock_lost.saturating_add(1);
            discontinuity = Some(self.apply_discontinuity(reason, now_mono_ns)?);
        }
        Ok(Some(ClockUpdate {
            servo: update,
            epoch_id: self.epoch.current().map(|stamp| stamp.epoch_id),
            discontinuity,
            applied_ppm_ppb: None,
        }))
    }

    /// control 显式请求 discontinuity（设备重开/不可恢复 xrun 等）：新 epoch、清 mapping。
    pub fn request_discontinuity(
        &mut self,
        reason: DiscontinuityReason,
        at_mono_ns: u64,
    ) -> Result<Discontinuity, ClockError> {
        if self.epoch.current().is_none() {
            return Err(ClockError::NotStarted);
        }
        self.apply_discontinuity(reason, at_mono_ns)
    }

    /// 生成 core-v1 §11.4 TspiEpochInfoV1（reanchor 后 flags bit0=1）。
    pub fn epoch_info(
        &self,
        plan_version: u32,
        block_size: u16,
        n_in: u16,
        n_out: u16,
        mode: u16,
    ) -> Option<TspiEpochInfoV1> {
        let stamp = self.epoch.current()?;
        let start_mono_ns = self
            .map
            .as_ref()
            .map(|map| map.mono_start_ns())
            .unwrap_or(stamp.start_mono_ns);
        Some(TspiEpochInfoV1 {
            epoch_id: stamp.epoch_id,
            plan_version,
            sample_rate: self.sample_rate_hz,
            block_size,
            n_in,
            n_out,
            mode,
            flags: u64::from(stamp.first_after_discontinuity),
            start_mono_ns,
            reserved: [0; 3],
        })
    }
}

// ---------------------------------------------------------------------------
// 编译期断言（§11.4 布局 / §13 无 Drop / 预算常量）
// ---------------------------------------------------------------------------

const _: () = {
    assert!(SPEC_TICK_READS_PER_BLOCK_MAX == 3);
    assert!(ESTIMATOR_CAPACITY == 64);
    // §12.3/§13：时钟类型全部 POD/Copy 或固定数组，无 Drop。
    assert!(!core::mem::needs_drop::<ClockMap>());
    assert!(!core::mem::needs_drop::<EpochClock>());
    assert!(!core::mem::needs_drop::<EpochStamp>());
    assert!(!core::mem::needs_drop::<Discontinuity>());
    assert!(!core::mem::needs_drop::<ServoGrant>());
    assert!(!core::mem::needs_drop::<ServoUpdate>());
    assert!(!core::mem::needs_drop::<PpmEstimate>());
    assert!(!core::mem::needs_drop::<PpmEstimator>());
    assert!(!core::mem::needs_drop::<ClockServo>());
    assert!(!core::mem::needs_drop::<ClockRuntime>());
    assert!(!core::mem::needs_drop::<ClockCounters>());
    assert!(!core::mem::needs_drop::<TickBudget>());
    // §11.4 epoch 元数据 64 B/align 64。
    assert!(core::mem::size_of::<TspiEpochInfoV1>() == 64);
    assert!(core::mem::align_of::<TspiEpochInfoV1>() == 64);
    assert!(core::mem::size_of::<TickBudget>() <= 16);
};

#[cfg(test)]
mod clock_tests {
    include!("../tests/T19/clock_models.rs");
}
