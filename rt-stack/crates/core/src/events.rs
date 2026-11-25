//! v1 事件类型、lane 与 severity 常量（core-v1 §8.1/§8.2）。
//!
//! T06 scaffold + T90 批次：固定 40 个 event kind 编号与导出名映射（v1 冻结 1–35；
//! core-v1 v1.1 追加 36–40，见 §8.2 与 §0.1）。编号是 v1 契约：不得改号；
//! 新增必须升 v1.1 并同时更新本模块常量与 event_name 映射（core-v1 §8.2 注）。
//! 本模块不是 RT 执行路径；RT 区只做序号递增 + 按值拷贝（§8.3），不格式化/不分配。

/// 事件 lane：DSP worker0。
pub const LANE_WORKER0: u8 = 0;
/// 事件 lane：DSP worker1。
pub const LANE_WORKER1: u8 = 1;
/// 事件 lane：DSP worker2。
pub const LANE_WORKER2: u8 = 2;
/// 事件 lane：DSP worker3（master）。
pub const LANE_WORKER3: u8 = 3;
/// 事件 lane：capture I/O。
pub const LANE_CAPTURE: u8 = 4;
/// 事件 lane：playback I/O。
pub const LANE_PLAYBACK: u8 = 5;
/// 事件 lane：watchdog。
pub const LANE_WATCHDOG: u8 = 6;
/// 事件 lane：control。
pub const LANE_CONTROL: u8 = 7;

/// severity：info。
pub const SEVERITY_INFO: u8 = 1;
/// severity：warning。
pub const SEVERITY_WARNING: u8 = 2;
/// severity：error。
pub const SEVERITY_ERROR: u8 = 3;
/// severity：fatal。
pub const SEVERITY_FATAL: u8 = 4;

/// kind 1：启动、解析配置、无音频 owner。
pub const EV_HOST_INIT: u16 = 1;
/// kind 2：admission 通过。
pub const EV_ADMISSION_OK: u16 = 2;
/// kind 3：admission 失败。
pub const EV_ADMISSION_FAIL: u16 = 3;
/// kind 4：plan 已构建/校验。
pub const EV_PLAN_PREPARED: u16 = 4;
/// kind 5：plan 资源准入通过。
pub const EV_PLAN_ADMITTED: u16 = 5;
/// kind 6：plan 写入 pending、switch_state=ARMED。
pub const EV_PLAN_STAGED: u16 = 6;
/// kind 7：plan 在 block 边界提交。
pub const EV_PLAN_SWAPPED: u16 = 7;
/// kind 8：4 worker swap all-ack。
pub const EV_SWAP_ACK: u16 = 8;
/// kind 9：旧 plan retire gate 通过并回收。
pub const EV_PLAN_RETIRED: u16 = 9;
/// kind 10：回滚到 last-good 版本。
pub const EV_ROLLBACK: u16 = 10;
/// kind 11：新 epoch 开始。
pub const EV_EPOCH_START: u16 = 11;
/// kind 12：epoch 结束。
pub const EV_EPOCH_END: u16 = 12;
/// kind 13：正常处理中。
pub const EV_RUNNING: u16 = 13;
/// kind 14：停止新提交并 drain。
pub const EV_DRAINING: u16 = 14;
/// kind 15：worker 迟到导致块无效。
pub const EV_BLOCK_LATE: u16 = 15;
/// kind 16：compute_ticks 超 gate（验收门，不停流）。
pub const EV_DEADLINE_MISS: u16 = 16;
/// kind 17：块 invalid。
pub const EV_BLOCK_INVALID: u16 = 17;
/// kind 18：capture ring 满导致丢块。
pub const EV_INPUT_OVERRUN: u16 = 18;
/// kind 19：playback ring 空导致补静音。
pub const EV_OUTPUT_UNDERRUN: u16 = 19;
/// kind 20：输入 nonfinite 已 sanitize。
pub const EV_INPUT_NONFINITE: u16 = 20;
/// kind 21：输出 clip 计数。
pub const EV_CLIP: u16 = 21;
/// kind 22：输出 nonfinite（fault 级）。
pub const EV_OUTPUT_NONFINITE: u16 = 22;
/// kind 23：进入安全静音。
pub const EV_SAFETY_MUTE_ENTER: u16 = 23;
/// kind 24：退出安全静音。
pub const EV_SAFETY_MUTE_EXIT: u16 = 24;
/// kind 25：进入保护 fallback 图。
pub const EV_FALLBACK_ENTER: u16 = 25;
/// kind 26：退出保护 fallback 图。
pub const EV_FALLBACK_EXIT: u16 = 26;
/// kind 27：块进入 quarantine。
pub const EV_QUARANTINE_ENTER: u16 = 27;
/// kind 28：quarantine 槽 all-ack 后释放。
pub const EV_QUARANTINE_RELEASE: u16 = 28;
/// kind 29：quarantine 池耗尽。
pub const EV_QUARANTINE_EXHAUSTED: u16 = 29;
/// kind 30：时钟 discontinuity（必须新 epoch）。
pub const EV_CLOCK_DISCONTINUITY: u16 = 30;
/// kind 31：设备 owner 变更。
pub const EV_DEVICE_OWNER_CHANGE: u16 = 31;
/// kind 32：worker 失联。
pub const EV_WORKER_LOST: u16 = 32;
/// kind 33：watchdog 请求安全 mute。
pub const EV_WATCHDOG_MUTE: u16 = 33;
/// kind 34：不可恢复 fault。
pub const EV_FAULT: u16 = 34;
/// kind 35：线程退出、ring 停、设备关闭。
pub const EV_STOPPED: u16 = 35;
/// kind 36（v1.1）：设备打开/协商/启动中（进入 Starting）。
pub const EV_STARTING: u16 = 36;
/// kind 37（v1.1）：plan stage/commit 被中止或未生效即失败。
pub const EV_PLAN_ABORTED: u16 = 37;
/// kind 38（v1.1）：epoch 准备失败/取消（EpochAborted，非正常 drain）。
pub const EV_EPOCH_ABORTED: u16 = 38;
/// kind 39（v1.1）：ack 槽耗尽（master 报告占用槽数/epoch）。
pub const EV_ACK_SLOT_EXHAUSTED: u16 = 39;
/// kind 40（v1.1）：crossfade 双图/双历史/峰值预算不足被拒。
pub const EV_CROSSFADE_BUDGET_REJECT: u16 = 40;

