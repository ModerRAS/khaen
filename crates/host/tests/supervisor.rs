//! 设备监督器测试：完整转移链、重试/锁存、速率稳定判定、拔插自恢复、单设备 owner
//! 冲突、状态快照字段、注入时钟退避、sysfs 模拟与外部命令错误映射。
//!
//! 全部用假实现注入，Windows 上可跑；真正的 Linux sysfs/proc 与 motu 命令映射不在本机验证
//! （见报告 NOT_RUN 项）。

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use khaen_host::owner::{DeviceGuard, DeviceId, OwnerError};
use khaen_host::safety::{FaultReason, SafetyPolicy, SafetySupervisor};
use khaen_host::supervisor::{
    ClockSource, CommandRateControl, DeviceProbe, DeviceSupervisor, LifecycleState, ProbeError,
    RateControl, StreamControl, SupervisorErrorCode, SupervisorPolicy, SupervisorStatus,
    SysfsProbe,
};

// ---- 假实现 ---------------------------------------------------------------

/// 可注入假时钟：外部句柄可推进（Rc<Cell> 与监督器共享）。
#[derive(Clone, Default)]
struct FakeClock {
    now_ms: Rc<Cell<u64>>,
}

impl FakeClock {
    fn at(now_ms: u64) -> Self {
        let clock = Self::default();
        clock.now_ms.set(now_ms);
        clock
    }

    fn advance(&self, delta_ms: u64) {
        self.now_ms.set(self.now_ms.get().saturating_add(delta_ms));
    }
}

impl ClockSource for FakeClock {
    fn now_ms(&mut self) -> u64 {
        self.now_ms.get()
    }
}

/// 探针假状态（外部句柄可模拟拔插/失败）。
#[derive(Clone, Default)]
struct ProbeState {
    present: Rc<Cell<bool>>,
    control_calls: Rc<Cell<u32>>,
    control_failures: Rc<Cell<u32>>,
}

impl ProbeState {
    fn set_present(&self, present: bool) {
        self.present.set(present);
    }

    fn fail_control(&self, times: u32) {
        self.control_failures.set(times);
    }

    fn control_calls(&self) -> u32 {
        self.control_calls.get()
    }
}

struct FakeProbe {
    device_id: DeviceId,
    state: ProbeState,
}

impl DeviceProbe for FakeProbe {
    fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    fn is_present(&mut self) -> bool {
        self.state.present.get()
    }

    fn bring_up_control(&mut self) -> Result<(), ProbeError> {
        self.state
            .control_calls
            .set(self.state.control_calls.get() + 1);
        let failures = self.state.control_failures.get();
        if failures > 0 {
            self.state.control_failures.set(failures - 1);
            return Err(ProbeError::new("net-up script returned 1"));
        }
        Ok(())
    }
}

/// 速率控制假状态（外部句柄可注入读/写失败与回读值）。
#[derive(Clone, Default)]
struct RateState {
    device_rate: Rc<Cell<u32>>,
    assert_calls: Rc<Cell<u32>>,
    read_calls: Rc<Cell<u32>>,
    assert_failures: Rc<Cell<u32>>,
    assert_override: Rc<Cell<Option<u32>>>,
    read_override: Rc<Cell<Option<u32>>>,
}

impl RateState {
    fn fail_assert(&self, times: u32) {
        self.assert_failures.set(times);
    }

    fn set_assert_override(&self, rate: Option<u32>) {
        self.assert_override.set(rate);
    }

    fn set_read_override(&self, rate: Option<u32>) {
        self.read_override.set(rate);
    }

    fn assert_calls(&self) -> u32 {
        self.assert_calls.get()
    }

    fn read_calls(&self) -> u32 {
        self.read_calls.get()
    }

    fn device_rate(&self) -> u32 {
        self.device_rate.get()
    }
}

struct FakeRateControl {
    state: RateState,
}

impl RateControl for FakeRateControl {
    fn read_rate(&mut self) -> Result<u32, ProbeError> {
        self.state.read_calls.set(self.state.read_calls.get() + 1);
        if let Some(rate) = self.state.read_override.get() {
            return Ok(rate);
        }
        Ok(self.state.device_rate.get())
    }

