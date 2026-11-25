//! 设备生命周期状态机（T70c）：枚举检测、控制通道、速率断言与回读、状态上报。
//!
//! 这一层把 v1 里靠人工/oneshot 服务完成的事（拔插后重新拉起控制通道、重设采样率、
//! 确认速率真的生效）收进一个显式状态机：设备事件一变就自动重跑，而不是等定时器或人工补。
//!
//! ## 状态机
//!
//! ```text
//! Absent --present+owner--> Enumerated --control ok--> ControlReady
//!    ^                          ^                          |
//!    |                          +-- control 失败(重试) -----+   写入速率并回读一致(连续 N 次)
//!    |                                                     v
//!    +-- 设备消失(安全静音) -- Streaming <--按方案速率开流-- RateAsserted
//! ```
//!
//! * 每次 DeviceSupervisor::poll 最多推进一步，上层按固定节拍轮询即可，不忙等。
//! * 每个状态入口都有进入条件与稳定失败码（SupervisorErrorCode）；失败记录里带
//!   fallback 字段，明确"退到哪一步"重试。
//! * 同一阶段连续失败超过 SupervisorPolicy::retry_limit 后锁存：进安全静音并停止自动
//!   尝试，等设备出现边沿（拔插）或显式 DeviceSupervisor::request_retry。
//! * 设备在 Streaming 中消失：立即 fail-closed（安全静音），退回 Absent 并清空
//!   device_rate / rate_asserted / owner / 流状态；重新出现后整条链路重跑一遍。
//! * 速率每次枚举都重新断言，绝不沿用设备保存的旧速率（DESIGN.md T70）。
//!
//! ## 与 SafetySupervisor 协作
//!
//! 本模块不实现静音逻辑：失败只调用 SafetySupervisor::note_processor_fault
//! （safety.rs 没有单独的"设备故障"原因，按通用故障处理，消息里带稳定错误码），
//! 由它决定 requires_mute。设备链路重新走通（再次进入 Streaming）时调用
//! SafetySupervisor::clear_fault 显式恢复，但只清本监督器自己造成的故障
//! （fault_active 标记）；音频链路的欠载/溢出故障仍归 SafetySupervisor 自己管。
//! 因此 DeviceSupervisor::poll 接收 &mut SafetySupervisor，两者由 daemon 同时持有，
//! 避免自引用结构。
//!
//! ## 时钟与重试
//!
//! 退避间隔与状态时间戳都走注入的 ClockSource：测试用假时钟验证"间隔没到就不重试"，
//! 生产用 MonotonicClock；DeviceSupervisor::poll 在退避期只读状态，不调用探针。
//!
//! ## 平台无关
//!
//! 所有系统交互都在 DeviceProbe / RateControl / StreamControl 后面：
//! - 测试用假实现覆盖完整转移、重试、锁存、拔插自恢复；
//! - SysfsProbe 只用普通 std::fs 读 sysfs/proc，测试用临时目录模拟结构（Windows 可跑）；
//! - CommandRateControl 通过可配置的外部命令读写速率；本机（Windows）不执行真实命令，
//!   Linux 上的实际命令映射属于部署配置，标为 NOT_RUN。

use std::fmt;
use std::path::PathBuf;
use std::time::Instant;

use crate::owner::{DeviceGuard, DeviceId, OwnerError};
use crate::safety::SafetySupervisor;

/// 设备生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifecycleState {
    /// 设备不在总线上（初始状态）。
    Absent,
    /// 设备已出现，且监督器取得了它的单设备所有权。
    Enumerated,
    /// 控制通道可用（MOTU：网口/地址/ping 或控制项可读；ALSA：控制节点可读）。
    ControlReady,
    /// 方案速率已写入并连续回读一致。
    RateAsserted,
    /// 已按方案速率打开音频流。
    Streaming,
}

impl LifecycleState {
    /// 稳定字符串（状态上报/协议用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Enumerated => "enumerated",
            Self::ControlReady => "control_ready",
            Self::RateAsserted => "rate_asserted",
            Self::Streaming => "streaming",
        }
    }
}

