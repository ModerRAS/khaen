//! 设备掉线不退出（T42o）：PCM 输出设备的缺失监督与按退避自动重开。
//!
//! 真机背景：MOTU 被拔掉/重新枚举后，ALSA 写返回 `alsa pcm disconnected`，旧实现让
//! `run_block` 的错误直接冒到块循环并退出 1；设备不在位时启动也直接失败退出。用户要求：
//! **声卡掉线不应该让后端退出**。
//!
//! 本模块是 rt-host 的控制路径监督循环，数据面热路径（`Runtime::run_block`）一行不加：
//!
//! - 设备打不开/写失败 → 关闭 PCM 句柄、进入 `absent`、进程保持运行；
//! - `absent` 期间绝不调用 `run_block`（fail-closed：不输出、不写设备、不 panic），
//!   状态文件照常原子落盘（`device_state=absent`、`safe=true`）；
//! - 按固定退避（首次 2 s，之后翻倍，封顶 5 s）尝试 `Runtime::open_device` 重开；
//! - 恢复后重建输入抖动缓冲（`open_device` 内做）、显式清除"设备丢失"故障，
//!   当前方案版本/graph_id 保持不动，块计数继续增长 → running；
//! - 只有配置/方案准入类错误与无法恢复的内部错误才允许退出（由调用方返回稳定退出码）。
//!
//! # 与设备监督器（DeviceSupervisor）的分工
//!
//! DeviceSupervisor 判"设备是否存在/速率是否断言/所有权"，靠 sysfs/控制命令；本模块判
//! "PCM 音频流是否还能用"，靠打开设备与写 PCM 的实际结果。两者互不替代：声卡被拔掉时
//! 写 PCM 先失败，本模块负责先把进程稳在 fail-closed 状态，DeviceSupervisor 的
//! `poll_device` 仍按原节拍运行（状态里 `device.state` 照旧上报）。
//!
//! # 收包线程 / 抖动缓冲
//!
//! UDP 收包线程与声卡生命周期无关，不随设备重开重启（socket 不重建）；恢复时由
//! `Runtime::open_device` 抽干 SPSC 里积压的旧包并重建抖动环，避免把断线期间的旧音频
//! 灌进新缓冲。因此恢复后先出现 Warmup 静音块（预填重新开始），凑够水位才恢复出声。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use khaen_host::alsa_backend::PcmOutput;
use khaen_host::backend::BackendError;
use khaen_host::runtime::{Runtime, RuntimeError};

use crate::app::{
    check_device_rate, device_pcm_index, print_status, read_cards_text, read_hw_rate,
    resolve_card_index, write_status_file_ext,
};

/// 第一次重开前的退避（毫秒）。
pub const REOPEN_BASE_DELAY_MS: u64 = 2_000;
/// 重开退避上限（毫秒）。
pub const REOPEN_MAX_DELAY_MS: u64 = 5_000;
/// 设备缺失期间日志限频：每 N 次重开尝试打印一条（首次失败与状态变化总是打印）。
pub const REOPEN_LOG_EVERY: u64 = 10;
/// 设备缺失期间控制命令轮询片长（毫秒）：块循环在退避睡眠中按该片长醒来处理控制命令，
/// 所以设备 absent 时 mute/commit 仍会被受理（在恢复出块后的块边界生效）。
pub const CONTROL_POLL_SLICE_MS: u64 = 50;

/// 第 `attempt` 次重开失败后的退避（毫秒）：2 s、4 s，之后封顶 5 s。
pub fn reopen_delay_ms(attempt: u64) -> u64 {
    let shift = attempt.saturating_sub(1).min(8);
    REOPEN_BASE_DELAY_MS
        .saturating_mul(1u64 << shift)
        .min(REOPEN_MAX_DELAY_MS)
}

/// 输出设备缺失/重开的状态机（纯逻辑，无 IO；时间由调用方注入，便于测试）。
///
/// 状态字段与状态文件一一对应：`absent` → `device_state`，`absent_since_ms` →
/// `absent_since`，`reopen_attempts` → `reopen_attempts`，`last_device_error` →
/// `last_device_error`。`recoveries` 是累计恢复次数（日志/取证用，也进状态文件之外的诊断）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceLifecycle {
    absent: bool,
    absent_since_ms: Option<u64>,
    reopen_attempts: u64,
    last_device_error: Option<String>,
    next_attempt_at_ms: u64,
    recoveries: u64,
}

impl DeviceLifecycle {
    /// 新建：初始视为 present（首次打开由监督循环立即尝试，失败才 note_absent）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前是否处于设备缺失（absent）状态。
    pub fn is_absent(&self) -> bool {
        self.absent
    }

    /// 本次缺失起始时间（进程内单调毫秒；present 时为 None）。
    pub fn absent_since_ms(&self) -> Option<u64> {
        self.absent_since_ms
    }

    /// 本次缺失期间的重开尝试次数（恢复后清零）。
    pub fn reopen_attempts(&self) -> u64 {
        self.reopen_attempts
    }

    /// 最近一次设备错误文本（人读；状态文件里为 `last_device_error`）。
    pub fn last_device_error(&self) -> Option<&str> {
        self.last_device_error.as_deref()
    }

    /// 累计恢复次数（每次从 absent 成功重开 +1）。
    pub fn recoveries(&self) -> u64 {
        self.recoveries
    }

    /// 下一次允许重开尝试的时间（进程内单调毫秒）。
    pub fn next_attempt_at_ms(&self) -> u64 {
        self.next_attempt_at_ms
    }

