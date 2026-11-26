//! 注入用的假 MOTU CueMix 控制面（T57）：**不碰真实网络/设备**。
//!
//! 实现 `khaen_daemon::motu::MotuControl`，可编排：
//! - 检测结果（ready / not_detected(reason) / unreachable / 任意 [DeviceDetection]）；
//! - 设备当前状态（mute / 两通道 fader）与读写失败（回读不一致、超时等设备错误码）；
//! - 慢操作（拉宽并发窗口，用来验证 daemon 的并发语义）；
//! - 调用记录：写序列、写次数、检测/读次数、最大并发操作数。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use khaen_daemon::motu::{
    self, device_codes, DeviceDetection, MotuControl, MotuError, MotuReading, WriteReceipt,
};

/// 一次写入记录（用于断言"单键写"顺序与次数，以及 prepare 不写设备）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WriteCall {
    /// kMuteEnable[0] 写入。
    Mute(bool),
    /// koBusFader[channel_index] 写入（channel_index 0 = Main 1）。
    Volume { channel_index: u16, linear: f64 },
}

/// 注入的一次失败：错误 + 设备实际留下的值（模拟"写入被设备接受但回读不一致"）。
#[derive(Clone, Debug)]
pub struct InjectedFailure<T> {
    /// 返回给 daemon 的错误。
    pub error: MotuError,
    /// 失败后设备上的实际值（None = 设备没变）。
    pub applied: Option<T>,
}

impl<T> InjectedFailure<T> {
    /// 只报错，设备状态不变（e.g. 回读超时/未收到回读）。
    pub fn error(error: MotuError) -> Self {
        Self {
            error,
            applied: None,
        }
    }

    /// 报错，但设备实际留下了另一个值（e.g. 回读不一致）。
    pub fn applied(error: MotuError, applied: T) -> Self {
        Self {
            error,
            applied: Some(applied),
        }
    }
}

/// 假设备状态。
#[derive(Debug)]
struct FakeDevice {
    mute: bool,
    faders: Vec<f64>,
    sample_rate_hz: u32,
}

impl Default for FakeDevice {
    fn default() -> Self {
        Self {
            mute: false,
            faders: vec![1.0, 1.0],
            sample_rate_hz: 48_000,
        }
    }
}

/// 可编排的假 MOTU 控制面。
#[derive(Debug)]
pub struct FakeCueMix {
    detection: Mutex<DeviceDetection>,
    device: Mutex<FakeDevice>,
    writes: Mutex<Vec<WriteCall>>,
    fail_mute: Mutex<Option<InjectedFailure<bool>>>,
    fail_volume: Mutex<Vec<(u16, InjectedFailure<f64>)>>,
    fail_read: Mutex<Option<MotuError>>,
    slow: Mutex<Option<Duration>>,
    detection_calls: AtomicUsize,
    read_calls: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

/// 检测通过的 MOTU 设备。
pub fn ready_detection(model: &str) -> DeviceDetection {
    DeviceDetection {
        detected: true,
        card_id: Some("UltraLitemk5".to_string()),
        card_index: Some(0),
        model: Some(model.to_string()),
        control_reachable: true,
        reason: motu::REASON_DETECTED,
    }
}

/// 没有检测到设备（默认 `cards_unavailable`，Windows/无设备场景）。
pub fn absent_detection(reason: &'static str) -> DeviceDetection {
    DeviceDetection {
        detected: false,
        card_id: None,
        card_index: None,
        model: None,
        control_reachable: false,
        reason,
    }
}

impl FakeCueMix {
    /// 检测通过、控制网可达、mute=false、fader=1.0（unity）。
    pub fn ready() -> Arc<Self> {
        Self::with_detection(ready_detection("UltraLite-mk5"))
    }

    /// 检测不到 MOTU（cards_unavailable）。
    pub fn not_detected() -> Arc<Self> {
        Self::with_detection(absent_detection(motu::REASON_CARDS_UNAVAILABLE))
    }

    /// 没有 MOTU 卡（no_motu_card）。
    pub fn no_motu_card() -> Arc<Self> {
        Self::with_detection(absent_detection(motu::REASON_NO_MOTU_CARD))
    }

    /// 卡在但控制网不通。
    pub fn unreachable() -> Arc<Self> {
        let mut detection = ready_detection("UltraLite-mk5");
        detection.control_reachable = false;
        detection.reason = motu::REASON_CONTROL_UNREACHABLE;
        Self::with_detection(detection)
    }

