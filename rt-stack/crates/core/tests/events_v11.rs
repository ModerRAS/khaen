//! T90：core-v1 v1.1 新增 event kind 36–40 的常量/导出映射测试（executed，非 RT）。
//!
//! 依据：spec/core-v1.md v1.1.1 §8.2 表（36–40 为 v1.1 追加；1–35 冻结不得改号）。
//! 归属：integrator T90 批次（crates/core/tests/events_v11.rs，integrator 写域）。

use khaen_core::events::{
    event_name, EV_ACK_SLOT_EXHAUSTED, EV_CROSSFADE_BUDGET_REJECT, EV_EPOCH_ABORTED,
    EV_PLAN_ABORTED, EV_STARTING, EV_STOPPED,
};

#[test]
fn events_v11_kinds_36_to_40_match_spec() {
    // 1–35 是 v1 冻结，35 仍是最后一条 v1 编号。
    assert_eq!(EV_STOPPED, 35);
    // v1.1 §8.2 追加 36–40（顺序/编号不得改）。
    assert_eq!(EV_STARTING, 36);
    assert_eq!(EV_PLAN_ABORTED, 37);
    assert_eq!(EV_EPOCH_ABORTED, 38);
    assert_eq!(EV_ACK_SLOT_EXHAUSTED, 39);
    assert_eq!(EV_CROSSFADE_BUDGET_REJECT, 40);
}

#[test]
fn events_v11_names_are_exported() {
    assert_eq!(event_name(EV_STARTING), "EV_STARTING");
    assert_eq!(event_name(EV_PLAN_ABORTED), "EV_PLAN_ABORTED");
    assert_eq!(event_name(EV_EPOCH_ABORTED), "EV_EPOCH_ABORTED");
    assert_eq!(event_name(EV_ACK_SLOT_EXHAUSTED), "EV_ACK_SLOT_EXHAUSTED");
    assert_eq!(
        event_name(EV_CROSSFADE_BUDGET_REJECT),
        "EV_CROSSFADE_BUDGET_REJECT"
    );
    // 未定义编号仍返回 EV_UNKNOWN（导出不 panic）。
    assert_eq!(event_name(41), "EV_UNKNOWN");
}