    /// 设备不可用（启动打开失败或运行中掉线）：进入 absent，记录原因并允许立即重试一次。
    ///
    /// 已在 absent 时只更新错误文本与重试时间（不重置 absent_since/attempts）。
    pub fn note_absent(&mut self, now_ms: u64, error: impl Into<String>) {
        if !self.absent {
            self.absent = true;
            self.absent_since_ms = Some(now_ms);
            self.reopen_attempts = 0;
        }
        self.last_device_error = Some(error.into());
        // 掉线瞬间立即试一次：设备可能已被重新枚举；失败后按退避走。
        self.next_attempt_at_ms = now_ms;
    }

    /// 到点该尝试重开了吗。
    pub fn should_attempt(&self, now_ms: u64) -> bool {
        self.absent && now_ms >= self.next_attempt_at_ms
    }

    /// 登记一次重开尝试并给出该次尝试的序号（从 1 起）；按序号安排下一次退避。
    pub fn note_attempt(&mut self, now_ms: u64) -> u64 {
        self.reopen_attempts = self.reopen_attempts.saturating_add(1);
        self.next_attempt_at_ms = now_ms.saturating_add(reopen_delay_ms(self.reopen_attempts));
        self.reopen_attempts
    }

    /// 一次重开尝试失败：更新错误文本并重新安排退避。
    pub fn note_attempt_failed(&mut self, now_ms: u64, error: impl Into<String>) {
        self.last_device_error = Some(error.into());
        self.next_attempt_at_ms =
            now_ms.saturating_add(reopen_delay_ms(self.reopen_attempts.max(1)));
    }

    /// 重开成功：清除 absent/absent_since/attempts，累计恢复次数；返回此前是否真的 absent。
    pub fn note_recovered(&mut self, _now_ms: u64) -> bool {
        let was_absent = self.absent;
        if was_absent {
            self.recoveries = self.recoveries.saturating_add(1);
        }
        self.absent = false;
        self.absent_since_ms = None;
        self.reopen_attempts = 0;
        self.next_attempt_at_ms = 0;
        was_absent
    }

    /// 本次缺失期间的失败日志是否该打印：首次失败总打，之后每 `REOPEN_LOG_EVERY` 次打一条。
    pub fn should_log_attempt(&self) -> bool {
        self.reopen_attempts <= 1 || self.reopen_attempts.is_multiple_of(REOPEN_LOG_EVERY)
    }
}

/// 监督循环的时钟/睡眠注入点（真实运行用 [`RealPacer`]，测试用假时钟）。
pub trait LoopPacer {
    /// 进程内单调毫秒。
    fn now_ms(&mut self) -> u64;

    /// 睡至少 `ms` 毫秒（设备 missing 期间按退避排队时调用）。
    fn sleep_ms(&mut self, ms: u64);
}

/// 真实节拍器：`Instant` 单调时钟 + `thread::sleep`。
#[derive(Debug)]
pub struct RealPacer {
    origin: Instant,
}