impl fmt::Display for LifecycleState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 设备生命周期的稳定错误码。
///
/// 前五个对应 DESIGN.md「设备生命周期」表的失败码；device_busy 是
/// device_open_failed 里"同进程内已有 owner"这一具体情形，单独给出便于诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupervisorErrorCode {
    /// 设备不在总线上（device_absent）：运行中设备消失时上报。
    DeviceAbsent,
    /// 设备打不开/枚举失败（device_open_failed）。
    DeviceOpenFailed,
    /// 单设备 owner 冲突（device_busy）：同进程内已有别的 owner。
    DeviceBusy,
    /// 控制通道不可用（control_channel_unavailable）。
    ControlChannelUnavailable,
    /// 速率断言失败（rate_assert_failed）：写不进或写后回读不一致。
    RateAssertFailed,
    /// 速率不一致（rate_mismatch）：回读值与方案速率不符。
    RateMismatch,
    /// 音频流打开失败（stream_open_failed）。
    StreamOpenFailed,
}

impl SupervisorErrorCode {
    /// 稳定字符串（协议/日志用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeviceAbsent => "device_absent",
            Self::DeviceOpenFailed => "device_open_failed",
            Self::DeviceBusy => "device_busy",
            Self::ControlChannelUnavailable => "control_channel_unavailable",
            Self::RateAssertFailed => "rate_assert_failed",
            Self::RateMismatch => "rate_mismatch",
            Self::StreamOpenFailed => "stream_open_failed",
        }
    }
}

impl fmt::Display for SupervisorErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 注入的探针/控制通道错误；只带人看的说明，稳定错误码由监督器按阶段决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeError {
    message: String,
}

impl ProbeError {
    /// 由说明文本构造。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 说明文本。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 取出说明文本。
    pub fn into_message(self) -> String {
        self.message
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProbeError {}

/// 一条设备生命周期失败记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorError {
    /// 稳定错误码。
    pub code: SupervisorErrorCode,
    /// 人看的说明（含实测值/原因）。
    pub message: String,
    /// 失败后状态机退回的状态（"退到哪一步"重试）。
    pub fallback: LifecycleState,
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} (fallback {})",
            self.code, self.message, self.fallback
        )
    }
}

/// 状态快照（给 daemon status 用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorStatus {
    /// 当前生命周期状态。
    pub state: LifecycleState,
    /// 方案速率（唯一事实来源；来自方案而不是设备保存值）。
    pub plan_rate: u32,
    /// 最近一次从设备读到的速率（还没读过为 None）。
    pub device_rate: Option<u32>,
    /// 速率是否已断言并连续回读一致（进入 RateAsserted/Streaming 后才为 true）。
    pub rate_asserted: bool,
    /// 最近一次失败（成功后清空；锁存期间保留原因）。
    pub last_error: Option<SupervisorError>,
    /// 当前阶段的连续失败次数（成功进入下一阶段后清零）。
    pub attempts: u32,
    /// 进入当前状态（或最近一次锁存）的注入时钟时间戳，毫秒。
    pub since_ms: u64,
}

/// 监督策略：重试次数、退避间隔、稳定判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorPolicy {
    /// 每个阶段在首次之外允许的重试次数；再失败就锁存并安全静音。
    pub retry_limit: u32,
    /// 失败后的退避间隔（毫秒）；退避期内 poll 不做系统调用。
    pub retry_interval_ms: u64,
    /// 速率稳定判定：写入后必须连续 N 次回读等于方案速率才进 RateAsserted。
    ///
    /// 构造时钳到 >= 1（0 没有意义）。
    pub stable_reads: u32,
    /// 是否在 Streaming 中周期性回读设备速率（发现漂移立即 fail-closed）。
    pub verify_rate_while_streaming: bool,
    /// Streaming 中回读速率的间隔（毫秒）。
    pub streaming_verify_interval_ms: u64,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            retry_limit: 3,
            retry_interval_ms: 250,
            stable_reads: 2,
            verify_rate_while_streaming: true,
            streaming_verify_interval_ms: 1_000,
        }
    }
}