    fn assert_rate(&mut self, rate: u32) -> Result<u32, ProbeError> {
        self.state
            .assert_calls
            .set(self.state.assert_calls.get() + 1);
        let failures = self.state.assert_failures.get();
        if failures > 0 {
            self.state.assert_failures.set(failures - 1);
            return Err(ProbeError::new("assert_rate failed"));
        }
        if let Some(actual) = self.state.assert_override.get() {
            self.state.device_rate.set(actual);
            return Ok(actual);
        }
        self.state.device_rate.set(rate);
        Ok(rate)
    }
}

/// 音频流假状态。
#[derive(Clone, Default)]
struct StreamState {
    open_calls: Rc<Cell<u32>>,
    close_calls: Rc<Cell<u32>>,
    open_failures: Rc<Cell<u32>>,
    last_rate: Rc<Cell<Option<u32>>>,
}

impl StreamState {
    fn fail_open(&self, times: u32) {
        self.open_failures.set(times);
    }

    fn open_calls(&self) -> u32 {
        self.open_calls.get()
    }

    fn close_calls(&self) -> u32 {
        self.close_calls.get()
    }

    fn last_rate(&self) -> Option<u32> {
        self.last_rate.get()
    }
}

struct FakeStream {
    state: StreamState,
}

impl StreamControl for FakeStream {
    fn open_stream(&mut self, rate: u32) -> Result<(), ProbeError> {
        self.state.open_calls.set(self.state.open_calls.get() + 1);
        let failures = self.state.open_failures.get();
        if failures > 0 {
            self.state.open_failures.set(failures - 1);
            return Err(ProbeError::new("open_stream failed"));
        }
        self.state.last_rate.set(Some(rate));
        Ok(())
    }

    fn close_stream(&mut self) -> Result<(), ProbeError> {
        self.state.close_calls.set(self.state.close_calls.get() + 1);
        Ok(())
    }
}

// ---- 测试夹具 -------------------------------------------------------------

struct Harness {
    supervisor: DeviceSupervisor<FakeProbe, FakeRateControl, FakeStream, FakeClock>,
    clock: FakeClock,
    probe: ProbeState,
    rate: RateState,
    stream: StreamState,
    safety: SafetySupervisor,
}

impl Harness {
    fn new(device_id: &str, plan_rate: u32, policy: SupervisorPolicy, present: bool) -> Self {
        Self::at(device_id, plan_rate, policy, present, 0)
    }

    fn at(
        device_id: &str,
        plan_rate: u32,
        policy: SupervisorPolicy,
        present: bool,
        start_ms: u64,
    ) -> Self {
        let clock = FakeClock::at(start_ms);
        let probe = ProbeState::default();
        probe.set_present(present);
        let rate = RateState::default();
        rate.device_rate.set(plan_rate);
        let stream = StreamState::default();
        let supervisor = DeviceSupervisor::with_components(
            plan_rate,
            policy,
            FakeProbe {
                device_id: DeviceId::new(device_id),
                state: probe.clone(),
            },
            FakeRateControl {
                state: rate.clone(),
            },
            FakeStream {
                state: stream.clone(),
            },
            clock.clone(),
        );
        Self {
            supervisor,
            clock,
            probe,
            rate,
            stream,
            safety: SafetySupervisor::new(SafetyPolicy::default()),
        }
    }

    fn poll(&mut self) -> SupervisorStatus {
        self.supervisor.poll(&mut self.safety)
    }

    fn advance(&self, delta_ms: u64) {
        self.clock.advance(delta_ms);
    }

    /// 反复 poll 直到进入目标状态（只用于成功路径）。
    fn reach(&mut self, target: LifecycleState) -> SupervisorStatus {
        if self.supervisor.state() == target {
            return self.supervisor.status();
        }
        for _ in 0..64 {
            let status = self.poll();
            if status.state == target {
                return status;
            }
        }
        panic!(
            "did not reach {:?}; last status {:?}",
            target,
            self.supervisor.status()
        );
    }

    fn reach_streaming(&mut self) -> SupervisorStatus {
        self.reach(LifecycleState::Streaming)
    }

    fn assert_unmuted(&self) {
        assert!(!self.safety.requires_mute(), "safety must not mute");
    }
}

/// 临时目录（模拟 sysfs/proc 结构；Drop 清理）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-host-supervisor-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---- 状态机测试 -----------------------------------------------------------