/// 导出映射：kind -> 规范名称（非 RT 侧导出/日志用；RT 内禁止调用）。
pub fn event_name(kind: u16) -> &'static str {
    match kind {
        EV_HOST_INIT => "EV_HOST_INIT",
        EV_ADMISSION_OK => "EV_ADMISSION_OK",
        EV_ADMISSION_FAIL => "EV_ADMISSION_FAIL",
        EV_PLAN_PREPARED => "EV_PLAN_PREPARED",
        EV_PLAN_ADMITTED => "EV_PLAN_ADMITTED",
        EV_PLAN_STAGED => "EV_PLAN_STAGED",
        EV_PLAN_SWAPPED => "EV_PLAN_SWAPPED",
        EV_SWAP_ACK => "EV_SWAP_ACK",
        EV_PLAN_RETIRED => "EV_PLAN_RETIRED",
        EV_ROLLBACK => "EV_ROLLBACK",
        EV_EPOCH_START => "EV_EPOCH_START",
        EV_EPOCH_END => "EV_EPOCH_END",
        EV_RUNNING => "EV_RUNNING",
        EV_DRAINING => "EV_DRAINING",
        EV_BLOCK_LATE => "EV_BLOCK_LATE",
        EV_DEADLINE_MISS => "EV_DEADLINE_MISS",
        EV_BLOCK_INVALID => "EV_BLOCK_INVALID",
        EV_INPUT_OVERRUN => "EV_INPUT_OVERRUN",
        EV_OUTPUT_UNDERRUN => "EV_OUTPUT_UNDERRUN",
        EV_INPUT_NONFINITE => "EV_INPUT_NONFINITE",
        EV_CLIP => "EV_CLIP",
        EV_OUTPUT_NONFINITE => "EV_OUTPUT_NONFINITE",
        EV_SAFETY_MUTE_ENTER => "EV_SAFETY_MUTE_ENTER",
        EV_SAFETY_MUTE_EXIT => "EV_SAFETY_MUTE_EXIT",
        EV_FALLBACK_ENTER => "EV_FALLBACK_ENTER",
        EV_FALLBACK_EXIT => "EV_FALLBACK_EXIT",
        EV_QUARANTINE_ENTER => "EV_QUARANTINE_ENTER",
        EV_QUARANTINE_RELEASE => "EV_QUARANTINE_RELEASE",
        EV_QUARANTINE_EXHAUSTED => "EV_QUARANTINE_EXHAUSTED",
        EV_CLOCK_DISCONTINUITY => "EV_CLOCK_DISCONTINUITY",
        EV_DEVICE_OWNER_CHANGE => "EV_DEVICE_OWNER_CHANGE",
        EV_WORKER_LOST => "EV_WORKER_LOST",
        EV_WATCHDOG_MUTE => "EV_WATCHDOG_MUTE",
        EV_FAULT => "EV_FAULT",
        EV_STOPPED => "EV_STOPPED",
        EV_STARTING => "EV_STARTING",
        EV_PLAN_ABORTED => "EV_PLAN_ABORTED",
        EV_EPOCH_ABORTED => "EV_EPOCH_ABORTED",
        EV_ACK_SLOT_EXHAUSTED => "EV_ACK_SLOT_EXHAUSTED",
        EV_CROSSFADE_BUDGET_REJECT => "EV_CROSSFADE_BUDGET_REJECT",
        _ => "EV_UNKNOWN",
    }
}

const _: () = {
    assert!(EV_HOST_INIT == 1);
    assert!(EV_STOPPED == 35);
    // v1.1 追加编号（core-v1 §8.2 表 36–40；T90 批次补齐常量/导出映射）。
    assert!(EV_STARTING == 36);
    assert!(EV_PLAN_ABORTED == 37);
    assert!(EV_EPOCH_ABORTED == 38);
    assert!(EV_ACK_SLOT_EXHAUSTED == 39);
    assert!(EV_CROSSFADE_BUDGET_REJECT == 40);
    assert!(LANE_WORKER0 == 0 && LANE_CONTROL == 7);
    assert!(SEVERITY_INFO == 1 && SEVERITY_FATAL == 4);
};
