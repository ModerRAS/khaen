//! T06 自证：rt-check forbid 的正/负 fixture（core-v1 §12.4 / §13.6）。
//!
//! 要求：正 fixture PASS；4 类负 fixture（Vec::new / Arc clone / drop(Box) / 返回 Vec 错误）
//! 必须 FAIL；全部为真实执行测试（executed test_count > 0）。

use std::path::PathBuf;

use tspi_rt_check::forbid;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ci/fixtures")
}

fn scan(rel: &str) -> tspi_rt_check::Report {
    forbid::scan_path(&fixtures().join(rel)).expect("scan fixture")
}

fn assert_rule(rel: &str, rule: &str) {
    let report = scan(rel);
    assert!(!report.ok(), "fixture {rel} 应当 FAIL，实际 PASS");
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.rule == rule),
        "fixture {rel} 未命中规则 {rule}：{:?}",
        report.violations
    );
}

#[test]
fn forbid_positive_fixture_passes() {
    let report = scan("forbid/positive");
    assert!(report.scanned >= 1);
    assert!(
        report.ok(),
        "positive fixture 不应有违规：{:?}",
        report.violations
    );
}

#[test]
fn forbid_negative_vec_alloc_detected() {
    assert_rule("forbid/negative/rt_vec_alloc.rs", "F-01");
}

#[test]
fn forbid_negative_arc_clone_detected() {
    assert_rule("forbid/negative/rt_arc_clone.rs", "F-03");
}

#[test]
fn forbid_negative_drop_box_detected() {
    assert_rule("forbid/negative/rt_drop_box.rs", "F-27");
}

#[test]
fn forbid_negative_vec_error_detected() {
    assert_rule("forbid/negative/rt_vec_error.rs", "F-25");
}