#[test]
fn initial_absent_device_reports_absent_without_fault() {
    let mut h = Harness::new(
        "sup-initial-absent",
        48_000,
        SupervisorPolicy::default(),
        false,
    );
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Absent);
    assert_eq!(status.plan_rate, 48_000);
    assert_eq!(status.device_rate, None);
    assert!(!status.rate_asserted);
    assert!(status.last_error.is_none());
    assert_eq!(status.attempts, 0);
    assert_eq!(status.since_ms, 0);
    h.assert_unmuted();
    assert_eq!(
        h.safety.status().total_faults,
        0,
        "设备本来就不在 == 不报警"
    );
}

#[test]
fn full_lifecycle_absent_to_streaming() {
    let mut h = Harness::new(
        "sup-full-lifecycle",
        48_000,
        SupervisorPolicy::default(),
        false,
    );
    h.probe.set_present(true);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Enumerated);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);

    // 写速率成功，但稳定性还没攒够（默认 stable_reads = 2）。
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(status.device_rate, Some(48_000));
    assert!(!status.rate_asserted);

    // 第 2 次连续回读一致 -> RateAsserted。
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::RateAsserted);
    assert!(status.rate_asserted);
    assert_eq!(status.device_rate, Some(48_000));
    assert_eq!(status.attempts, 0);
    assert!(status.last_error.is_none());

    // 按方案速率开流 -> Streaming。
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Streaming);
    assert_eq!(status.plan_rate, 48_000);
    assert_eq!(h.stream.open_calls(), 1);
    assert_eq!(h.stream.last_rate(), Some(48_000));
    h.assert_unmuted();
}

#[test]
fn stable_reads_gates_rate_asserted() {
    let policy = SupervisorPolicy {
        stable_reads: 3,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-stable-reads", 48_000, policy, true);

    h.reach(LifecycleState::ControlReady);
    assert_eq!(h.rate.assert_calls(), 0, "ControlReady 还没写速率");

    let status = h.poll();
    assert_eq!(h.rate.assert_calls(), 1);
    assert_eq!(status.state, LifecycleState::ControlReady, "1/3 stable");
    assert!(!status.rate_asserted);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady, "2/3 stable");

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::RateAsserted, "3/3 stable");
    assert!(status.rate_asserted);
}

#[test]
fn control_channel_retries_after_backoff_then_succeeds() {
    let policy = SupervisorPolicy {
        retry_limit: 3,
        retry_interval_ms: 250,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-control-retry", 48_000, policy, true);
    h.probe.fail_control(2);

    assert_eq!(h.poll().state, LifecycleState::Enumerated);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Enumerated);
    assert_eq!(status.attempts, 1);
    assert_eq!(
        status.last_error.as_ref().map(|err| err.code),
        Some(SupervisorErrorCode::ControlChannelUnavailable)
    );
    assert_eq!(
        status.last_error.as_ref().map(|err| err.fallback),
        Some(LifecycleState::Enumerated)
    );
    assert_eq!(h.probe.control_calls(), 1);
    h.assert_unmuted();

    // 退避期没到：不调用探针、不涨 attempts。
    let status = h.poll();
    assert_eq!(status.attempts, 1);
    assert_eq!(h.probe.control_calls(), 1, "退避期内不得重试");

    h.advance(250);
    let status = h.poll();
    assert_eq!(status.attempts, 2);
    assert_eq!(h.probe.control_calls(), 2);
    h.assert_unmuted();

    h.advance(250);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(status.attempts, 0);
    assert!(status.last_error.is_none());
}

#[test]
fn control_exhaustion_latches_safe_mute_then_request_retry_recovers() {
    let policy = SupervisorPolicy {
        retry_limit: 1,
        retry_interval_ms: 250,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-control-latch", 48_000, policy, true);
    h.probe.fail_control(2);

    assert_eq!(h.poll().state, LifecycleState::Enumerated);
    assert_eq!(h.poll().attempts, 1);
    h.advance(250);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Enumerated);
    assert_eq!(status.attempts, 2, "1 次首试 + 1 次重试用尽");
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::ControlChannelUnavailable);
    assert_eq!(error.fallback, LifecycleState::Enumerated);
    assert!(h.safety.requires_mute(), "重试耗尽必须安全静音");
    let fault = h.safety.status().last_fault.expect("fault record");
    assert_eq!(fault.reason, FaultReason::ProcessorFault);
    assert!(fault.message.contains("control_channel_unavailable"));

    // 锁存：不再自动尝试。
    let calls = h.probe.control_calls();
    let status = h.poll();
    assert_eq!(status.attempts, 2);
    assert_eq!(h.probe.control_calls(), calls, "锁存后不得继续重试");

    // 显式请求重试 + 控制通道恢复 -> 链路走通后清故障。
    h.supervisor.request_retry();
    assert_eq!(h.poll().state, LifecycleState::ControlReady);
    h.reach_streaming();
    assert!(
        !h.safety.requires_mute(),
        "重新 Streaming 后必须清除设备故障"
    );
    assert!(h.supervisor.last_error().is_none());
}

