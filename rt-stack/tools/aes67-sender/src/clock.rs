//! Local monotonic clock (QPC on Windows) + high-precision deadline pacing.
//!
//! IMPORTANT (clock sync): this sender has no PTP/PHC access. RTP timestamps are
//! generated from this local monotonic clock only. That is NOT strict AES67
//! PTP media-clock synchronization; see evidence/aes67-tx-win/CLOCK-AND-SYNC.md.
//!
//! Pacing approach (Windows):
//!   * QPC (QueryPerformanceCounter) is the time base (sub-microsecond resolution).
//!   * A 1 ms system timer resolution is requested via winmm timeBeginPeriod(1);
//!     sleep() alone is too coarse (default ~15.6 ms) without it.
//!   * Final ~400 us before each deadline is spent spinning on QPC, then a
//!     `std::thread::yield_now` fallback keeps the core usable.
//! Measured lateness is reported in the sender stats JSON (deadline stats).

use std::time::Duration;

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(uperiod: u32) -> u32;
    fn timeEndPeriod(uperiod: u32) -> u32;
}

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, GetCurrentThread, GetThreadPriority, SetThreadPriority,
    SetWaitableTimer, WaitForSingleObject, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE,
    THREAD_PRIORITY, THREAD_PRIORITY_HIGHEST, TIMER_ALL_ACCESS,
};

pub struct MonoClock {
    #[cfg(windows)]
    freq: i64,
    /// High-resolution waitable timer (Win10 1803+). Sleeps precisely between
    /// packets instead of spinning the CPU hot for the whole ptime.
    #[cfg(windows)]
    timer: Option<WaitableTimer>,
    #[cfg(not(windows))]
    origin: std::time::Instant,
}

impl MonoClock {
    pub fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            let mut freq = 0i64;
            unsafe {
                QueryPerformanceFrequency(&mut freq)
                    .map_err(|e| anyhow::anyhow!("QueryPerformanceFrequency failed: {e}"))?;
            }
            anyhow::ensure!(freq > 0, "invalid QPC frequency {freq}");
            let timer = WaitableTimer::new_high_res().ok();
            Ok(Self { freq, timer })
        }
        #[cfg(not(windows))]
        {
            Ok(Self {
                origin: std::time::Instant::now(),
            })
        }
    }

    /// Monotonic nanoseconds since an arbitrary epoch.
    pub fn now_ns(&self) -> u64 {
        #[cfg(windows)]
        {
            let mut ticks = 0i64;
            unsafe {
                let _ = QueryPerformanceCounter(&mut ticks);
            }
            ((ticks as u128) * 1_000_000_000u128 / (self.freq as u128)) as u64
        }
        #[cfg(not(windows))]
        {
            self.origin.elapsed().as_nanos() as u64
        }
    }

    /// Sleep/spin until `target_ns` (absolute, same base as `now_ns`).
    pub fn sleep_until(&self, target_ns: u64) {
        #[cfg(windows)]
        if let Some(t) = &self.timer {
            t.wait_until(self, target_ns);
            return;
        }
        const SPIN_NS: u64 = 400_000;
        loop {
            let now = self.now_ns();
            if now >= target_ns {
                return;
            }
            let rem = target_ns - now;
            if rem > SPIN_NS * 2 {
                std::thread::sleep(Duration::from_nanos(rem - SPIN_NS));
            } else if rem > SPIN_NS {
                std::thread::sleep(Duration::from_nanos(rem - SPIN_NS));
            } else if rem > 20_000 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
    }
}

/// High-resolution waitable timer wrapper (Windows 10 1803+).
#[cfg(windows)]
pub struct WaitableTimer {
    handle: HANDLE,
}

#[cfg(windows)]
impl WaitableTimer {
    pub fn new_high_res() -> anyhow::Result<Self> {
        unsafe {
            let h = CreateWaitableTimerExW(
                None,
                windows::core::PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0 as u32,
            )
            .map_err(|e| anyhow::anyhow!("CreateWaitableTimerExW(high-res) failed: {e}"))?;
            Ok(Self { handle: h })
        }
    }

    /// Wait until `target_ns`, then spin the last ~150 us on QPC for accuracy.
    pub fn wait_until(&self, clock: &MonoClock, target_ns: u64) {
        const SPIN_NS: u64 = 150_000;
        loop {
            let now = clock.now_ns();
            if now >= target_ns {
                return;
            }
            let rem = target_ns - now;
            if rem > SPIN_NS * 2 {
                let due_100ns = -(((rem - SPIN_NS) / 100).max(1) as i64);
                unsafe {
                    if SetWaitableTimer(self.handle, &due_100ns, 0, None, None, false).is_err() {
                        return;
                    }
                    let rc = WaitForSingleObject(self.handle, INFINITE);
                    if rc.0 != 0 {
                        // WAIT_OBJECT_0 == 0; anything else => fall back to yield
                        std::thread::yield_now();
                    }
                }
            } else {
                std::hint::spin_loop();
            }
        }
    }
}