/// 设备存在性与控制通道拉起的注入接口。
pub trait DeviceProbe {
    /// 设备标识（用于单设备 owner 注册，和守护进程其它组件共享同一命名空间）。
    fn device_id(&self) -> &DeviceId;

    /// 设备是否在总线上（对应 sysfs / USB 枚举 / 网口可达）。
    ///
    /// 只读检查，必须便宜；不得在这里做写操作。
    fn is_present(&mut self) -> bool;

    /// 拉起控制通道（等价 v1 的 tspi-motu-net-up：网口 up、地址、ping/WS；
    /// ALSA 路径则是控制项可读）。成功才允许进入 ControlReady。
    fn bring_up_control(&mut self) -> Result<(), ProbeError>;
}

/// 速率控制注入接口（写 + 读回）。
pub trait RateControl {
    /// 读设备当前速率。读不到返回错误，绝不能猜。
    fn read_rate(&mut self) -> Result<u32, ProbeError>;

    /// 把设备设到 rate 并回读，返回回读到的实际速率。
    ///
    /// 监督器会把返回值与方案速率比对；不相等按速率断言失败处理。
    fn assert_rate(&mut self, rate: u32) -> Result<u32, ProbeError>;
}

/// 音频流开关注入接口。
pub trait StreamControl {
    /// 按 rate 打开音频流。
    fn open_stream(&mut self, rate: u32) -> Result<(), ProbeError>;

    /// 关闭音频流；只用于失败/拔插的清理路径，错误可以忽略。
    fn close_stream(&mut self) -> Result<(), ProbeError>;
}

/// 注入时钟接口（毫秒）。
pub trait ClockSource {
    /// 单调递增的毫秒时间戳。
    fn now_ms(&mut self) -> u64;
}

/// 未接音频流的占位实现：只验证生命周期与速率，开/关流都立即成功。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NullStream;

impl StreamControl for NullStream {
    fn open_stream(&mut self, _rate: u32) -> Result<(), ProbeError> {
        Ok(())
    }

    fn close_stream(&mut self) -> Result<(), ProbeError> {
        Ok(())
    }
}