#[test]
fn rate_assert_failure_retries_then_succeeds() {
    let policy = SupervisorPolicy {
        retry_limit: 3,
        retry_interval_ms: 250,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-assert-retry", 48_000, policy, true);
    h.reach(LifecycleState::ControlReady);
    h.rate.fail_assert(1);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(status.attempts, 1);
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::RateAssertFailed);
    assert_eq!(error.fallback, LifecycleState::ControlReady);
    h.assert_unmuted();

    h.advance(250);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(
        status.attempts, 1,
        "写入成功但还没走完稳定判定，失败计数保留"
    );
    assert_eq!(status.device_rate, Some(48_000));

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::RateAsserted);
    assert_eq!(status.attempts, 0);
    assert!(status.last_error.is_none());
}

#[test]
fn assert_rate_returning_wrong_rate_is_rate_assert_failed() {
    let policy = SupervisorPolicy {
        retry_limit: 0,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-assert-wrong-rate", 48_000, policy, true);
    h.reach(LifecycleState::ControlReady);
    h.rate.set_assert_override(Some(44_100));

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(status.device_rate, Some(44_100));
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::RateAssertFailed);
    assert_eq!(error.fallback, LifecycleState::ControlReady);
    assert!(h.safety.requires_mute(), "retry_limit=0 首次失败即锁存");
}

#[test]
fn rate_readback_mismatch_retries_then_safe_mutes() {
    let policy = SupervisorPolicy {
        retry_limit: 2,
        retry_interval_ms: 250,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-rate-mismatch", 48_000, policy, true);
    h.reach(LifecycleState::ControlReady);
    h.rate.set_read_override(Some(192_000));

    for round in 0..3u32 {
        if round > 0 {
            h.advance(250);
        }
        // 写入成功（返回方案速率），但回读不一致。
        assert_eq!(h.poll().state, LifecycleState::ControlReady);
        let status = h.poll();
        assert_eq!(status.state, LifecycleState::ControlReady);
        if round < 2 {
            assert_eq!(status.attempts, round + 1);
            assert!(!h.safety.requires_mute(), "重试预算内先不静音");
        }
    }

    let status = h.supervisor.status();
    assert_eq!(status.attempts, 3, "1 次首试 + 2 次重试用尽");
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::RateMismatch);
    assert_eq!(error.fallback, LifecycleState::ControlReady);
    assert_eq!(status.device_rate, Some(192_000));
    assert!(
        h.safety.requires_mute(),
        "速率回读不一致且重试耗尽必须安全静音"
    );
    assert_eq!(
        h.safety.status().last_fault.expect("fault").reason,
        FaultReason::ProcessorFault
    );

    // 锁存后不再读写速率。
    let reads = h.rate.read_calls();
    h.poll();
    assert_eq!(h.rate.read_calls(), reads, "锁存后不得继续重试速率");
}

#[test]
fn device_disappear_then_replug_recovers() {
    let device_id = "sup-replug";
    let mut h = Harness::new(device_id, 48_000, SupervisorPolicy::default(), true);
    h.reach_streaming();
    assert_eq!(h.stream.open_calls(), 1);

    // 拔掉：Streaming -> Absent，清状态 + 安全静音 + 关流 + 释放 owner。
    h.probe.set_present(false);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Absent);
    assert_eq!(status.device_rate, None);
    assert!(!status.rate_asserted);
    assert_eq!(status.attempts, 0);
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::DeviceAbsent);
    assert_eq!(error.fallback, LifecycleState::Absent);
    assert!(h.safety.requires_mute(), "流中断必须 fail-closed");
    assert_eq!(h.stream.close_calls(), 1);
    assert_eq!(h.safety.status().total_faults, 1);

    // 继续 Absent：不重复计故障。
    h.poll();
    assert_eq!(h.safety.status().total_faults, 1);

    // owner 已释放（Absent 不占设备）。
    DeviceGuard::acquire(device_id).expect("owner released while absent");

    // 插回：出现边沿解除锁存，整条链路重跑（重新枚举 -> 重新断言 -> 重新开流）。
    h.probe.set_present(true);
    let status = h.reach_streaming();
    assert_eq!(status.state, LifecycleState::Streaming);
    assert_eq!(status.device_rate, Some(48_000));
    assert!(status.rate_asserted);
    assert!(status.last_error.is_none());
    assert!(!h.safety.requires_mute(), "设备恢复后清安全静音");
    assert_eq!(h.stream.open_calls(), 2);
    assert_eq!(h.rate.assert_calls(), 2, "重新枚举必须重新断言速率");
    assert!(matches!(
        DeviceGuard::acquire(device_id),
        Err(OwnerError::AlreadyOwned(_))
    ));
}