    /// 用任意检测结果构造。
    pub fn with_detection(detection: DeviceDetection) -> Arc<Self> {
        Arc::new(Self {
            detection: Mutex::new(detection),
            device: Mutex::new(FakeDevice::default()),
            writes: Mutex::new(Vec::new()),
            fail_mute: Mutex::new(None),
            fail_volume: Mutex::new(Vec::new()),
            fail_read: Mutex::new(None),
            slow: Mutex::new(None),
            detection_calls: AtomicUsize::new(0),
            read_calls: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        })
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn begin(&self) {
        let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(current, Ordering::SeqCst);
    }

    fn end(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn delay(&self) {
        let slow = *Self::lock(&self.slow);
        if let Some(duration) = slow {
            if !duration.is_zero() {
                std::thread::sleep(duration);
            }
        }
    }

    /// 改检测结果（模拟设备热插拔/控制网掉线）。
    pub fn set_detection(&self, detection: DeviceDetection) {
        *Self::lock(&self.detection) = detection;
    }

    /// 设置设备 mute 当前值（GET 现状断言用）。
    pub fn set_mute_state(&self, mute: bool) {
        Self::lock(&self.device).mute = mute;
    }

    /// 设置两通道 fader 当前值。
    pub fn set_faders(&self, faders: [f64; 2]) {
        Self::lock(&self.device).faders = faders.to_vec();
    }

    /// 当前设备 mute。
    pub fn mute_state(&self) -> bool {
        Self::lock(&self.device).mute
    }

    /// 当前设备某通道 fader（0 = Main 1）。
    pub fn fader(&self, channel_index: u16) -> f64 {
        Self::lock(&self.device).faders[channel_index as usize]
    }

    /// 已发生的写入序列（顺序即 daemon 发出的单键写顺序）。
    pub fn writes(&self) -> Vec<WriteCall> {
        Self::lock(&self.writes).clone()
    }

    /// 写次数。
    pub fn write_count(&self) -> usize {
        Self::lock(&self.writes).len()
    }

    /// 检测被调用次数。
    pub fn detection_calls(&self) -> usize {
        self.detection_calls.load(Ordering::SeqCst)
    }

    /// 设备读被调用次数。
    pub fn read_calls(&self) -> usize {
        self.read_calls.load(Ordering::SeqCst)
    }

    /// 观察到的最大并发操作数（>=2 说明测试确实制造了并发）。
    pub fn max_concurrent(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// 下一次 mute 写入失败。
    pub fn fail_mute(&self, failure: InjectedFailure<bool>) {
        *Self::lock(&self.fail_mute) = Some(failure);
    }

    /// 指定通道的下一次音量写入失败（按 channel_index 匹配）。
    pub fn fail_volume_on(&self, channel_index: u16, failure: InjectedFailure<f64>) {
        Self::lock(&self.fail_volume).push((channel_index, failure));
    }

    /// 下一次设备读失败。
    pub fn fail_read(&self, error: MotuError) {
        *Self::lock(&self.fail_read) = Some(error);
    }

    /// 让每次操作前 sleep 一段时间（拉宽并发窗口）。
    pub fn set_slow(&self, duration: Duration) {
        *Self::lock(&self.slow) = Some(duration);
    }

    fn not_ready_error(detection: &DeviceDetection) -> MotuError {
        MotuError::new(
            device_codes::NOT_DETECTED,
            format!("fake MOTU is not ready: {}", detection.reason),
        )
    }
}

impl MotuControl for FakeCueMix {
    fn detection(&self) -> DeviceDetection {
        self.begin();
        self.detection_calls.fetch_add(1, Ordering::SeqCst);
        self.delay();
        let detection = Self::lock(&self.detection).clone();
        self.end();
        detection
    }

    fn read(&self) -> Result<MotuReading, MotuError> {
        self.begin();
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        self.delay();
        let detection = Self::lock(&self.detection).clone();
        if let Some(error) = Self::lock(&self.fail_read).take() {
            self.end();
            return Err(error);
        }
        if !detection.is_ready() {
            self.end();
            return Err(Self::not_ready_error(&detection));
        }
        let reading = {
            let device = Self::lock(&self.device);
            MotuReading {
                model_id: Some("UL5L".to_string()),
                device_name: detection.model.clone(),
                version: Some("1.0.0".to_string()),
                api_version: Some(10),
                mute: Some(device.mute),
                sample_rate_hz: Some(device.sample_rate_hz),
                main_faders: device.faders.iter().map(|value| Some(*value)).collect(),
                main_trim: Some(31),
                main_bus_mute: Some(false),
                missing: Vec::new(),
            }
        };
        self.end();
        Ok(reading)
    }

    fn write_mute(&self, mute: bool) -> Result<WriteReceipt<bool>, MotuError> {
        self.begin();
        self.delay();
        let detection = Self::lock(&self.detection).clone();
        if !detection.is_ready() {
            self.end();
            return Err(Self::not_ready_error(&detection));
        }
        let failure = Self::lock(&self.fail_mute).take();
        Self::lock(&self.writes).push(WriteCall::Mute(mute));
        let before = Self::lock(&self.device).mute;
        let receipt = {
            let mut device = Self::lock(&self.device);
            if let Some(failure) = &failure {
                if let Some(applied) = failure.applied {
                    device.mute = applied;
                }
            } else {
                device.mute = mute;
            }
            device.mute
        };
        self.end();
        if let Some(failure) = failure {
            return Err(failure.error);
        }
        Ok(WriteReceipt {
            key: "kMuteEnable",
            index: 0,
            before: Some(before),
            wrote: mute,
            readback: receipt,
            changed: before != mute,
            frame_sent: before != mute,
        })
    }

    fn write_volume(
        &self,
        channel_index: u16,
        linear: f64,
    ) -> Result<WriteReceipt<f64>, MotuError> {
        self.begin();
        self.delay();
        let detection = Self::lock(&self.detection).clone();
        if !detection.is_ready() {
            self.end();
            return Err(Self::not_ready_error(&detection));
        }
        let failure = {
            let mut failures = Self::lock(&self.fail_volume);
            failures
                .iter()
                .position(|(index, _)| *index == channel_index)
                .map(|position| failures.remove(position).1)
        };
        Self::lock(&self.writes).push(WriteCall::Volume {
            channel_index,
            linear,
        });
        let before = Self::lock(&self.device).faders[channel_index as usize];
        let readback = {
            let mut device = Self::lock(&self.device);
            match &failure {
                Some(failure) => {
                    if let Some(applied) = failure.applied {
                        device.faders[channel_index as usize] = applied;
                    }
                    device.faders[channel_index as usize]
                }
                None => {
                    device.faders[channel_index as usize] = linear;
                    linear
                }
            }
        };
        self.end();
        if let Some(failure) = failure {
            return Err(failure.error);
        }
        Ok(WriteReceipt {
            key: "koBusFader",
            index: channel_index,
            before: Some(before),
            wrote: linear,
            readback,
            changed: before != linear,
            frame_sent: before != linear,
        })
    }
}
