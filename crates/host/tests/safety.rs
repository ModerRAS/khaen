//! 故障隔离测试：last-good 保留、安全静音、欠载/溢出阈值、显式恢复。

use khaen_host::safety::{
    FaultReason, RecoveryError, RuntimeState, SafetyPolicy, SafetySupervisor,
};

fn supervisor() -> SafetySupervisor {
    let mut supervisor = SafetySupervisor::new(SafetyPolicy::default());
    supervisor.start();
    supervisor
}

#[test]
fn successful_commit_advances_last_good() {
    let mut supervisor = supervisor();
    supervisor.note_commit_applied(7);
    let status = supervisor.status();
    assert_eq!(status.state, RuntimeState::Running);
    assert_eq!(status.active_version, Some(7));
    assert_eq!(status.last_good_version, Some(7));
    assert!(!supervisor.requires_mute());
}

#[test]
fn failed_commit_keeps_last_good_and_safe_mutes() {
    let mut supervisor = supervisor();
    supervisor.note_commit_applied(7);
    supervisor.note_commit_failed("version 8 admission rejected");
    let status = supervisor.status();
    assert_eq!(status.state, RuntimeState::Faulted);
    assert!(supervisor.requires_mute());
    assert_eq!(status.active_version, Some(7), "失败版本不得变成现役");
    assert_eq!(status.last_good_version, Some(7));
    assert_eq!(
        status.last_fault.as_ref().unwrap().reason,
        FaultReason::CommitFailed
    );
    assert_eq!(status.total_faults, 1);
}

#[test]
fn consecutive_underruns_trigger_safe_mute() {
    let mut supervisor = supervisor();
    assert!(!supervisor.note_underrun());
    assert!(!supervisor.note_underrun());
    assert!(!supervisor.requires_mute(), "偶发欠载不该静音");
    assert!(supervisor.note_underrun(), "第 3 次连续欠载必须进安全静音");
    assert_eq!(supervisor.state(), RuntimeState::Faulted);
    assert!(supervisor.requires_mute());
    let status = supervisor.status();
    assert_eq!(status.consecutive_underruns, 3);
    assert_eq!(
        status.last_fault.as_ref().unwrap().reason,
        FaultReason::Underrun
    );
}

#[test]
fn a_good_block_resets_the_underrun_counter() {
    let mut supervisor = supervisor();
    assert!(!supervisor.note_underrun());
    assert!(!supervisor.note_underrun());
    supervisor.note_block_ok();
    assert!(!supervisor.note_underrun(), "计数清零后重新计数");
    assert_eq!(supervisor.status().consecutive_underruns, 1);
    assert_eq!(supervisor.state(), RuntimeState::Running);
}

#[test]
fn consecutive_overruns_trigger_safe_mute() {
    let mut supervisor = supervisor();
    assert!(!supervisor.note_overrun());
    assert!(!supervisor.note_overrun());
    assert!(supervisor.note_overrun());
    assert_eq!(supervisor.state(), RuntimeState::Faulted);
    assert_eq!(
        supervisor.status().last_fault.as_ref().unwrap().reason,
        FaultReason::Overrun
    );
}

#[test]
fn processor_fault_is_immediate() {
    let mut supervisor = supervisor();
    supervisor.note_processor_fault("stage returned error");
    assert_eq!(supervisor.state(), RuntimeState::Faulted);
    assert!(supervisor.requires_mute());
    assert_eq!(
        supervisor.status().last_fault.as_ref().unwrap().message,
        "stage returned error"
    );
}

#[test]
fn recovery_must_be_explicit() {
    let mut supervisor = supervisor();
    assert_eq!(supervisor.clear_fault(), Err(RecoveryError::NotFaulted));
    supervisor.note_processor_fault("boom");
    supervisor.clear_fault().expect("clear");
    assert_eq!(supervisor.state(), RuntimeState::Running);
    assert!(!supervisor.requires_mute());
    assert_eq!(supervisor.status().total_faults, 1, "历史故障数不清零");
}

#[test]
fn user_mute_is_independent_of_fault_state() {
    let mut supervisor = supervisor();
    supervisor.set_user_mute(true);
    assert_eq!(supervisor.state(), RuntimeState::Muted);
    assert!(supervisor.requires_mute());

    supervisor.clear_fault().expect_err("muted is not faulted");

    supervisor.note_processor_fault("boom");
    supervisor.clear_fault().expect("clear");
    assert_eq!(
        supervisor.state(),
        RuntimeState::Muted,
        "用户静音仍然生效，恢复后回到 muted"
    );

    supervisor.set_user_mute(false);
    assert_eq!(supervisor.state(), RuntimeState::Running);
    assert!(!supervisor.requires_mute());
}