#[test]
fn stream_open_retry_then_succeeds() {
    let policy = SupervisorPolicy {
        retry_limit: 3,
        retry_interval_ms: 250,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-stream-retry", 48_000, policy, true);
    h.reach(LifecycleState::RateAsserted);
    h.stream.fail_open(1);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::RateAsserted);
    assert_eq!(status.attempts, 1);
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::StreamOpenFailed);
    assert_eq!(error.fallback, LifecycleState::RateAsserted);
    h.assert_unmuted();

    // 退避期内不重试。
    h.poll();
    assert_eq!(h.stream.open_calls(), 1);

    h.advance(250);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Streaming);
    assert_eq!(h.stream.open_calls(), 2);
    assert!(status.last_error.is_none());
    h.assert_unmuted();
}

#[test]
fn stream_open_exhaustion_latches_safe_mute() {
    let policy = SupervisorPolicy {
        retry_limit: 0,
        ..SupervisorPolicy::default()
    };
    let mut h = Harness::new("sup-stream-latch", 48_000, policy, true);
    h.reach(LifecycleState::RateAsserted);
    h.stream.fail_open(1);

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::RateAsserted);
    assert_eq!(status.attempts, 1);
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::StreamOpenFailed);
    assert_eq!(error.fallback, LifecycleState::RateAsserted);
    assert!(h.safety.requires_mute());

    // 锁存：不再尝试开流。
    let calls = h.stream.open_calls();
    h.poll();
    assert_eq!(h.stream.open_calls(), calls);
}

#[test]
fn single_device_owner_conflict_is_reported_then_resolved() {
    let device_id = "sup-owner-conflict";
    let mut h = Harness::new(device_id, 48_000, SupervisorPolicy::default(), true);
    let external = DeviceGuard::acquire(device_id).expect("external owner");

    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Absent);
    assert_eq!(status.attempts, 1);
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::DeviceBusy);
    assert_eq!(error.fallback, LifecycleState::Absent);
    h.assert_unmuted();

    drop(external);
    h.advance(250);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::Enumerated);
    assert_eq!(status.attempts, 0);
    assert!(status.last_error.is_none());
    // 枚举成功后监督器持有 owner。
    assert!(matches!(
        DeviceGuard::acquire(device_id),
        Err(OwnerError::AlreadyOwned(_))
    ));
}

#[test]
fn status_snapshot_fields_track_transitions() {
    let mut h = Harness::at(
        "sup-status-fields",
        96_000,
        SupervisorPolicy::default(),
        false,
        1_000,
    );
    let status = h.supervisor.status();
    assert_eq!(status.state, LifecycleState::Absent);
    assert_eq!(status.plan_rate, 96_000);
    assert_eq!(status.device_rate, None);
    assert!(!status.rate_asserted);
    assert!(status.last_error.is_none());
    assert_eq!(status.attempts, 0);
    assert_eq!(status.since_ms, 1_000);

    h.advance(500);
    h.probe.set_present(true);
    assert_eq!(h.poll().since_ms, 1_500, "状态时间戳来自注入时钟");

    h.advance(100);
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert_eq!(status.since_ms, 1_600);

    let status = h.reach_streaming();
    assert_eq!(status.plan_rate, 96_000);
    assert_eq!(status.device_rate, Some(96_000));
    assert!(status.rate_asserted);
    assert!(status.last_error.is_none());
    assert_eq!(status.attempts, 0);
    assert_eq!(h.rate.device_rate(), 96_000);
}

