# ci/fixtures/drop-audit —— 无 RT 析构 lint fixture（WBS T06 / core-v1 §13）

- `positive/rt_clean.rs`：RT 模块无 Drop/owned 类型，`rt-check drop-audit` 必须 PASS。
- `negative/rt_impl_drop.rs`：RT 模块含 `impl Drop`，必须 FAIL。
- `negative/rt_arc_field.rs`：RT 结构含 `Arc<...>`，必须 FAIL。

fixture 随 crate 内置（不在 Cargo target 目录下），不参与构建；
由 crates/rt-check/tests/drop_audit_fixtures.rs 真实执行。
