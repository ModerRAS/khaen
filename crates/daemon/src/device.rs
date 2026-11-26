//! 设备监督器接线（T70c → daemon REST）。
//!
//! 设计要点：
//! - 监督器只能有一个 owner：daemon 进程持有它，音频线程通过块路径上的安全检查使用同样的
//!   `SafetySupervisor` 语义（这里的状态是控制面快照，不是 RT 路径的权威状态）。
//! - 状态上报必须真实：没有配置设备（没有 probe/速率控制命令）时 `wired = false` 并给出原因，
//!   绝不编造 `device_rate`。
//! - 安全联动 fail-closed：监督器锁存（latch）后 `requires_mute = true`，daemon 在轮询时把
//!   引擎静音；恢复必须由用户显式 `/api/mute false`（不允许自动解除）。

use std::fmt;

use serde_json::{json, Value};

use khaen_host::safety::{SafetyPolicy, SafetySupervisor};
use khaen_host::supervisor::{
    ClockSource, DeviceProbe, DeviceSupervisor, LifecycleState, RateControl, StreamControl,
    SupervisorPolicy,
};

/// 设备后端组合：trait 对象便于在 daemon 里按配置选择真实/假实现。
///
/// 组件必须 `Send`：监督器挂在 `Mutex` 里被 tokio handler 与轮询线程共享。
type WiredSupervisor = DeviceSupervisor<
    Box<dyn DeviceProbe + Send>,
    Box<dyn RateControl + Send>,
    Box<dyn StreamControl + Send>,
    Box<dyn ClockSource + Send>,
>;

struct Wired {
    supervisor: WiredSupervisor,
    safety: SafetySupervisor,
}

/// 设备监视器：持有一个可选的生命周期状态机，并缓存最近一次快照。
pub struct DeviceMonitor {
    wired: Option<Wired>,
    /// 最近一次 `poll` 的结果快照（`/api/device` 直接返回；`/api/status` 只在 rt-host
    /// 状态不可用时用它回退 `device.state`）。
    last: Value,
}

impl fmt::Debug for DeviceMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceMonitor")
            .field("wired", &self.wired.is_some())
            .finish()
    }
}

impl DeviceMonitor {
    /// 没有接线设备时使用：如实报 `wired=false` + 原因。
    pub fn disabled(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            wired: None,
            last: json!({ "wired": false, "reason": reason }),
        }
    }

    /// 用显式组件构造（正常接线与测试注入共用）。
    pub fn with_components(
        plan_rate: u32,
        policy: SupervisorPolicy,
        probe: Box<dyn DeviceProbe + Send>,
        rate: Box<dyn RateControl + Send>,
        stream: Box<dyn StreamControl + Send>,
        clock: Box<dyn ClockSource + Send>,
    ) -> Self {
        let supervisor =
            DeviceSupervisor::with_components(plan_rate, policy, probe, rate, stream, clock);
        let safety = SafetySupervisor::new(SafetyPolicy::default());
        let mut monitor = Self {
            wired: Some(Wired { supervisor, safety }),
            last: Value::Null,
        };
        monitor.last = monitor.build_status();
        monitor
    }

    /// 是否接了设备。
    pub fn is_wired(&self) -> bool {
        self.wired.is_some()
    }

    /// 方案速率（没接线为 `None`）。
    pub fn plan_rate(&self) -> Option<u32> {
        self.wired
            .as_ref()
            .map(|wired| wired.supervisor.plan_rate())
    }

    /// 当前生命周期状态（没接线为 `None`）。
    pub fn state(&self) -> Option<LifecycleState> {
        self.wired.as_ref().map(|wired| wired.supervisor.state())
    }

    /// 轮询一次状态机；返回最新快照。没接线时是空操作（返回既有快照）。
    pub fn poll(&mut self) -> Value {
        if let Some(wired) = self.wired.as_mut() {
            wired.supervisor.poll(&mut wired.safety);
            self.last = self.build_status();
        }
        self.last.clone()
    }

    /// 请求监督器跳过退避、立即重试一次。
    pub fn request_retry(&mut self) -> Value {
        if let Some(wired) = self.wired.as_mut() {
            wired.supervisor.request_retry();
            self.last = self.build_status();
        }
        self.last.clone()
    }

    /// 最近一次快照（不触发轮询）。
    pub fn status(&self) -> Value {
        self.last.clone()
    }

    /// 监督器是否要求静音（锁存后为 `true`；没接线恒为 `false`）。
    pub fn requires_mute(&self) -> bool {
        self.wired
            .as_ref()
            .is_some_and(|wired| wired.safety.requires_mute())
    }

    /// 清除安全故障（恢复路径；仅当当前状态不再要求静音时成功）。
    pub fn clear_fault(&mut self) -> bool {
        match self.wired.as_mut() {
            Some(wired) => wired.safety.clear_fault().is_ok(),
            None => false,
        }
    }

    fn build_status(&self) -> Value {
        let Some(wired) = self.wired.as_ref() else {
            return self.last.clone();
        };
        let status = wired.supervisor.status();
        let safety = wired.safety.status();
        json!({
            "wired": true,
            "state": status.state.as_str(),
            "plan_rate": status.plan_rate,
            "device_rate": status.device_rate,
            "rate_asserted": status.rate_asserted,
            "attempts": status.attempts,
            "since_ms": status.since_ms,
            "last_error": status.last_error.as_ref().map(|error| json!({
                "code": error.code.as_str(),
                "message": error.message,
                "fallback": error.fallback.as_str(),
            })),
            "safety": {
                "state": safety.state.as_str(),
                "requires_mute": safety.state.requires_mute(),
                "user_mute": safety.user_mute,
                "blocks": safety.blocks,
                "total_faults": safety.total_faults,
            },
        })
    }
}
