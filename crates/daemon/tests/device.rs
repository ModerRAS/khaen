//! 设备监督器接线测试：用假 probe / 速率控制 / 流 / 时钟驱动真实生命周期状态机，
//! 再用 `DaemonState::poll_device` 验证“锁存 → 引擎静音”的 fail-closed 行为。
//! 真实 sysfs/ALSA/CueMix 路径在 Windows 本机为 NOT_RUN（CI 的 Linux job 覆盖平台相关代码）。

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use khaen_daemon::{DaemonState, DeviceMonitor};
use khaen_host::owner::DeviceId;
use khaen_host::supervisor::{
    ClockSource, DeviceProbe, ProbeError, RateControl, StreamControl, SupervisorPolicy,
};
use khaen_host::EngineConfig;
use serde_json::Value;

const PLAN_RATE: u32 = 48_000;

/// 假组件的共享状态句柄。
struct FakeHandles {
    present: Arc<AtomicBool>,
    bring_up_ok: Arc<AtomicBool>,
    bring_ups: Arc<AtomicU32>,
    device_rate: Arc<AtomicU32>,
    rate_reads: Arc<AtomicU32>,
    rate_writes: Arc<AtomicU32>,
    stream_opens: Arc<AtomicU32>,
    now_ms: Arc<AtomicU64>,
}

struct FakeProbe {
    id: DeviceId,
    present: Arc<AtomicBool>,
    bring_up_ok: Arc<AtomicBool>,
    bring_ups: Arc<AtomicU32>,
}

impl DeviceProbe for FakeProbe {
    fn device_id(&self) -> &DeviceId {
        &self.id
    }

    fn is_present(&mut self) -> bool {
        self.present.load(Ordering::SeqCst)
    }

    fn bring_up_control(&mut self) -> Result<(), ProbeError> {
        self.bring_ups.fetch_add(1, Ordering::SeqCst);
        if self.bring_up_ok.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ProbeError::new("fake control channel refused"))
        }
    }
}

struct FakeRate {
    device_rate: Arc<AtomicU32>,
    reads: Arc<AtomicU32>,
    writes: Arc<AtomicU32>,
}

impl RateControl for FakeRate {
    fn read_rate(&mut self) -> Result<u32, ProbeError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.device_rate.load(Ordering::SeqCst))
    }

    fn assert_rate(&mut self, rate: u32) -> Result<u32, ProbeError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.device_rate.store(rate, Ordering::SeqCst);
        Ok(rate)
    }
}

struct FakeStream {
    opens: Arc<AtomicU32>,
}

impl StreamControl for FakeStream {
    fn open_stream(&mut self, _rate: u32) -> Result<(), ProbeError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn close_stream(&mut self) -> Result<(), ProbeError> {
        Ok(())
    }
}

struct FakeClock {
    now_ms: Arc<AtomicU64>,
}

