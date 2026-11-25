//! T06 自证：rt-check drop-audit 的类型清单与正/负 fixture（core-v1 §13.2/§13.6）。

use std::path::PathBuf;

use tspi_rt_check::drop_audit;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ci/fixtures")
}

fn scan(rel: &str) -> tspi_rt_check::Report {
    drop_audit::scan_path(&fixtures().join(rel)).expect("scan fixture")
}

#[test]
fn drop_audit_types_have_no_drop() {
    let report = drop_audit::audit_types();
    assert!(report.scanned >= 20, "类型清单过少：{}", report.scanned);
    assert!(report.ok(), "RT 可见类型出现 Drop：{:?}", report.violations);
}

#[test]
fn drop_audit_positive_fixture_passes() {
    let report = scan("drop-audit/positive");
    assert!(report.scanned >= 1);
    assert!(
        report.ok(),
        "positive fixture 不应有违规：{:?}",
        report.violations
    );
}

#[test]
fn drop_audit_negative_impl_drop_detected() {
    let report = scan("drop-audit/negative/rt_impl_drop.rs");
    assert!(!report.ok(), "impl Drop fixture 应当 FAIL");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.detail.contains("impl Drop")));
}

#[test]
fn drop_audit_negative_arc_field_detected() {
    let report = scan("drop-audit/negative/rt_arc_field.rs");
    assert!(!report.ok(), "Arc fixture 应当 FAIL");
    assert!(report
        .violations
        .iter()
        .any(|violation| violation.detail.contains("Arc<")));
}
