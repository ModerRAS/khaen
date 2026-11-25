//! 媒体时钟接口（[NONRT] 接口 + RT 安全的读数实现）。
//!
//! AES67 媒体面时间基准 = PTP（IEEE 1588-2008），SDP `a=ts-refclk` 声明。
//! 本原型把"媒体时间"抽象成 `MediaClock` trait：
//!
//! | 实现 | 用途 | 状态 |
//! |---|---|---|
//! | `UsrvClock` | 用户态单调时基（`Instant` 进程级原点）；离线 loopback/RT 路径 | 已验证（离线） |
//! | `MockClock` | 单测/确定性回放：手动推进 ns | 已验证 |
//! | `PtpPhcClock` | 真实 `/dev/ptp0` PHC 读数（ioctl PTP_CLOCK_GETTIME） | **NOT_WIRED**（板端接） |
//!
//! 读时钟是 `&self` + 原子/单调调用，**不分配、不阻塞**；RT 线程每块读一次。
//! 板端接法（后续 T67/T74）：打开 `/dev/ptp0`，用 `PTP_CLOCK_GETTIME` ioctl 读
//! `phc_ns`，再换算成流时间轴；PTP servo/锁状态归 T19，本 trait 只消费时间值。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// 默认 PTP 设备路径（板端 `/dev/ptp0`）。
pub const PTP_DEVICE_DEFAULT: &str = "/dev/ptp0";
/// 每秒纳秒数。
pub const NS_PER_SEC: u64 = 1_000_000_000;

/// 媒体时钟读数接口（RT 安全：不分配、不阻塞、可跨线程 `&self` 调用）。
pub trait MediaClock: Send + Sync + std::fmt::Debug {
    /// 当前媒体时间（ns，单调；原点由实现定义，接收端只使用差值）。
    fn now_ns(&self) -> u64;

    /// 媒体采样率（AES67 48k）。
    fn rate_hz(&self) -> u32;

    /// 实现名（证据/日志用；非 RT 格式化调用）。
    fn name(&self) -> &'static str;
}

/// ns → 帧（向下取整；`delta_ns` 为相对原点差值）。
#[inline]
pub fn ns_to_frames(delta_ns: u64, rate_hz: u32) -> u64 {
    (delta_ns as u128 * rate_hz as u128 / NS_PER_SEC as u128) as u64
}

/// 帧 → ns（向下取整）。
#[inline]
pub fn frames_to_ns(frames: u64, rate_hz: u32) -> u64 {
    (frames as u128 * NS_PER_SEC as u128 / rate_hz as u128) as u64
}

/// 帧 → ns（向上取整；保证 `ns_to_frames(frames_to_ns_ceil(f)) == f`）。
#[inline]
pub fn frames_to_ns_ceil(frames: u64, rate_hz: u32) -> u64 {
    if rate_hz == 0 {
        return 0;
    }
    (frames as u128 * NS_PER_SEC as u128).div_ceil(rate_hz as u128) as u64
}

/// 进程级单调时基原点（`UsrvClock` 各实例共享同一原点，保证 IO/RT 线程可比）。
static USRV_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// 用户态单调时钟（`Instant`）：离线/非 PTP 环境的媒体时钟替身。
///
/// 注意：`Instant` 是主机单调时钟，不是 PTP 时间。离线 loopback 只需"两个线程
/// 同一时间轴"，因此成立；真实接收端必须把 RTP 时间戳映射到 PTP/PHC（板端接），
/// 否则无法与发送端/其他接收端保持采样级相位一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsrvClock {
    rate_hz: u32,
}

impl UsrvClock {
    /// 以指定媒体速率构造（AES67 必须 48000）。
    pub const fn new(rate_hz: u32) -> Self {
        Self { rate_hz }
    }

    /// 进程级原点（测试/证据用；两个线程读到同一原点）。
    pub fn origin() -> Instant {
        *USRV_ORIGIN.get_or_init(Instant::now)
    }
}

impl MediaClock for UsrvClock {
    fn now_ns(&self) -> u64 {
        let origin = Self::origin();
        origin.elapsed().as_nanos() as u64
    }

    fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    fn name(&self) -> &'static str {
        "usrvclock"
    }
}

/// 手动推进的确定性时钟（单测/离线回放；RT 路径不构造）。
#[derive(Debug)]
pub struct MockClock {
    now_ns: AtomicU64,
    rate_hz: u32,
}

impl MockClock {
    /// 从 0 开始。
    pub const fn new(rate_hz: u32) -> Self {
        Self {
            now_ns: AtomicU64::new(0),
            rate_hz,
        }
    }

    /// 直接设置时间（ns）。
    pub fn set_ns(&self, value: u64) {
        self.now_ns.store(value, Ordering::Release);
    }

    /// 推进 ns。
    pub fn advance_ns(&self, delta_ns: u64) {
        self.now_ns.fetch_add(delta_ns, Ordering::AcqRel);
    }

    /// 推进若干采样帧对应的时间。
    pub fn advance_frames(&self, frames: u64) {
        self.advance_ns(frames_to_ns(frames, self.rate_hz));
    }
}

impl MediaClock for MockClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::Acquire)
    }

    fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    fn name(&self) -> &'static str {
        "mockclock"
    }
}