#[test]
fn streaming_rate_drift_mutes_then_recovers() {
    let mut h = Harness::new(
        "sup-streaming-drift",
        48_000,
        SupervisorPolicy::default(),
        true,
    );
    h.reach_streaming();

    // 流式期间周期性回读：间隔未到不读。
    let reads = h.rate.read_calls();
    h.poll();
    assert_eq!(h.rate.read_calls(), reads, "回读间隔未到");

    h.advance(1_000);
    h.rate.set_read_override(Some(96_000));
    let status = h.poll();
    assert_eq!(status.state, LifecycleState::ControlReady);
    assert!(!status.rate_asserted);
    assert_eq!(status.device_rate, Some(96_000));
    let error = status.last_error.as_ref().expect("last_error");
    assert_eq!(error.code, SupervisorErrorCode::RateMismatch);
    assert_eq!(error.fallback, LifecycleState::ControlReady);
    assert!(h.safety.requires_mute(), "速率漂移必须立刻 fail-closed");
    assert_eq!(h.stream.close_calls(), 1, "漂移时先关流");

    // 速率问题解除：退回 ControlReady 后自动重断言并恢复到 Streaming。
    h.rate.set_read_override(None);
    let status = h.reach_streaming();
    assert_eq!(status.state, LifecycleState::Streaming);
    assert!(status.rate_asserted);
    assert!(status.last_error.is_none());
    assert!(!h.safety.requires_mute(), "恢复后清设备故障");
    assert_eq!(h.stream.open_calls(), 2);
}

#[test]
fn new_constructor_uses_null_stream_and_monotonic_clock() {
    let probe_state = ProbeState::default();
    probe_state.set_present(true);
    let mut supervisor = DeviceSupervisor::new(
        48_000,
        SupervisorPolicy::default(),
        FakeProbe {
            device_id: DeviceId::new("sup-new-constructor"),
            state: probe_state,
        },
        FakeRateControl {
            state: RateState::default(),
        },
    );
    let mut safety = SafetySupervisor::new(SafetyPolicy::default());
    let mut reached = None;
    for _ in 0..8 {
        let state = supervisor.poll(&mut safety).state;
        if state == LifecycleState::Streaming {
            reached = Some(state);
            break;
        }
    }
    assert_eq!(reached, Some(LifecycleState::Streaming));
    assert!(!safety.requires_mute());
}

// ---- Linux 参考实现测试（普通 fs / 命令错误映射，Windows 可跑） -----------

#[test]
fn sysfs_probe_reads_fake_sysfs_and_proc() {
    let tmp = TempDir::new("sysfs");
    let sysfs = tmp.path().join("sys").join("class").join("sound");
    let proc_asound = tmp.path().join("proc").join("asound");
    std::fs::create_dir_all(sysfs.join("card1")).expect("card dir");
    std::fs::write(sysfs.join("card1").join("id"), "UltraLite\n").expect("id");

    let proc_node = proc_asound
        .join("card1")
        .join("pcm0p")
        .join("sub0")
        .join("hw_params");
    let mut probe = SysfsProbe::new("UltraLite")
        .with_roots(&sysfs, &proc_asound)
        .with_card_match("ultralite");
    assert!(probe.is_present(), "id 文件匹配（大小写不敏感）");
    assert_eq!(probe.device_id().as_str(), "UltraLite");
    assert!(probe.bring_up_control().is_err(), "proc 控制节点缺失");

    std::fs::create_dir_all(proc_node.parent().expect("parent")).expect("proc dirs");
    std::fs::write(&proc_node, "closed\n").expect("hw_params");
    probe
        .bring_up_control()
        .expect("控制节点可读即 ControlReady");

    // 控制通道命令缺失：spawn 失败 -> ProbeError（不假成功）。
    let mut command_probe = SysfsProbe::new("UltraLite")
        .with_roots(&sysfs, &proc_asound)
        .with_control_command(vec![String::from("khaen-supervisor-no-such-command")]);
    assert!(command_probe.bring_up_control().is_err());

    std::fs::remove_file(sysfs.join("card1").join("id")).expect("remove id");
    assert!(!probe.is_present());
    assert!(
        probe.bring_up_control().is_err(),
        "设备不在时控制通道也不可用"
    );
}

#[test]
fn command_rate_control_maps_spawn_errors() {
    let mut control = CommandRateControl::new(
        vec![String::from("khaen-supervisor-missing-read")],
        vec![String::from("khaen-supervisor-missing-assert")],
    );
    assert!(control.read_rate().is_err());
    assert!(control.assert_rate(48_000).is_err());
}
