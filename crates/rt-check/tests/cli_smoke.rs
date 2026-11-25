//! T06 自证：rt-check CLI 真实进程入口/退出码（core-v1 §12.2/§12.4）。
//!
//! v2 迁移说明：
//! - fixture 随 crate 内置（`crates/rt-check/fixtures/`），不再读 legacy `rt-stack/ci/fixtures/`；
//! - 扫描根改为本 crate 的 `src/`：原实现扫 legacy workspace 全部 crates 作为门禁自证，
//!   但 v2 workspace 由多 agent 并行开发，跨 crate 扫描会把别人的在途状态耦合成本 crate 的
//!   测试失败；门禁对全仓的扫描由 CI 以 `rt-check forbid --root crates` 单独执行，不放在单元测试里。

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rt-check")
}

fn crate_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn abi_layout_exit_0() {
    let output = Command::new(bin())
        .arg("abi-layout")
        .output()
        .expect("run rt-check");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS abi-layout"), "stdout={stdout}");
    // core-v1 §11.2 v1.1.2：abi-layout 检查项数必须等于 spec 期望（旧 109 → 120）。
    let want = format!(
        "scanned={}",
        khaen_rt_check::abi_layout::RT_CHECK_ABI_LAYOUT_SCANNED
    );
    assert!(
        stdout.contains(&want),
        "abi-layout scanned 与 spec v1.1.2 不一致（期望 {want}）: stdout={stdout}"
    );
}

#[test]
fn forbid_crate_src_exit_0() {
    let output = Command::new(bin())
        .args(["forbid", "--root"])
        .arg(crate_src())
        .output()
        .expect("run rt-check");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn forbid_negative_fixture_exit_1() {
    let output = Command::new(bin())
        .args(["forbid", "--root"])
        .arg(fixtures().join("forbid/negative"))
        .output()
        .expect("run rt-check");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn drop_audit_crate_src_exit_0() {
    let output = Command::new(bin())
        .args(["drop-audit", "--root"])
        .arg(crate_src())
        .output()
        .expect("run rt-check");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS drop-audit"), "stdout={stdout}");
}

#[test]
fn plan_hash_not_implemented_exit_3() {
    let output = Command::new(bin())
        .arg("plan-hash")
        .output()
        .expect("run rt-check");
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn unknown_command_exit_2() {
    let output = Command::new(bin())
        .arg("no-such-command")
        .output()
        .expect("run rt-check");
    assert_eq!(output.status.code(), Some(2));
}
