//! 块线程侧的控制通道桥：轮询命令、执行块边界动作、发布状态快照。
//!
//! drive_loop 每轮循环（即每个块边界前）调用 [ControlBridge::poll_commands]：
//! - 用 try_recv 取走控制线程排队的命令（不阻塞块循环）；
//! - 在块线程上执行（Runtime 的唯一 owner），用一次性 ack 回执给控制线程；
//! - 处理完命令后立刻发布一次状态快照（status 马上能看到 pending_switch 等真实状态）。
//!
//! 周期快照按 KHAEN_STATS_EVERY_BLOCKS 发布；设备生命周期变化/命令后由 drive_loop 显式
//! 调用 publish。没有控制通道（bridge disabled）时所有方法都是空操作，块循环行为不变。

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Instant;

use crate::recovery::DeviceLifecycle;

use super::command::{execute_command, CommandContext, ControlCommand, ControlTarget};
use super::reconfigure::{shared_tracker, SharedReconfigure};
use super::snapshot::{publish_period_ms, RuntimeSnapshot, SharedSnapshot};

/// 控制通道在块线程侧的桥。
pub struct ControlBridge {
    /// 命令来源；None = 没有控制通道（或对端已全部断开）。
    commands: Option<Receiver<ControlCommand>>,
    /// 状态快照槽；None = 不对外提供 status。
    snapshot: Option<SharedSnapshot>,
    /// 周期发布间隔（块）；0 = 只按命令/生命周期变化发布。
    publish_every: u64,
    /// 上一次发布时的块号。
    published_blocks: u64,
    /// 上一次发布时采用的周期（毫秒；随数据面格式算一次）。
    publish_period_ms: Option<u64>,
    /// 热重配追踪器（块线程执行 channels_commit 时写；控制线程/状态文件读）。
    reconfigure: SharedReconfigure,
}

impl ControlBridge {
    /// 没有控制通道：块循环里全部是空操作（既有 drive_loop 行为不变）。
    pub fn disabled() -> Self {
        Self {
            commands: None,
            snapshot: None,
            publish_every: 0,
            published_blocks: 0,
            publish_period_ms: None,
            reconfigure: shared_tracker(),
        }
    }

    /// 接上控制通道：commands 来自控制服务端，snapshot 由块线程发布。
    pub fn new(
        commands: Receiver<ControlCommand>,
        snapshot: SharedSnapshot,
        publish_every: u64,
    ) -> Self {
        Self {
            commands: Some(commands),
            snapshot: Some(snapshot),
            publish_every,
            published_blocks: 0,
            publish_period_ms: None,
            reconfigure: shared_tracker(),
        }
    }

    /// 用共享的热重配追踪器（与 ControlCore/状态文件同一份；main 装配用）。
    pub fn with_reconfigure(mut self, reconfigure: SharedReconfigure) -> Self {
        self.reconfigure = reconfigure;
        self
    }

    /// 追踪器句柄（测试/诊断用）。
    pub fn reconfigure(&self) -> SharedReconfigure {
        Arc::clone(&self.reconfigure)
    }

    /// 是否接上了控制通道。
    pub fn enabled(&self) -> bool {
        self.commands.is_some()
    }

    /// 取出并执行所有已排队的块边界命令；返回处理条数（块线程调用）。
    ///
    /// `lifecycle` 可变：热重配失败/设备 absent 时要如实登记缺失（退避重开由监督循环
    /// 接着做）；`now_ms` 是监督循环的单调毫秒时钟（与 DeviceLifecycle 同口径）。
    pub fn poll_commands<T: ControlTarget>(
        &mut self,
        target: &mut T,
        lifecycle: &mut DeviceLifecycle,
        now_ms: u64,
    ) -> usize {
        let mut handled = 0usize;
        let mut disconnected = false;
        {
            let Some(commands) = self.commands.as_ref() else {
                return 0;
            };
            loop {
                match commands.try_recv() {
                    Ok(command) => {
                        handled += 1;
                        let mut ctx = CommandContext {
                            lifecycle,
                            reconfigure: &self.reconfigure,
                            now_ms,
                        };
                        execute_command(command, target, &mut ctx);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.commands = None;
        }
        if handled > 0 {
            self.publish(target, lifecycle);
        }
        handled
    }

    /// 立即发布一份状态快照（命令后/设备生命周期变化时调用）。
    pub fn publish<T: ControlTarget>(&mut self, target: &T, lifecycle: &DeviceLifecycle) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let format = target.data_plane_format();
        let period = publish_period_ms(self.publish_every, &format);
        self.publish_period_ms = period;
        self.published_blocks = target.blocks();
        let value = capture_snapshot(target, lifecycle, period);
        let mut guard = snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = Some(value);
    }

    /// 到点才发布（drive_loop 每块调用；publish_every == 0 时不做事）。
    pub fn maybe_publish<T: ControlTarget>(&mut self, target: &T, lifecycle: &DeviceLifecycle) {
        if self.snapshot.is_none() || self.publish_every == 0 {
            return;
        }
        let blocks = target.blocks();
        if blocks >= self.published_blocks.saturating_add(self.publish_every) {
            self.publish(target, lifecycle);
        }
    }
}

/// 采集一份快照（Runtime::status() 克隆 + 设备生命周期 + 格式 + 采集时刻）。
pub fn capture_snapshot<T: ControlTarget>(
    target: &T,
    lifecycle: &DeviceLifecycle,
    publish_period_ms: Option<u64>,
) -> RuntimeSnapshot {
    RuntimeSnapshot {
        status: target.status(),
        faults: target.total_faults(),
        lifecycle: lifecycle.clone(),
        format: target.data_plane_format(),
        captured_at: Instant::now(),
        publish_period_ms,
    }
}