impl RealPacer {
    /// 以当前时刻为原点。
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for RealPacer {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopPacer for RealPacer {
    fn now_ms(&mut self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn sleep_ms(&mut self, ms: u64) {
        if ms != 0 {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// 监督循环参数（来自环境配置；平台无关，测试可直接构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopOptions {
    /// ALSA PCM 设备名（`hw:` 写法才能回读速率；其它写法只记 unknown）。
    pub alsa_device: String,
    /// 读速率用的 `/proc/asound` 根（测试注入临时目录/不存在路径）。
    pub proc_asound: PathBuf,
    /// 状态文件路径（None = 不落盘）。
    pub status_file: Option<PathBuf>,
    /// 运行秒数（0 = 一直跑）；按块数换算，与旧实现一致。
    pub run_seconds: u64,
    /// 每 N 块打印统计并落状态文件（0 = 不打印）。
    pub stats_every: u64,
}

/// 块路径错误分类：true = 输出设备不可用（可恢复：关设备、进入 absent、退避重开）。
///
/// 只有 `BackendError::Fault`（设备掉线/写失败）与 `NotStarted`（句柄已释放）算设备不可用；
/// `BadConfig`/`WrongBlockLength` 是配置或程序错误（不可恢复），由调用方退出。
pub fn is_recoverable_device_loss(error: &RuntimeError) -> bool {
    matches!(
        error,
        RuntimeError::Backend(BackendError::Fault(_))
            | RuntimeError::Backend(BackendError::NotStarted)
    )
}

/// 打开输出设备并做速率回读校验；失败返回人读原因（设备缺失/速率不符都算可重试）。
///
/// 速率不符时**关掉设备再返回错误**：fail-closed 不带着错速播放，等设备回到方案速率再重开。
fn open_output<P: PcmOutput>(
    runtime: &mut Runtime<P>,
    options: &LoopOptions,
    plan_rate: u32,
) -> Result<(), String> {
    let cards = read_cards_text(&options.proc_asound);
    let card_index = resolve_card_index(&options.alsa_device, &cards);
    let pcm_index = device_pcm_index(&options.alsa_device);
    let rate_before =
        card_index.and_then(|card| read_hw_rate(&options.proc_asound, card, pcm_index));
    if let Err(error) = runtime.open_device() {
        return Err(format!("[{:?}] {error}", error.code()));
    }
    let rate_after =
        card_index.and_then(|card| read_hw_rate(&options.proc_asound, card, pcm_index));
    if let Err(code) = check_device_rate(plan_rate, rate_after) {
        let _ = runtime.close_device();
        return Err(format!(
            "{code}: device rate {} != plan {plan_rate} Hz (fail-closed, not playing)",
            rate_text(rate_after)
        ));
    }
    println!(
        "khaen-rt-host: output device open (rate before {} / after {}, plan {plan_rate} Hz)",
        rate_text(rate_before),
        rate_text(rate_after)
    );
    Ok(())
}

/// 速率的人读文本（None = 读不到）。
fn rate_text(rate: Option<u32>) -> String {
    rate.map_or_else(|| "unknown".to_string(), |rate| format!("{rate} Hz"))
}

/// 把当前状态快照原子写入状态文件（未配置路径时什么都不做）。
///
/// 增量字段（schema 仍是 khaen-rt-status/v1）：`format`（当前数据面块长/速率/通道）与
/// `reconfigure`（generation/last_gap_ms/last_result/在飞），来自块线程发布的热重配追踪器。
fn persist_status<P: PcmOutput>(
    runtime: &Runtime<P>,
    options: &LoopOptions,
    lifecycle: &DeviceLifecycle,
    bridge: &crate::control::ControlBridge,
) {
    let Some(path) = options.status_file.as_deref() else {
        return;
    };
    let status = runtime.status();
    let tracker = bridge.reconfigure();
    let tracker = tracker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Err(error) = write_status_file_ext(
        path,
        &status,
        lifecycle,
        Some(&runtime.config().engine),
        Some(&tracker),
    ) {
        eprintln!(
            "khaen-rt-host: cannot write status file {}: {error}",
            path.display()
        );
    }
}

/// 数据面主循环（平台无关；`P` 为 PCM 设备类型，真机是 AlsaPcm，测试用脚本化假设备）。
///
/// 无控制通道的入口：等价于 [drive_loop_with_control] + 一个 disabled 的控制桥
/// （既有 T42o 行为与测试不变）。
pub fn drive_loop<P: PcmOutput + 'static, C: LoopPacer>(
    runtime: &mut Runtime<P>,
    options: &LoopOptions,
    pacer: &mut C,
) -> (ExitCode, DeviceLifecycle) {
    let mut bridge = crate::control::ControlBridge::disabled();
    drive_loop_with_control(runtime, options, pacer, &mut bridge)
}

/// 数据面主循环 + 跨进程控制通道（T70f-a）。
///
/// 行为（顺序即优先级）：
/// 1. 每轮块边界前 poll 控制命令（mute/换图）并回执；命令后用一次性 ack 唤醒控制线程；
/// 2. 设备已打开 → `run_block`；成功则块计数 +1，按需轮询设备监督器/打印统计/落状态文件，
///    并按 `stats_every` 发布控制通道状态快照；
/// 3. `run_block` 报设备不可用 → 关句柄、把故障交给安全监督器（立刻 faulted + 安全静音）、
///    进入 absent；**不退出**；
/// 4. 设备未打开 → 绝不调用 `run_block`（fail-closed）；到点按退避调 `open_device` 重开，
///    成功后清设备丢失故障、记录恢复日志、继续块循环；
/// 5. 不可恢复错误（引擎/配置类后端错误等）→ 返回退出码 1（稳定），并停收包线程。
///
/// 控制通道的约束：`Runtime::run_block` 一行不改；命令只在块边界执行，快照在块边界发布，
/// 控制线程只读快照。设备 absent 期间用 [CONTROL_POLL_SLICE_MS] 小片睡眠轮询命令，
/// 所以 status/mute/commit 照常响应（图在恢复后的第一个块边界生效）。
///
/// 返回退出码与最终设备生命周期快照（测试/诊断用；状态文件也已落过盘）。
pub fn drive_loop_with_control<P: PcmOutput + 'static, C: LoopPacer>(
    runtime: &mut Runtime<P>,
    options: &LoopOptions,
    pacer: &mut C,
    bridge: &mut crate::control::ControlBridge,
) -> (ExitCode, DeviceLifecycle) {
    let (plan_rate, block_frames, sample_rate) = {
        let engine = &runtime.config().engine;
        (
            engine.sample_rate,
            engine.block_frames as u64,
            u64::from(engine.sample_rate),
        )
    };
    let target_blocks = if options.run_seconds == 0 {
        0
    } else {
        (options.run_seconds.saturating_mul(sample_rate)).div_ceil(block_frames)
    };
    let mut blocks: u64 = 0;
    let mut lifecycle = DeviceLifecycle::new();
    // 控制通道：先发布一份初始快照（设备尚未打开也如实报告），status 不必等第一个块。
    bridge.publish(runtime, &lifecycle);

    // 启动时先试一次打开：设备不在位/打不开不退出，进入 absent 按退避重试。
    if let Err(error) = open_output(runtime, options, plan_rate) {
        eprintln!(
            "khaen-rt-host: output device unavailable at startup: {error}; \
             staying up fail-closed (no output), retrying every {REOPEN_BASE_DELAY_MS}..{REOPEN_MAX_DELAY_MS} ms"
        );
        // 启动时没有块路径错误可上报：控制路径显式进 faulted（状态文件 state=faulted/safe=true）。
        runtime.report_device_loss(format!("output device unavailable at startup: {error}"));
        lifecycle.note_absent(pacer.now_ms(), error);
        persist_status(runtime, options, &lifecycle, bridge);
        bridge.publish(runtime, &lifecycle);
    }

    loop {
        // 块边界：先执行控制线程排队的命令（mute / 换图 / 热重配），再用 ack 唤醒等待中的控制线程。
        bridge.poll_commands(runtime, &mut lifecycle, pacer.now_ms());
        if target_blocks != 0 && blocks >= target_blocks {
            break;
        }
        if runtime.device_started() {
            // 校准播放（T70f-c）优先：音频线程只把预渲染扫频块写到设备，不跑节目图；
            // 扫频写完后继续写收尾静音，直到控制面显式 CalPlaybackEnd（引擎侧收完尾巴）。
            let calibration = runtime.calibration_playback_active();
            let result = if calibration {
                runtime.run_calibration_block().map(|_| ())
            } else {
                runtime.run_block().map(|_| ())
            };
            match result {
                Ok(()) => {
                    blocks = blocks.saturating_add(1);
                    if runtime.has_pending_fault() {
                        runtime.poll_device();
                        if runtime.clear_recovered_underrun_fault() {
                            println!(
                                "khaen-rt-host: cleared recovered input-underrun fault at block {blocks}; resuming"
                            );
                        }
                    }
                    if options.stats_every != 0 && blocks.is_multiple_of(options.stats_every) {
                        // 验证工具的让步点：状态打印与 supervise 轮询都在块线程上（见模块文档）。
                        runtime.poll_device();
                        if runtime.clear_recovered_underrun_fault() {
                            println!(
                                "khaen-rt-host: cleared recovered input-underrun fault at block {blocks}; resuming"
                            );
                        }
                        let status = runtime.status();
                        print_status(&status, &lifecycle);
                        if status.state.requires_mute() {
                            eprintln!(
                                "khaen-rt-host: safety requires mute; continuing (fail-closed blocks are silent)"
                            );
                        }
                        persist_status(runtime, options, &lifecycle, bridge);
                    }
                    bridge.maybe_publish(runtime, &lifecycle);
                }
                Err(error) => {
                    if !is_recoverable_device_loss(&error) {
                        eprintln!(
                            "khaen-rt-host: run_block failed at block {blocks}: [{:?}] {error}; exiting (unrecoverable)",
                            error.code()
                        );
                        bridge.publish(runtime, &lifecycle);
                        let _ = runtime.stop_receiver();
                        return (ExitCode::from(1), lifecycle);
                    }
                    // 设备掉线：关句柄、fail-closed、进入 absent；进程不退出。
                    let _ = runtime.close_device();
                    // 把块路径故障交给安全监督器：状态立刻 faulted（safe=true），输出已停。
                    runtime.poll_device();
                    eprintln!(
                        "khaen-rt-host: output device lost at block {blocks}: [{:?}] {error}; entering fail-closed absent state (process stays up, retrying)",
                        error.code()
                    );
                    lifecycle.note_absent(pacer.now_ms(), error.to_string());
                    persist_status(runtime, options, &lifecycle, bridge);
                    bridge.publish(runtime, &lifecycle);
                }
            }
        } else {
            // 设备不在位：按退避尝试重开。这里绝不调用 run_block、不写设备（fail-closed）。
            let now = pacer.now_ms();
            if !lifecycle.should_attempt(now) {
                let wait = lifecycle.next_attempt_at_ms().saturating_sub(now).max(1);
                // 设备缺失期间也照常响应控制命令：按小片睡，片间 poll 一次（
                // absent 时 mute/commit 仍会被受理，图在恢复后的第一个块边界生效）。
                pacer.sleep_ms(wait.min(CONTROL_POLL_SLICE_MS));
                bridge.poll_commands(runtime, &mut lifecycle, pacer.now_ms());
                continue;
            }
            let attempt = lifecycle.note_attempt(now);
            // 热重配可能改了采样率/通道：退避重开必须按当前生效配置的速率校验
            // （用启动时快照的 plan_rate 会把已生效的新速率误判成 device_rate_mismatch）。
            let plan_rate = runtime.config().effective_plan_rate();
            match open_output(runtime, options, plan_rate) {
                Ok(()) => {
                    let absent_ms = now.saturating_sub(lifecycle.absent_since_ms().unwrap_or(now));
                    lifecycle.note_recovered(now);
                    // 设备丢失故障显式清除；欠载/处理器等其它锁存故障保持（fail-closed 语义）。
                    runtime.clear_device_loss_fault();
                    let graph = runtime.graph_info();
                    let total_recoveries = lifecycle.recoveries();
                    println!(
                        "khaen-rt-host: device recovered at block {blocks} after {attempt} reopen attempt(s) ({absent_ms} ms absent, {total_recoveries} lifetime recoveries); jitter buffer rebuilt + refilling, plan version {:?} graph {:?} unchanged; resuming",
                        graph.version, graph.graph_id
                    );
                    persist_status(runtime, options, &lifecycle, bridge);
                    bridge.publish(runtime, &lifecycle);
                }
                Err(error) => {
                    let now = pacer.now_ms();
                    lifecycle.note_attempt_failed(now, error.clone());
                    if lifecycle.should_log_attempt() {
                        let absent_ms =
                            now.saturating_sub(lifecycle.absent_since_ms().unwrap_or(now));
                        eprintln!(
                            "khaen-rt-host: device reopen attempt {attempt} failed: {error}; still absent {absent_ms} ms, next retry in {} ms",
                            lifecycle.next_attempt_at_ms().saturating_sub(now)
                        );
                    }
                    persist_status(runtime, options, &lifecycle, bridge);
                    bridge.publish(runtime, &lifecycle);
                }
            }
        }
    }

    // 结束：打印最终状态、落盘、停收包线程。
    bridge.publish(runtime, &lifecycle);
    let status = runtime.status();
    print_status(&status, &lifecycle);
    persist_status(runtime, options, &lifecycle, bridge);
    if let Err(error) = runtime.stop_receiver() {
        eprintln!(
            "khaen-rt-host: receiver stop failed: [{:?}] {error}",
            error.code()
        );
        return (ExitCode::from(1), lifecycle);
    }
    println!(
        "khaen-rt-host: finished {blocks} blocks ({:.3} s of audio), exiting",
        (blocks.saturating_mul(block_frames)) as f64 / sample_rate as f64
    );
    (ExitCode::SUCCESS, lifecycle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use khaen_host::aes67_input::Aes67InputConfig;
    use khaen_host::alsa_backend::{PcmError, PcmFault, PcmOutput, BYTES_PER_SAMPLE};
    use khaen_host::backend::{BackendError, DeviceConfig};
    use khaen_host::engine::EngineError;
    use khaen_host::runtime::{
        PacketSource, ReceiverConfig, Runtime, RuntimeConfig, SupervisorOptions, UnloadedPolicy,
    };
    use khaen_host::{DeviceId, EngineConfig, SafetyPolicy};

    const SAMPLE_RATE: u32 = 48_000;
    const BLOCK_FRAMES: usize = 128;
    const PAYLOAD_TYPE: u8 = 99;
    const SSRC: u32 = 0x0bad_cafe;

    // -------------------------------------------------------------------
    // 脚本化假 PCM：open 失败次数、断线时机、写计数全由共享状态控制
    // -------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct ScriptState {
        /// 还需要失败的 open 次数（启动时设备不在位）。
        fail_opens: Arc<AtomicU32>,
        /// 第 N 次（从 1 起）write 返回"设备掉线"的脚本。
        fail_write_at: Arc<Mutex<VecDeque<u32>>>,
        opens: Arc<AtomicU32>,
        writes: Arc<AtomicU32>,
        write_ok: Arc<AtomicU32>,
        closed: Arc<AtomicU32>,
        connected: Arc<AtomicBool>,
    }

    static REGISTRY: OnceLock<Mutex<Vec<(String, ScriptState)>>> = OnceLock::new();

    fn registry() -> &'static Mutex<Vec<(String, ScriptState)>> {
        REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn register_scripted_pcm(device: &str, state: &ScriptState) {
        registry()
            .lock()
            .expect("script registry")
            .push((device.to_string(), state.clone()));
    }

    fn scripted_state(device: &str) -> ScriptState {
        registry()
            .lock()
            .expect("script registry")
            .iter()
            .find(|(name, _)| name == device)
            .map(|(_, state)| state.clone())
            .expect("scripted pcm registered")
    }

    struct ScriptedPcm {
        state: ScriptState,
        config: Option<DeviceConfig>,
    }

    impl PcmOutput for ScriptedPcm {
        fn open(config: &DeviceConfig) -> Result<Self, BackendError> {
            let state = scripted_state(config.device.as_str());
            state.opens.fetch_add(1, Ordering::SeqCst);
            if state.fail_opens.load(Ordering::SeqCst) > 0 {
                state.fail_opens.fetch_sub(1, Ordering::SeqCst);
                return Err(BackendError::Fault(
                    "alsa pcm open failed: device absent".to_string(),
                ));
            }
            state.connected.store(true, Ordering::SeqCst);
            Ok(Self {
                state,
                config: Some(config.clone()),
            })
        }

        fn write(&mut self, bytes: &[u8]) -> Result<usize, PcmError> {
            let config = self
                .config
                .as_ref()
                .ok_or(PcmError::Fatal("fake pcm not open"))?;
            assert_eq!(
                bytes.len(),
                config.samples_per_block() * BYTES_PER_SAMPLE,
                "scripted pcm block bytes"
            );
            // fail-closed 验证点：设备 absent 期间任何一次写都会在这里 panic。
            assert!(
                self.state.connected.load(Ordering::SeqCst),
                "write while device absent (fail-closed violated)"
            );
            let count = self.state.writes.fetch_add(1, Ordering::SeqCst) + 1;
            let should_fail = {
                let mut queue = self.state.fail_write_at.lock().expect("script queue");
                if queue.front() == Some(&count) {
                    queue.pop_front();
                    true
                } else {
                    false
                }
            };
            if should_fail {
                self.state.connected.store(false, Ordering::SeqCst);
                return Err(PcmError::Fatal("alsa pcm disconnected"));
            }
            self.state.write_ok.fetch_add(1, Ordering::SeqCst);
            Ok(config.block_frames)
        }

        fn prepare(&mut self, _fault: PcmFault) -> Result<(), BackendError> {
            Ok(())
        }

        fn available(&self) -> Result<usize, PcmError> {
            Ok(0)
        }

        fn close(&mut self) {
            self.state.closed.fetch_add(1, Ordering::SeqCst);
            self.state.connected.store(false, Ordering::SeqCst);
            self.config = None;
        }
    }

    // -------------------------------------------------------------------
    // 假包源
    // -------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct TestSourceHandle {
        packets: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl TestSourceHandle {
        fn send(&self, packet: Vec<u8>) {
            self.packets
                .lock()
                .expect("test source queue")
                .push_back(packet);
        }
    }

    struct TestSource {
        handle: TestSourceHandle,
    }

    impl PacketSource for TestSource {
        fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let packet = {
                let mut queue = self.handle.packets.lock().expect("test source queue");
                queue.pop_front()
            };
            match packet {
                Some(packet) => {
                    let len = packet.len().min(buf.len());
                    buf[..len].copy_from_slice(&packet[..len]);
                    Ok(len)
                }
                None => {
                    // 空闲时短暂睡眠后返回 WouldBlock：收包线程能在有限时间内看到停止标志。
                    std::thread::sleep(Duration::from_micros(200));
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                }
            }
        }
    }

    fn test_source() -> (TestSource, TestSourceHandle) {
        let handle = TestSourceHandle::default();
        (
            TestSource {
                handle: handle.clone(),
            },
            handle,
        )
    }

    /// 组一个 128 帧单声道 L24 RTP 包（正好一块）。
    fn packet(sequence: u16, timestamp: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + BLOCK_FRAMES * 3);
        out.push(0x80);
        out.push(PAYLOAD_TYPE);
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(&timestamp.to_be_bytes());
        out.extend_from_slice(&SSRC.to_be_bytes());
        for sample in 0..BLOCK_FRAMES {
            let raw = (sample as u32 * 1000) & 0x00ff_ffff;
            out.push((raw >> 16) as u8);
            out.push((raw >> 8) as u8);
            out.push(raw as u8);
        }
        out
    }

    fn runtime_config(device: &str) -> RuntimeConfig {
        RuntimeConfig {
            input: Aes67InputConfig {
                multicast_addr: Ipv4Addr::new(239, 69, 0, 1),
                port: 5004,
                payload_type: PAYLOAD_TYPE,
                sample_rate: SAMPLE_RATE,
                channels: 1,
                channels_map: vec![0],
                output_channels: 1,
                block_frames: BLOCK_FRAMES,
                jitter_target_frames: 0,
            },
            engine: EngineConfig {
                block_frames: BLOCK_FRAMES,
                sample_rate: SAMPLE_RATE,
                channels: 1,
            },
            device: DeviceConfig {
                device: DeviceId::new(device),
                sample_rate: SAMPLE_RATE,
                block_frames: BLOCK_FRAMES,
                channels: 1,
            },
            safety: SafetyPolicy {
                max_consecutive_underruns: 10_000,
                max_consecutive_overruns: 10_000,
            },
            receiver: ReceiverConfig::default(),
            supervisor: SupervisorOptions::default(),
            unloaded_policy: UnloadedPolicy::Passthrough,
        }
    }

    /// 假节拍器：sleep 只推进逻辑时间（测试不真等 2..5 s）。
    #[derive(Default)]
    struct TestPacer {
        now: u64,
        sleeps: u64,
    }

    impl LoopPacer for TestPacer {
        fn now_ms(&mut self) -> u64 {
            self.now
        }

        fn sleep_ms(&mut self, ms: u64) {
            self.sleeps += 1;
            self.now = self.now.saturating_add(ms.max(1));
        }
    }

    // -------------------------------------------------------------------
    // 纯逻辑：退避 / 状态机 / 分类 / 日志限频
    // -------------------------------------------------------------------

    #[test]
    fn reopen_backoff_starts_at_two_seconds_and_caps_at_five() {
        assert_eq!(reopen_delay_ms(0), REOPEN_BASE_DELAY_MS);
        assert_eq!(reopen_delay_ms(1), 2_000);
        assert_eq!(reopen_delay_ms(2), 4_000);
        assert_eq!(reopen_delay_ms(3), REOPEN_MAX_DELAY_MS);
        assert_eq!(reopen_delay_ms(10), REOPEN_MAX_DELAY_MS);
        assert_eq!(reopen_delay_ms(u64::MAX), REOPEN_MAX_DELAY_MS);
    }

    #[test]
    fn lifecycle_tracks_absent_reopen_attempts_and_recovery() {
        let mut lifecycle = DeviceLifecycle::new();
        assert!(!lifecycle.is_absent());
        lifecycle.note_absent(1_000, "alsa pcm disconnected");
        assert!(lifecycle.is_absent());
        assert_eq!(lifecycle.absent_since_ms(), Some(1_000));
        assert_eq!(lifecycle.reopen_attempts(), 0);
        assert_eq!(lifecycle.last_device_error(), Some("alsa pcm disconnected"));
        assert!(lifecycle.should_attempt(1_000), "掉线后允许立即重试一次");
        assert_eq!(lifecycle.note_attempt(1_000), 1);
        assert_eq!(lifecycle.next_attempt_at_ms(), 3_000);
        assert!(!lifecycle.should_attempt(2_999));
        assert!(lifecycle.should_attempt(3_000));
        assert_eq!(lifecycle.note_attempt(3_000), 2);
        assert_eq!(lifecycle.next_attempt_at_ms(), 7_000);
        lifecycle.note_attempt_failed(3_000, "open failed: no such device");
        assert_eq!(
            lifecycle.last_device_error(),
            Some("open failed: no such device")
        );
        assert_eq!(lifecycle.next_attempt_at_ms(), 7_000);
        // 重复 note_absent 不重置 absent_since/attempts（仍是同一次缺失）。
        lifecycle.note_absent(7_500, "still absent");
        assert_eq!(lifecycle.absent_since_ms(), Some(1_000));
        assert_eq!(lifecycle.reopen_attempts(), 2);
        assert_eq!(lifecycle.note_attempt(7_500), 3);
        assert!(lifecycle.note_recovered(9_000));
        assert!(!lifecycle.is_absent());
        assert_eq!(lifecycle.absent_since_ms(), None);
        assert_eq!(lifecycle.reopen_attempts(), 0);
        assert_eq!(lifecycle.recoveries(), 1);
        assert!(!lifecycle.note_recovered(9_100), "present 时重复恢复不计次");
        // 第二次掉线重新计时。
        lifecycle.note_absent(20_000, "device lost again");
        assert_eq!(lifecycle.absent_since_ms(), Some(20_000));
        assert_eq!(lifecycle.reopen_attempts(), 0);
        assert_eq!(lifecycle.recoveries(), 1);
    }

    #[test]
    fn attempt_logging_is_throttled_but_first_failure_always_logs() {
        let mut lifecycle = DeviceLifecycle::new();
        lifecycle.note_absent(0, "absent");
        let mut logged = Vec::new();
        for attempt in 1..=25u64 {
            let now = attempt * 10_000;
            lifecycle.note_attempt(now);
            lifecycle.note_attempt_failed(now, "open failed");
            if lifecycle.should_log_attempt() {
                logged.push(attempt);
            }
        }
        assert_eq!(logged, vec![1, 10, 20], "首次 + 每 10 次一条");
    }

    #[test]
    fn classifies_device_loss_versus_unrecoverable_errors() {
        assert!(is_recoverable_device_loss(&RuntimeError::Backend(
            BackendError::Fault("alsa pcm disconnected".to_string())
        )));
        assert!(is_recoverable_device_loss(&RuntimeError::Backend(
            BackendError::NotStarted
        )));
        assert!(!is_recoverable_device_loss(&RuntimeError::Backend(
            BackendError::BadConfig("bad device")
        )));
        assert!(!is_recoverable_device_loss(&RuntimeError::Backend(
            BackendError::WrongBlockLength {
                expected: 128,
                actual: 64,
            }
        )));
        assert!(!is_recoverable_device_loss(&RuntimeError::Config(
            "bad runtime config"
        )));
        assert!(!is_recoverable_device_loss(&RuntimeError::Engine(
            EngineError::BadConfig("bad engine")
        )));
    }

    // -------------------------------------------------------------------
    // 端到端：启动 absent → 运行中断线 → 恢复 → 再次断线 → 恢复并跑完
    // -------------------------------------------------------------------

    #[test]
    fn drive_loop_survives_startup_absence_and_runtime_disconnects() {
        let device = format!("rt-host-test-scripted-pcm-{}", std::process::id());
        let state = ScriptState::default();
        state.fail_opens.store(2, Ordering::SeqCst); // 启动时设备不在位，第一次重开也失败
        {
            let mut queue = state.fail_write_at.lock().expect("script queue");
            queue.push_back(2); // 运行中断线 #1
            queue.push_back(4); // 恢复后再次断线 #2
        }
        register_scripted_pcm(&device, &state);

        let (source, handle) = test_source();
        for index in 0..64u16 {
            handle.send(packet(index, u32::from(index) * BLOCK_FRAMES as u32));
        }
        let mut runtime =
            Runtime::<ScriptedPcm>::with_source_deferred(runtime_config(&device), source)
                .expect("deferred runtime");
        assert!(
            !runtime.device_started(),
            "deferred constructor must not open the device"
        );

        let options = LoopOptions {
            alsa_device: "hw:99".to_string(),
            proc_asound: std::env::temp_dir().join("khaen-rt-host-tests-no-asound"),
            status_file: None,
            run_seconds: 1,
            stats_every: 0,
        };
        let mut pacer = TestPacer::default();
        let (exit, lifecycle) = drive_loop(&mut runtime, &options, &mut pacer);

        // 循环没有退出（正常跑完），设备缺失/断线都只是 fail-closed 重开。
        assert_eq!(exit, ExitCode::SUCCESS);
        // 1 s @ 48 kHz / 128 = 375 块；两次写失败不推进本地块计数，运行时内部块数 +2。
        assert_eq!(runtime.blocks(), 375 + 2);
        assert!(!lifecycle.is_absent());
        assert_eq!(
            lifecycle.recoveries(),
            3,
            "startup absence + 2 runtime disconnects"
        );
        assert_eq!(lifecycle.reopen_attempts(), 0);
        assert_eq!(
            state.opens.load(Ordering::SeqCst),
            5,
            "2 failed startup opens + 3 successful reopens"
        );
        assert_eq!(
            state.writes.load(Ordering::SeqCst),
            375 + 2,
            "375 成功写 + 2 次掉线失败写"
        );
        assert_eq!(state.write_ok.load(Ordering::SeqCst), 375);
        assert_eq!(
            state.closed.load(Ordering::SeqCst),
            2,
            "两次掉线各关闭旧句柄一次"
        );
        assert!(runtime.device_started(), "跑完后设备已恢复");
        assert!(pacer.sleeps > 0, "启动 absent 至少睡过一次退避");
        // 运行期两次写失败：out_errors 计数、last_error 稳定码、最终回到 running。
        assert_eq!(runtime.status().output_errors, 2);
        assert_eq!(
            runtime.status().last_error,
            Some(khaen_host::runtime::codes::BACKEND)
        );
        assert_eq!(
            runtime.status().state,
            khaen_host::safety::RuntimeState::Running
        );
    }

    /// T70f-a：控制通道与 T42o 恢复循环共存——设备 absent 期间命令照常受理，
    /// 图/静音在恢复出块后的第一个块边界生效；status 快照照常发布（设备字段如实 absent）。
    #[test]
    fn drive_loop_with_control_serves_commands_while_device_absent() {
        let device = format!("rt-host-test-control-absent-{}", std::process::id());
        let state = ScriptState::default();
        state.fail_opens.store(2, Ordering::SeqCst); // 启动 absent + 首次重开失败
        register_scripted_pcm(&device, &state);

        let (source, handle) = test_source();
        for index in 0..64u16 {
            handle.send(packet(index, u32::from(index) * BLOCK_FRAMES as u32));
        }
        let mut runtime =
            Runtime::<ScriptedPcm>::with_source_deferred(runtime_config(&device), source)
                .expect("deferred runtime");

        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let snapshot: std::sync::Arc<
            std::sync::Mutex<Option<crate::control::snapshot::RuntimeSnapshot>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut bridge = crate::control::ControlBridge::new(command_rx, snapshot.clone(), 0);
        let options = LoopOptions {
            alsa_device: "hw:99".to_string(),
            proc_asound: std::env::temp_dir().join("khaen-rt-host-tests-no-asound"),
            status_file: None,
            run_seconds: 1,
            stats_every: 0,
        };
        let mut pacer = TestPacer::default();

        // 设备还没打开就排入静音命令：absent 退避睡眠的小片轮询必须把它受理并回执。
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        command_tx
            .send(crate::control::command::ControlCommand::Mute {
                mute: true,
                ack: ack_tx,
            })
            .expect("queue mute");

        let (exit, lifecycle) =
            drive_loop_with_control(&mut runtime, &options, &mut pacer, &mut bridge);
        assert_eq!(exit, ExitCode::SUCCESS);
        assert!(!lifecycle.is_absent());
        match ack_rx.recv().expect("mute acked while device absent") {
            crate::control::command::CommandReply::Mute {
                mute,
                applied_block,
                idempotent,
            } => {
                assert!(mute);
                assert_eq!(applied_block, 1, "no block had run when the mute was taken");
                assert!(!idempotent);
            }
            other => panic!("unexpected reply: {other:?}"),
        }
        assert!(
            runtime.muted(),
            "mute must take effect at the first recovered block"
        );

        let snapshot = snapshot
            .lock()
            .expect("snapshot lock")
            .clone()
            .expect("block loop published a snapshot");
        assert_eq!(
            snapshot.status.blocks, 375,
            "1 s @ 48 kHz / 128 = 375 blocks"
        );
        assert!(snapshot.status.muted);
        // 最终状态：设备已恢复（present），快照是恢复后发布的。
        assert!(!snapshot.lifecycle.is_absent());
        assert!(!snapshot.status.switch_pending);
    }

    /// T70f-c：校准播放期间块循环走 Runtime::run_calibration_block（不跑节目图），
    /// 扫频块写完后转收尾静音；结束后回到节目块路径。脚本化假 PCM 逐块校验字节长度
    /// （脏写/长度错会 panic），因此这里同时覆盖 "run_block 在 cal 模式不 panic/不脏写"。
    #[test]
    fn drive_loop_plays_calibration_playback_then_returns_to_program() {
        use khaen_host::runtime::{CalibrationPlayback, CalibrationProgress};

        use crate::control::command::{CommandReply, ControlCommand};

        let device = format!("rt-host-test-cal-playback-{}", std::process::id());
        let state = ScriptState::default();
        register_scripted_pcm(&device, &state);
        let (source, handle) = test_source();
        for index in 0..64u16 {
            handle.send(packet(index, u32::from(index) * BLOCK_FRAMES as u32));
        }
        let mut runtime =
            Runtime::<ScriptedPcm>::with_source_deferred(runtime_config(&device), source)
                .expect("deferred runtime");
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let snapshot: std::sync::Arc<
            std::sync::Mutex<Option<crate::control::snapshot::RuntimeSnapshot>>,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut bridge = crate::control::ControlBridge::new(command_rx, snapshot.clone(), 0);
        let mut options = LoopOptions {
            alsa_device: "hw:99".to_string(),
            proc_asound: std::env::temp_dir().join("khaen-rt-host-tests-no-asound"),
            status_file: None,
            run_seconds: 1,
            stats_every: 0,
        };
        let mut pacer = TestPacer::default();

        // 2 块已知扫频数据（块长 128、1 通道，与运行时格式一致）。
        let progress = std::sync::Arc::new(CalibrationProgress::new(2));
        let mut data = vec![0.0f32; BLOCK_FRAMES * 2];
        data[..BLOCK_FRAMES].fill(0.5);
        data[BLOCK_FRAMES..].fill(-0.25);
        let playback =
            CalibrationPlayback::new(data, BLOCK_FRAMES, 1, std::sync::Arc::clone(&progress))
                .expect("calibration playback buffer");
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        command_tx
            .send(ControlCommand::CalPlaybackBegin {
                playback,
                ack: ack_tx,
            })
            .expect("queue calibration playback");

        let (exit, lifecycle) =
            drive_loop_with_control(&mut runtime, &options, &mut pacer, &mut bridge);
        assert_eq!(exit, ExitCode::SUCCESS);
        assert!(!lifecycle.is_absent());
        match ack_rx.recv().expect("calibration playback acked") {
            CommandReply::CalPlaybackStarted { total_blocks } => assert_eq!(total_blocks, 2),
            other => panic!("unexpected calibration playback reply: {other:?}"),
        }
        // 循环跑满 1 s（375 块）：前 2 块写扫频，其余写收尾静音；进度如实 finished。
        assert_eq!(runtime.calibration_playback_progress(), Some((2, 2)));
        assert!(progress.finished(), "calibration progress must finish");
        assert_eq!(runtime.status().blocks, 375);
        assert_eq!(
            runtime.status().graph.version,
            None,
            "calibration playback must not stage/commit a plan"
        );
        let writes_during_calibration = state.write_ok.load(Ordering::SeqCst);
        assert!(
            writes_during_calibration >= 375,
            "calibration playback must keep writing the device (got {writes_during_calibration})"
        );
        // 结束校准播放再跑一轮：回到节目块路径（继续写设备、块号继续增长）。
        assert!(runtime.end_calibration_playback());
        assert!(!runtime.calibration_playback_active());
        options.run_seconds = 1;
        let (exit, _) = drive_loop_with_control(&mut runtime, &options, &mut pacer, &mut bridge);
        assert_eq!(exit, ExitCode::SUCCESS);
        assert!(
            state.write_ok.load(Ordering::SeqCst) > writes_during_calibration,
            "program blocks must resume after calibration playback ends"
        );
        let snapshot = snapshot
            .lock()
            .expect("snapshot lock")
            .clone()
            .expect("block loop published a snapshot");
        assert!(snapshot.status.blocks > 375);
    }
}