/// 生产用单调时钟：构造时刻为 0。
#[derive(Debug)]
pub struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    /// 以当前时刻为原点。
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ClockSource for MonotonicClock {
    fn now_ms(&mut self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// 设备生命周期监督器：状态机、重试、速率断言、插拔自恢复。
///
/// 泛型参数：
/// - P 设备探针（存在性 + 控制通道）；
/// - R 速率控制（写 + 读回）；
/// - S 音频流开关（默认 NullStream，真机由 daemon 接 ALSA 后端）；
/// - C 时钟（默认 MonotonicClock，测试注入假时钟）。
#[derive(Debug)]
pub struct DeviceSupervisor<
    P: DeviceProbe,
    R: RateControl,
    S: StreamControl = NullStream,
    C: ClockSource = MonotonicClock,
> {
    plan_rate: u32,
    policy: SupervisorPolicy,
    probe: P,
    rate: R,
    stream: S,
    clock: C,
    state: LifecycleState,
    device_rate: Option<u32>,
    rate_asserted: bool,
    last_error: Option<SupervisorError>,
    attempts: u32,
    since_ms: u64,
    next_attempt_ms: u64,
    stable_matches: u32,
    stabilizing: bool,
    verify_at_ms: u64,
    guard: Option<DeviceGuard>,
    last_present: bool,
    latched: bool,
    fault_active: bool,
}

impl<P: DeviceProbe, R: RateControl> DeviceSupervisor<P, R, NullStream, MonotonicClock> {
    /// 新建：未接音频流（NullStream）+ 单调时钟。
    ///
    /// plan_rate 来自方案，必须非零；0 属于上游准入错误（admission 会先拒掉）。
    pub fn new(plan_rate: u32, policy: SupervisorPolicy, probe: P, rate: R) -> Self {
        Self::with_components(
            plan_rate,
            policy,
            probe,
            rate,
            NullStream,
            MonotonicClock::new(),
        )
    }
}

impl<P, R, S, C> DeviceSupervisor<P, R, S, C>
where
    P: DeviceProbe,
    R: RateControl,
    S: StreamControl,
    C: ClockSource,
{
    /// 用显式组件构造（测试注入假时钟/假流）。
    pub fn with_components(
        plan_rate: u32,
        policy: SupervisorPolicy,
        probe: P,
        rate: R,
        stream: S,
        mut clock: C,
    ) -> Self {
        assert!(plan_rate > 0, "plan_rate must be non-zero");
        let mut policy = policy;
        policy.stable_reads = policy.stable_reads.max(1);
        let now = clock.now_ms();
        Self {
            plan_rate,
            policy,
            probe,
            rate,
            stream,
            clock,
            state: LifecycleState::Absent,
            device_rate: None,
            rate_asserted: false,
            last_error: None,
            attempts: 0,
            since_ms: now,
            next_attempt_ms: now,
            stable_matches: 0,
            stabilizing: false,
            verify_at_ms: now,
            guard: None,
            last_present: false,
            latched: false,
            fault_active: false,
        }
    }

    /// 当前状态。
    pub fn state(&self) -> LifecycleState {
        self.state
    }

    /// 方案速率。
    pub fn plan_rate(&self) -> u32 {
        self.plan_rate
    }

    /// 最近一次从设备读到的速率。
    pub fn device_rate(&self) -> Option<u32> {
        self.device_rate
    }

    /// 速率是否已断言并回读一致。
    pub fn rate_asserted(&self) -> bool {
        self.rate_asserted
    }

    /// 最近一次失败。
    pub fn last_error(&self) -> Option<&SupervisorError> {
        self.last_error.as_ref()
    }

    /// 状态快照（给 daemon status 用；只在有错误时分配错误文本）。
    pub fn status(&self) -> SupervisorStatus {
        SupervisorStatus {
            state: self.state,
            plan_rate: self.plan_rate,
            device_rate: self.device_rate,
            rate_asserted: self.rate_asserted,
            last_error: self.last_error.clone(),
            attempts: self.attempts,
            since_ms: self.since_ms,
        }
    }

    /// 驱动状态机走一步，并把设备故障上报给 SafetySupervisor。
    ///
    /// 每次只推进一步（至多一次外部调用）；退避期内不调用探针。返回最新快照。
    pub fn poll(&mut self, safety: &mut SafetySupervisor) -> SupervisorStatus {
        let now = self.clock.now_ms();
        let present = self.probe.is_present();
        let appeared = present && !self.last_present;
        self.last_present = present;

        if appeared {
            // 设备（重新）出现：解除锁存，允许状态机从 Absent 重跑（插拔自恢复入口）。
            self.latched = false;
            self.attempts = 0;
            self.next_attempt_ms = now;
        }

        if !present {
            if self.state != LifecycleState::Absent {
                let previous = self.state;
                if previous == LifecycleState::Streaming {
                    let _ = self.stream.close_stream();
                }
                self.enter_absent(now);
                if !self.fault_active {
                    self.note_device_fault(
                        SupervisorErrorCode::DeviceAbsent,
                        format!("device disappeared while {previous}"),
                        LifecycleState::Absent,
                        safety,
                    );
                }
            }
            return self.status();
        }

        if self.latched {
            return self.status();
        }

        match self.state {
            LifecycleState::Absent => self.poll_absent(now, safety),
            LifecycleState::Enumerated => self.poll_enumerated(now, safety),
            LifecycleState::ControlReady => self.poll_control_ready(now, safety),
            LifecycleState::RateAsserted => self.poll_rate_asserted(now, safety),
            LifecycleState::Streaming => self.poll_streaming(now, safety),
        }
        self.status()
    }

    /// 显式请求重新尝试：清除锁存与退避（不改变 SafetySupervisor 的静音状态；
    /// 只有链路重新走到 Streaming 才会显式清故障）。
    pub fn request_retry(&mut self) {
        if self.latched {
            self.latched = false;
            self.attempts = 0;
            self.next_attempt_ms = 0;
        }
    }

    // ---- 各状态的一步推进 -------------------------------------------------

    fn poll_absent(&mut self, now: u64, safety: &mut SafetySupervisor) {
        if now < self.next_attempt_ms {
            return;
        }
        let device_id = self.probe.device_id().clone();
        match DeviceGuard::acquire(device_id.as_str()) {
            Ok(guard) => {
                self.guard = Some(guard);
                self.enter(LifecycleState::Enumerated, now);
            }
            Err(OwnerError::AlreadyOwned(owner)) => {
                self.fail_attempt(
                    SupervisorErrorCode::DeviceBusy,
                    format!("device {owner} already owned by another component"),
                    LifecycleState::Absent,
                    safety,
                    now,
                );
            }
            Err(OwnerError::EmptyDeviceId) => {
                self.fail_attempt(
                    SupervisorErrorCode::DeviceOpenFailed,
                    String::from("probe reported an empty device id"),
                    LifecycleState::Absent,
                    safety,
                    now,
                );
            }
        }
    }

    fn poll_enumerated(&mut self, now: u64, safety: &mut SafetySupervisor) {
        if now < self.next_attempt_ms {
            return;
        }
        match self.probe.bring_up_control() {
            Ok(()) => self.enter(LifecycleState::ControlReady, now),
            Err(err) => self.fail_attempt(
                SupervisorErrorCode::ControlChannelUnavailable,
                err.into_message(),
                LifecycleState::Enumerated,
                safety,
                now,
            ),
        }
    }

    fn poll_control_ready(&mut self, now: u64, safety: &mut SafetySupervisor) {
        if now < self.next_attempt_ms {
            return;
        }
        if self.stabilizing {
            self.verify_rate(now, safety);
            return;
        }
        match self.rate.assert_rate(self.plan_rate) {
            Ok(actual) => {
                self.device_rate = Some(actual);
                if actual != self.plan_rate {
                    self.fail_attempt(
                        SupervisorErrorCode::RateAssertFailed,
                        format!("assert_rate({}) read back {actual}", self.plan_rate),
                        LifecycleState::ControlReady,
                        safety,
                        now,
                    );
                    return;
                }
                // 写入成功只是本阶段的一步；回读没稳定居中前不算成功，不能清失败计数，
                // 否则"写成功但回读始终不一致"会无限重试。
                self.stable_matches = 1;
                self.stabilizing = true;
                self.next_attempt_ms = now;
                if self.stable_matches >= self.policy.stable_reads {
                    self.enter_rate_asserted(now);
                }
            }
            Err(err) => self.fail_attempt(
                SupervisorErrorCode::RateAssertFailed,
                err.into_message(),
                LifecycleState::ControlReady,
                safety,
                now,
            ),
        }
    }

    /// 速率稳定判定：连续 stable_reads 次回读等于方案速率才算断言成功。
    fn verify_rate(&mut self, now: u64, safety: &mut SafetySupervisor) {
        match self.rate.read_rate() {
            Ok(actual) => {
                self.device_rate = Some(actual);
                if actual != self.plan_rate {
                    self.stabilizing = false;
                    self.stable_matches = 0;
                    self.fail_attempt(
                        SupervisorErrorCode::RateMismatch,
                        format!("rate read back {actual}, plan is {}", self.plan_rate),
                        LifecycleState::ControlReady,
                        safety,
                        now,
                    );
                    return;
                }
                self.stable_matches = self.stable_matches.saturating_add(1);
                if self.stable_matches >= self.policy.stable_reads {
                    self.enter_rate_asserted(now);
                }
            }
            Err(err) => {
                self.stabilizing = false;
                self.stable_matches = 0;
                self.fail_attempt(
                    SupervisorErrorCode::RateAssertFailed,
                    err.into_message(),
                    LifecycleState::ControlReady,
                    safety,
                    now,
                );
            }
        }
    }

    fn poll_rate_asserted(&mut self, now: u64, safety: &mut SafetySupervisor) {
        if now < self.next_attempt_ms {
            return;
        }
        match self.stream.open_stream(self.plan_rate) {
            Ok(()) => {
                self.enter(LifecycleState::Streaming, now);
                self.verify_at_ms = now.saturating_add(self.policy.streaming_verify_interval_ms);
                self.recover_from_device_fault(safety);
            }
            Err(err) => self.fail_attempt(
                SupervisorErrorCode::StreamOpenFailed,
                err.into_message(),
                LifecycleState::RateAsserted,
                safety,
                now,
            ),
        }
    }

    fn poll_streaming(&mut self, now: u64, safety: &mut SafetySupervisor) {
        if !self.policy.verify_rate_while_streaming || now < self.verify_at_ms {
            return;
        }
        self.verify_at_ms = now.saturating_add(self.policy.streaming_verify_interval_ms);
        match self.rate.read_rate() {
            Ok(actual) => {
                self.device_rate = Some(actual);
                if actual != self.plan_rate {
                    let message = format!(
                        "streaming rate drifted to {actual}, plan is {}",
                        self.plan_rate
                    );
                    self.degrade(SupervisorErrorCode::RateMismatch, message, safety, now);
                }
            }
            Err(err) => {
                let message = format!("rate read failed while streaming: {}", err.message());
                self.degrade(
                    SupervisorErrorCode::ControlChannelUnavailable,
                    message,
                    safety,
                    now,
                );
            }
        }
    }

    // ---- 状态迁移与失败处理 -----------------------------------------------

    /// 成功迁移到下一状态（清失败计数与最近错误）。
    fn enter(&mut self, state: LifecycleState, now: u64) {
        self.state = state;
        self.since_ms = now;
        self.next_attempt_ms = now;
        self.attempts = 0;
        self.last_error = None;
        self.stabilizing = false;
        self.stable_matches = 0;
    }

    fn enter_rate_asserted(&mut self, now: u64) {
        self.enter(LifecycleState::RateAsserted, now);
        self.rate_asserted = true;
        self.device_rate = Some(self.plan_rate);
    }

    fn enter_absent(&mut self, now: u64) {
        self.state = LifecycleState::Absent;
        self.since_ms = now;
        self.device_rate = None;
        self.rate_asserted = false;
        self.attempts = 0;
        self.stable_matches = 0;
        self.stabilizing = false;
        self.next_attempt_ms = now;
        // 释放单设备所有权：Absent 状态下监督器不持有设备。
        self.guard = None;
    }

    /// 锁存：停止自动尝试，等设备出现边沿或显式 request_retry。
    fn latch(&mut self, fallback: LifecycleState, now: u64) {
        self.state = fallback;
        self.since_ms = now;
        self.latched = true;
        self.stabilizing = false;
        self.stable_matches = 0;
    }

    /// 记一次阶段失败；超过重试预算就锁存并安全静音。
    fn fail_attempt(
        &mut self,
        code: SupervisorErrorCode,
        message: String,
        fallback: LifecycleState,
        safety: &mut SafetySupervisor,
        now: u64,
    ) {
        self.attempts = self.attempts.saturating_add(1);
        self.stabilizing = false;
        self.stable_matches = 0;
        if self.attempts > self.policy.retry_limit {
            self.note_device_fault(code, message, fallback, safety);
            self.latch(fallback, now);
        } else {
            self.last_error = Some(SupervisorError {
                code,
                message,
                fallback,
            });
            self.next_attempt_ms = now.saturating_add(self.policy.retry_interval_ms);
        }
    }

    /// 流式阶段发现速率漂移/控制通道故障：立刻 fail-closed（安全静音），
    /// 退回 ControlReady 重断言；重新回到 Streaming 时清故障；重试耗尽则锁存。
    fn degrade(
        &mut self,
        code: SupervisorErrorCode,
        message: String,
        safety: &mut SafetySupervisor,
        now: u64,
    ) {
        let _ = self.stream.close_stream();
        self.rate_asserted = false;
        self.attempts = 0;
        self.next_attempt_ms = now;
        self.state = LifecycleState::ControlReady;
        self.since_ms = now;
        self.note_device_fault(code, message, LifecycleState::ControlReady, safety);
    }

    /// 记设备故障：只在尚未故障时通知 SafetySupervisor（不重复计故障）。
    fn note_device_fault(
        &mut self,
        code: SupervisorErrorCode,
        message: String,
        fallback: LifecycleState,
        safety: &mut SafetySupervisor,
    ) {
        self.last_error = Some(SupervisorError {
            code,
            message: message.clone(),
            fallback,
        });
        if self.fault_active {
            return;
        }
        self.fault_active = true;
        // 不自己实现静音：交给 SafetySupervisor 的既有接口（safety.rs 无设备故障原因，
        // 按通用处理器故障上报，稳定错误码放在消息里）。
        safety.note_processor_fault(format!("device supervisor: {}: {message}", code.as_str()));
    }

    /// 设备链路重新走通：清掉本监督器造成的安全静音，恢复显式完成。
    fn recover_from_device_fault(&mut self, safety: &mut SafetySupervisor) {
        if !self.fault_active {
            return;
        }
        let _ = safety.clear_fault();
        self.fault_active = false;
    }
}

// ---- Linux 参考实现（本机 Windows 不执行真实系统调用） --------------------

/// sysfs/proc 探针：用普通 std::fs 判断设备存在性与 ALSA 控制节点可读性。
///
/// 默认根为 /sys/class/sound 与 /proc/asound（Linux）；测试用临时目录模拟结构，
/// 因此该实现本身平台无关。MOTU 的"控制通道"不是 ALSA 控制项，而是 USB-CDC 网口，
/// 此时用 SysfsProbe::with_control_command 挂等价于 v1 tspi-motu-net-up 的命令。
#[derive(Debug, Clone)]
pub struct SysfsProbe {
    device_id: DeviceId,
    sysfs_root: PathBuf,
    proc_root: PathBuf,
    card_match: String,
    control_argv: Option<Vec<String>>,
}

impl SysfsProbe {
    /// 以设备标识构造；卡名匹配默认取设备标识（在 /sys/class/sound/card*/id 里匹配）。
    pub fn new(device_id: impl Into<String>) -> Self {
        let device_id = DeviceId::new(device_id);
        let card_match = device_id.as_str().to_string();
        Self {
            device_id,
            sysfs_root: PathBuf::from("/sys/class/sound"),
            proc_root: PathBuf::from("/proc/asound"),
            card_match,
            control_argv: None,
        }
    }

    /// 覆盖 sysfs / proc 根目录（测试用）。
    pub fn with_roots(
        mut self,
        sysfs_root: impl Into<PathBuf>,
        proc_root: impl Into<PathBuf>,
    ) -> Self {
        self.sysfs_root = sysfs_root.into();
        self.proc_root = proc_root.into();
        self
    }

    /// 覆盖卡名匹配串（默认等于设备标识）。
    pub fn with_card_match(mut self, card_match: impl Into<String>) -> Self {
        self.card_match = card_match.into();
        self
    }

    /// 挂控制通道命令（等价 tspi-motu-net-up）；不配置时退回 ALSA 控制节点只读检查。
    pub fn with_control_command(mut self, argv: Vec<String>) -> Self {
        self.control_argv = Some(argv);
        self
    }

    /// 在 sysfs 里找匹配的声卡目录，返回 (cardN, 目录)；按文件名排序保证确定性。
    fn find_card(&self) -> Option<(String, PathBuf)> {
        let mut entries: Vec<_> = std::fs::read_dir(&self.sysfs_root)
            .ok()?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        let needle = self.card_match.to_ascii_lowercase();
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with("card") {
                continue;
            }
            let id_path = entry.path().join("id");
            let Ok(id) = std::fs::read_to_string(id_path) else {
                continue;
            };
            if id.trim().to_ascii_lowercase().contains(&needle) {
                return Some((name.to_string(), entry.path()));
            }
        }
        None
    }
}

impl DeviceProbe for SysfsProbe {
    fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    fn is_present(&mut self) -> bool {
        self.find_card().is_some()
    }

    fn bring_up_control(&mut self) -> Result<(), ProbeError> {
        if let Some(argv) = &self.control_argv {
            return run_command(argv);
        }
        let (card, _dir) = self
            .find_card()
            .ok_or_else(|| ProbeError::new("device not present in sysfs"))?;
        let node = self
            .proc_root
            .join(&card)
            .join("pcm0p")
            .join("sub0")
            .join("hw_params");
        if node.is_file() {
            Ok(())
        } else {
            Err(ProbeError::new(format!(
                "ALSA control node unreadable: {}",
                node.display()
            )))
        }
    }
}

/// 由可配置外部命令实现的速率控制（{rate} 占位符会被替换成目标速率）。
///
/// 典型 Linux 部署：
/// - 读：motu-read-rate.sh（打印一个整数速率）；
/// - 写：motu-set-rate.sh {rate}（内部 aplay 静音流/写 CueMix，并自己回读）。
///
/// 本机（Windows）不执行真实命令，Linux 实际命令映射 = NOT_RUN；错误映射（命令不存在）
/// 在测试里覆盖。
#[derive(Debug, Clone)]
pub struct CommandRateControl {
    read_argv: Vec<String>,
    assert_argv: Vec<String>,
}

impl CommandRateControl {
    /// 构造：read_argv 读速率，assert_argv 写速率（含 {rate} 占位符；
    /// 不含占位符时把速率作为最后一个参数追加）。
    pub fn new(read_argv: Vec<String>, assert_argv: Vec<String>) -> Self {
        Self {
            read_argv,
            assert_argv,
        }
    }
}

impl RateControl for CommandRateControl {
    fn read_rate(&mut self) -> Result<u32, ProbeError> {
        let output = run_output(&self.read_argv)?;
        parse_first_u32(&output.stdout)
            .ok_or_else(|| ProbeError::new("read command printed no integer sample rate"))
    }

    fn assert_rate(&mut self, rate: u32) -> Result<u32, ProbeError> {
        let rate_text = rate.to_string();
        let mut argv = Vec::with_capacity(self.assert_argv.len() + 1);
        let mut substituted = false;
        for arg in &self.assert_argv {
            if arg.contains("{rate}") {
                substituted = true;
                argv.push(arg.replace("{rate}", &rate_text));
            } else {
                argv.push(arg.clone());
            }
        }
        if !substituted {
            argv.push(rate_text);
        }
        run_command(&argv)?;
        // 写入后必须回读：返回实际速率，由监督器与方案速率比对。
        self.read_rate()
    }
}

/// 跑命令并只要求退出码成功。
fn run_command(argv: &[String]) -> Result<(), ProbeError> {
    let output = run_output(argv)?;
    if output.status.success() {
        Ok(())
    } else {
        let program = argv.first().map(String::as_str).unwrap_or("<empty>");
        Err(ProbeError::new(format!(
            "command {program} exited with {}",
            output.status
        )))
    }
}

/// 跑命令并取回输出。
fn run_output(argv: &[String]) -> Result<std::process::Output, ProbeError> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| ProbeError::new("empty command"))?;
    std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|err| ProbeError::new(format!("spawn {program} failed: {err}")))
}

/// 从命令输出里取第一个十进制整数（容忍前后缀文本/换行）。
fn parse_first_u32(bytes: &[u8]) -> Option<u32> {
    let mut value: Option<u32> = None;
    for &byte in bytes {
        if byte.is_ascii_digit() {
            let digit = u32::from(byte - b'0');
            value = Some(value.unwrap_or(0).saturating_mul(10).saturating_add(digit));
        } else if value.is_some() {
            break;
        }
    }
    value
}