impl ClockSource for FakeClock {
    fn now_ms(&mut self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

fn monitor() -> (DeviceMonitor, FakeHandles) {
    // 单设备所有权（DeviceGuard）是进程级全局状态：并行测试必须用不同设备标识，
    // 否则会互相撞成 DeviceBusy。
    static NEXT_DEVICE: AtomicU32 = AtomicU32::new(0);
    let device_id = format!("fake-motu-{}", NEXT_DEVICE.fetch_add(1, Ordering::SeqCst));
    let handles = FakeHandles {
        present: Arc::new(AtomicBool::new(true)),
        bring_up_ok: Arc::new(AtomicBool::new(true)),
        bring_ups: Arc::new(AtomicU32::new(0)),
        device_rate: Arc::new(AtomicU32::new(PLAN_RATE)),
        rate_reads: Arc::new(AtomicU32::new(0)),
        rate_writes: Arc::new(AtomicU32::new(0)),
        stream_opens: Arc::new(AtomicU32::new(0)),
        now_ms: Arc::new(AtomicU64::new(0)),
    };
    let probe = FakeProbe {
        id: DeviceId::new(device_id),
        present: Arc::clone(&handles.present),
        bring_up_ok: Arc::clone(&handles.bring_up_ok),
        bring_ups: Arc::clone(&handles.bring_ups),
    };
    let rate = FakeRate {
        device_rate: Arc::clone(&handles.device_rate),
        reads: Arc::clone(&handles.rate_reads),
        writes: Arc::clone(&handles.rate_writes),
    };
    let stream = FakeStream {
        opens: Arc::clone(&handles.stream_opens),
    };
    let clock = FakeClock {
        now_ms: Arc::clone(&handles.now_ms),
    };
    let monitor = DeviceMonitor::with_components(
        PLAN_RATE,
        SupervisorPolicy::default(),
        Box::new(probe),
        Box::new(rate),
        Box::new(stream),
        Box::new(clock),
    );
    (monitor, handles)
}

/// 推进假时钟并轮询直到目标状态出现。
fn poll_until(
    monitor: &mut DeviceMonitor,
    handles: &FakeHandles,
    state: &str,
    max: usize,
) -> Value {
    for _ in 0..max {
        handles.now_ms.fetch_add(1_000, Ordering::SeqCst);
        let snapshot = monitor.poll();
        if snapshot["state"] == Value::String(state.to_string()) {
            return snapshot;
        }
    }
    panic!(
        "state {state} not reached in {max} polls; last = {}",
        monitor.status()
    );
}

#[test]
fn disabled_monitor_reports_not_wired_with_reason() {
    let mut monitor = DeviceMonitor::disabled("no device configured");
    assert!(!monitor.is_wired());
    assert!(!monitor.requires_mute());
    assert_eq!(monitor.plan_rate(), None);
    assert_eq!(monitor.state(), None);
    let status = monitor.status();
    assert_eq!(status["wired"], Value::Bool(false));
    assert_eq!(
        status["reason"],
        Value::String("no device configured".into())
    );
    assert_eq!(monitor.poll()["wired"], Value::Bool(false));
}

#[test]
fn lifecycle_reaches_streaming_and_reports_rate_honestly() {
    let (mut monitor, handles) = monitor();
    // 初始（尚未轮询）就是 absent，速率还没读过。
    assert_eq!(monitor.state().map(|s| s.as_str()), Some("absent"));
    let initial = monitor.status();
    assert_eq!(initial["wired"], Value::Bool(true));
    assert_eq!(initial["device_rate"], Value::Null);
    assert_eq!(initial["rate_asserted"], Value::Bool(false));

    let streaming = poll_until(&mut monitor, &handles, "streaming", 40);
    assert_eq!(streaming["plan_rate"], Value::from(PLAN_RATE));
    assert_eq!(streaming["device_rate"], Value::from(PLAN_RATE));
    assert_eq!(streaming["rate_asserted"], Value::Bool(true));
    assert_eq!(streaming["last_error"], Value::Null);
    assert!(handles.rate_writes.load(Ordering::SeqCst) >= 1);
    assert!(handles.stream_opens.load(Ordering::SeqCst) >= 1);
    assert!(!monitor.requires_mute());
}

#[test]
fn unplug_latches_safety_mute_and_replug_recovers() {
    let (mut monitor, handles) = monitor();
    poll_until(&mut monitor, &handles, "streaming", 40);

    handles.present.store(false, Ordering::SeqCst);
    let snapshot = poll_until(&mut monitor, &handles, "absent", 10);
    assert_eq!(snapshot["safety"]["requires_mute"], Value::Bool(true));
    // 设备不在时速率未知：如实给 null，不沿用“最后一次读到的值”冒充现状。
    assert_eq!(snapshot["device_rate"], Value::Null);
    assert_eq!(snapshot["rate_asserted"], Value::Bool(false));
    assert!(monitor.requires_mute());

    handles.present.store(true, Ordering::SeqCst);
    let recovered = poll_until(&mut monitor, &handles, "streaming", 40);
    assert_eq!(recovered["safety"]["requires_mute"], Value::Bool(false));
    assert!(!monitor.requires_mute());
}

#[test]
fn control_channel_failure_is_reported_not_hidden() {
    let (mut monitor, handles) = monitor();
    handles.bring_up_ok.store(false, Ordering::SeqCst);
    let mut snapshot = monitor.poll();
    for _ in 0..10 {
        handles.now_ms.fetch_add(1_000, Ordering::SeqCst);
        snapshot = monitor.poll();
        if snapshot["last_error"].is_object() {
            break;
        }
    }
    let error = snapshot["last_error"]
        .as_object()
        .expect("last_error object");
    assert!(error["code"].as_str().is_some_and(|code| !code.is_empty()));
    assert!(error["message"]
        .as_str()
        .is_some_and(|message| message.contains("fake control channel refused")));
    assert_ne!(snapshot["state"], Value::String("streaming".into()));
    assert_eq!(snapshot["device_rate"], Value::Null);
}

#[tokio::test(flavor = "current_thread")]
async fn safety_latch_mutes_engine_through_daemon_poll() {
    let (mut monitor, handles) = monitor();
    poll_until(&mut monitor, &handles, "streaming", 40);
    handles.present.store(false, Ordering::SeqCst);
    poll_until(&mut monitor, &handles, "absent", 10);
    assert!(monitor.requires_mute());

    let config = EngineConfig {
        block_frames: 128,
        sample_rate: PLAN_RATE,
        channels: 2,
    };
    let state = DaemonState::new(config)
        .expect("state")
        .with_device(monitor);
    assert_eq!(state.control().status_json()["mute"], Value::Bool(false));

    // 静音在块边界生效：静音请求后必须真的跑块，状态里的 mute 才反映实际输出。
    let run_block = |state: &DaemonState| {
        let input = vec![0.0f32; 128 * 2];
        {
            let mut control = state.control();
            control.engine_mut().run_block(&input).expect("block");
        }
    };
    state.poll_device();
    run_block(&state);
    assert_eq!(state.control().status_json()["mute"], Value::Bool(true));

    // 用户显式解除静音后，锁存仍然存在的话下一次轮询必须重新静音（fail-closed）。
    state.control().mute_json(false);
    run_block(&state);
    assert_eq!(state.control().status_json()["mute"], Value::Bool(false));
    state.poll_device();
    run_block(&state);
    assert_eq!(state.control().status_json()["mute"], Value::Bool(true));
}

#[tokio::test(flavor = "current_thread")]
async fn daemon_status_carries_device_snapshot() {
    let config = EngineConfig::default();
    let plain = DaemonState::new(config).expect("state");
    let status = plain.control().status_json();
    assert!(status.get("device").is_none()); // control 层不掺设备字段
    let device = plain.device_json();
    assert_eq!(device["wired"], Value::Bool(false));
    assert!(device["reason"].as_str().is_some_and(|r| !r.is_empty()));
}
