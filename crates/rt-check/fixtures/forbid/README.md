# ci/fixtures/forbid —— RT 禁止行为 lint fixture（WBS T06）

- `positive/rt_clean.rs`：无 F-01..F-44 子集禁止符号，`rt-check forbid` 必须 PASS。
- `negative/*.rs`：每文件至少包含一条禁止符号，`rt-check forbid` 必须 FAIL 且规则 ID 匹配：
  - `rt_vec_alloc.rs` -> F-01
  - `rt_arc_clone.rs` -> F-03
  - `rt_drop_box.rs` -> F-27
  - `rt_vec_error.rs` -> F-25

fixture 随 crate 内置（不在 Cargo target 目录下），不参与构建；
由 crates/rt-check/tests/forbid_fixtures.rs 真实执行。