#[cfg(windows)]
impl Drop for WaitableTimer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Temporarily raise the current thread priority for the send loop.
pub struct ThreadPriorityGuard {
    #[cfg(windows)]
    previous: Option<i32>,
}

impl ThreadPriorityGuard {
    pub fn boost_highest() -> Self {
        #[cfg(windows)]
        {
            unsafe {
                let h = GetCurrentThread();
                let prev = GetThreadPriority(h);
                // 0x7FFFFFFF == THREAD_PRIORITY_ERROR_RETURN
                let previous = if prev != 0x7FFF_FFFF {
                    Some(prev)
                } else {
                    None
                };
                let _ = SetThreadPriority(h, THREAD_PRIORITY_HIGHEST);
                Self { previous }
            }
        }
        #[cfg(not(windows))]
        {
            Self {}
        }
    }
}

impl Drop for ThreadPriorityGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(prev) = self.previous {
            unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY(prev));
            }
        }
    }
}

/// RAII guard requesting 1 ms Windows timer resolution (no-op off Windows).
pub struct TimerResolutionGuard {
    active: bool,
}

impl TimerResolutionGuard {
    pub fn request_1ms() -> Self {
        #[cfg(windows)]
        {
            // timeBeginPeriod(1) is deprecated on Win10+ but remains the simple,
            // broadly compatible way to get ~1 ms Sleep granularity for a CLI tool.
            let rc = unsafe { timeBeginPeriod(1) };
            Self { active: rc == 0 }
        }
        #[cfg(not(windows))]
        {
            Self { active: false }
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.active {
            unsafe {
                timeEndPeriod(1);
            }
        }
    }
}

/// Simple fixed-capacity-ish latency accumulator for deadline stats.
#[derive(Debug, Default, Clone)]
pub struct LatenessStats {
    pub count: u64,
    pub late_count: u64,
    pub miss_count: u64,
    pub max_ns: u64,
    samples: Vec<u64>,
}

impl LatenessStats {
    pub fn new() -> Self {
        Self {
            samples: Vec::with_capacity(1 << 16),
            ..Default::default()
        }
    }

    /// `late_ns` = how much after the intended deadline the packet actually left.
    /// Late > 50 us counts as "late"; late > 1 ptime (> 1 ms here) counts as miss.
    pub fn observe(&mut self, late_ns: u64, period_ns: u64) {
        self.count += 1;
        if late_ns > 50_000 {
            self.late_count += 1;
        }
        if late_ns > period_ns {
            self.miss_count += 1;
        }
        if late_ns > self.max_ns {
            self.max_ns = late_ns;
        }
        if self.samples.len() < (1 << 16) {
            self.samples.push(late_ns);
        }
    }

    pub fn percentile_ns(&self, p: f64) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
        sorted[idx]
    }

    pub fn mean_ns(&self) -> u64 {
        if self.samples.is_empty() {
            return 0;
        }
        (self.samples.iter().sum::<u64>() / self.samples.len() as u64) as u64
    }
}

/// Convert a frame count at 48 kHz to nanoseconds (exact integer math).
pub fn frames_to_ns(frames: u64) -> u64 {
    (frames.saturating_mul(1_000_000_000) / 48_000) as u64
}

pub fn ms_to_ns(ms: f64) -> u64 {
    if ms <= 0.0 {
        0
    } else {
        (ms * 1_000_000.0).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_to_ns_is_exact_for_common_ptimes() {
        assert_eq!(frames_to_ns(48), 1_000_000);
        assert_eq!(frames_to_ns(6), 125_000);
        assert_eq!(frames_to_ns(192), 4_000_000);
        assert_eq!(frames_to_ns(480), 10_000_000);
        assert_eq!(frames_to_ns(48_000), 1_000_000_000);
    }

    #[test]
    fn clock_is_monotonic_and_sleep_until_waits() {
        let c = MonoClock::new().unwrap();
        let a = c.now_ns();
        let b = c.now_ns();
        assert!(b >= a, "QPC went backwards: {a} -> {b}");
        let target = c.now_ns() + 2_000_000;
        c.sleep_until(target);
        assert!(c.now_ns() >= target);
    }

    #[test]
    fn lateness_percentiles() {
        let mut s = LatenessStats::new();
        for v in [0u64, 10_000, 60_000, 1_500_000, 20_000] {
            s.observe(v, 1_000_000);
        }
        assert_eq!(s.count, 5);
        assert_eq!(s.late_count, 2);
        assert_eq!(s.miss_count, 1);
        assert_eq!(s.max_ns, 1_500_000);
        assert_eq!(s.percentile_ns(0.5), 20_000);
    }
}