/// PTP/PHC 时钟错误（Copy；稳定错误名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtpClockError {
    /// 原型未接真实 PHC（离线/板端接线前恒为此值）。
    NotWired,
    /// `/dev/ptp0` 不存在/无权限。
    DeviceUnavailable,
    /// ioctl 读失败。
    Io,
    /// 时间戳非法。
    InvalidTimestamp,
}

impl PtpClockError {
    /// 稳定错误名。
    pub const fn name(self) -> &'static str {
        match self {
            PtpClockError::NotWired => "ERR_PTP_NOT_WIRED",
            PtpClockError::DeviceUnavailable => "ERR_PTP_DEVICE_UNAVAILABLE",
            PtpClockError::Io => "ERR_PTP_IO",
            PtpClockError::InvalidTimestamp => "ERR_PTP_INVALID_TIMESTAMP",
        }
    }
}

/// 真实 PTP 硬件时钟（`/dev/ptp0` PHC）接口占位。
///
/// **NOT_WIRED**：离线原型不打开设备、不做 ioctl；`read_ns` 恒返回
/// `PtpClockError::NotWired`。板端接线步骤（T67/T74 前置）：
/// 1. `open("/dev/ptp0", O_RDONLY)`（需要 CAP_SYS_TIME 或 udev 权限）；
/// 2. `ioctl(fd, PTP_CLOCK_GETTIME, &PtpClockTime{ ... })`（内核 4.1+）或
///    `clock_gettime(CLOCKFD, &ts)`；
/// 3. 把 PHC ns 映射到流时间轴（与 `a=ts-refclk` domain 绑定）；
/// 4. PTP 锁状态由 T19 servo 提供（LockState::Locked 才允许开始采集）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtpPhcClock {
    device: &'static str,
    rate_hz: u32,
}

impl PtpPhcClock {
    /// 离线占位构造（不打开设备）。
    pub const fn not_wired(rate_hz: u32) -> Self {
        Self {
            device: PTP_DEVICE_DEFAULT,
            rate_hz,
        }
    }

    /// 指定设备路径的占位构造（仍不打开）。
    pub const fn with_device(device: &'static str, rate_hz: u32) -> Self {
        Self { device, rate_hz }
    }

    /// 设备路径。
    pub const fn device(&self) -> &'static str {
        self.device
    }

    /// 读 PHC 时间（板端接；当前恒 `NotWired`）。
    pub const fn read_ns(&self) -> Result<u64, PtpClockError> {
        Err(PtpClockError::NotWired)
    }

    /// 媒体速率。
    pub const fn media_rate_hz(&self) -> u32 {
        self.rate_hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_mock_advances_and_shared_origin_usrvclock_is_monotonic() {
        let mock = MockClock::new(48_000);
        assert_eq!(mock.now_ns(), 0);
        mock.advance_frames(128);
        let ns = mock.now_ns();
        assert!(ns > 2_600_000 && ns < 2_800_000, "128 帧 @48k ≈ 2.667ms");
        assert_eq!(mock.rate_hz(), 48_000);
        assert_eq!(mock.name(), "mockclock");

        let a = UsrvClock::new(48_000);
        let b = UsrvClock::new(48_000);
        let first = a.now_ns();
        let second = b.now_ns();
        assert!(second >= first);
        assert_eq!(UsrvClock::origin(), UsrvClock::origin());
    }

    #[test]
    fn frame_ns_conversions() {
        assert_eq!(frames_to_ns(48_000, 48_000), NS_PER_SEC);
        assert_eq!(frames_to_ns(128, 48_000), 2_666_666);
        assert_eq!(ns_to_frames(NS_PER_SEC, 48_000), 48_000);
        // 向下取整的双向换算可能少 1 帧；向上取整版本必须精确可逆。
        assert_eq!(ns_to_frames(frames_to_ns(128, 48_000), 48_000), 127);
        assert_eq!(ns_to_frames(frames_to_ns_ceil(128, 48_000), 48_000), 128);
        assert_eq!(
            ns_to_frames(frames_to_ns_ceil(48_000, 48_000), 48_000),
            48_000
        );
        // 边界：1ns 不足 1 帧。
        assert_eq!(ns_to_frames(1, 48_000), 0);
        // 大时间不溢出（u128 中间值）。
        let huge = ns_to_frames(u64::MAX / 2, 48_000);
        assert_eq!(
            huge,
            (u64::MAX as u128 / 2 * 48_000 / NS_PER_SEC as u128) as u64
        );
    }

    #[test]
    fn ptp_placeholder_is_not_wired_and_named() {
        let ptp = PtpPhcClock::not_wired(48_000);
        assert_eq!(ptp.device(), PTP_DEVICE_DEFAULT);
        assert_eq!(ptp.read_ns().unwrap_err(), PtpClockError::NotWired);
        assert_eq!(ptp.read_ns().unwrap_err().name(), "ERR_PTP_NOT_WIRED");
        assert_eq!(ptp.media_rate_hz(), 48_000);
        assert_eq!(
            PtpPhcClock::with_device("/dev/ptp1", 48_000).device(),
            "/dev/ptp1"
        );
    }
}
