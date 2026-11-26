//! 运行时状态快照：块线程发布，控制线程只读（附 age），不阻塞块线程。
//!
//! 快照按 KHAEN_STATS_EVERY_BLOCKS 周期发布；控制命令处理完、设备生命周期变化时也立即发布。
//! KHAEN_STATS_EVERY_BLOCKS=0 时不做周期发布（只在命令/设备状态变化时发布），status 的
//! age_ms 会如实增长——不编造"看起来新鲜"的时间戳。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use khaen_host::engine::EngineConfig;
use khaen_host::runtime::RuntimeStatus;

use crate::recovery::DeviceLifecycle;

/// 控制线程与块线程共享的快照槽。
pub type SharedSnapshot = Arc<Mutex<Option<RuntimeSnapshot>>>;

/// 一份运行时状态快照（非实时路径构造：块循环在统计周期调用 Runtime::status() 克隆）。
#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    /// Runtime::status() 的完整快照（安全状态、块计数、输入/输出/收包/设备统计、图信息）。
    pub status: RuntimeStatus,
    /// 安全监督器累计故障数（RuntimeStatus 没有这个字段，单独带上）。
    pub faults: u64,
    /// 设备生命周期快照（T42o：absent/absent_since/reopen_attempts/last_device_error）。
    pub lifecycle: DeviceLifecycle,
    /// 数据面执行器格式（进程生命周期内固定；format op 不会改它）。
    pub format: EngineConfig,
    /// 快照采集时刻（单调时钟；控制线程用它算 age_ms）。
    pub captured_at: Instant,
    /// 周期发布间隔（毫秒；None = 不做周期发布，age 不设"过期"上限）。
    pub publish_period_ms: Option<u64>,
}

impl RuntimeSnapshot {
    /// 快照年龄（毫秒）：控制线程读快照时现算，不阻塞块线程。
    pub fn age_ms(&self) -> u64 {
        u64::try_from(self.captured_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// 快照是否已经超过合理的周期（2 个发布周期 + 500 ms，下限 1 s）。
    ///
    /// 只对"有周期发布"的装配有意义；不做周期发布时没有可比基准，恒为 false
    /// （status 里照样给 age_ms，让人自己判断，不假装新鲜）。
    pub fn stale(&self) -> bool {
        match self.publish_period_ms {
            Some(period) => {
                let tolerance = period.saturating_mul(2).saturating_add(500).max(1_000);
                self.age_ms() > tolerance
            }
            None => false,
        }
    }
}

/// 周期发布间隔（毫秒）：publish_every 块 * 块长 / 采样率。
///
/// publish_every == 0（KHAEN_STATS_EVERY_BLOCKS=0）或非法格式时返回 None。
pub fn publish_period_ms(publish_every: u64, format: &EngineConfig) -> Option<u64> {
    if publish_every == 0 || format.sample_rate == 0 {
        return None;
    }
    let frames =
        publish_every.saturating_mul(u64::try_from(format.block_frames).unwrap_or(u64::MAX));
    Some(frames.saturating_mul(1_000) / u64::from(format.sample_rate))
}
