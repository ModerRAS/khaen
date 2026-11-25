# tests/ —— tspi-modal-design 私有测试占位

- 公共注册文件：`public_registration.rs`（integrator 独占）。
- leaf 测试请放 `tests/<Txx>/<name>.rs`（例如 `tests/T07/direct.rs`），测试函数名带上任务关键字，
  以便 `cargo test -p <crate> <keyword>` 过滤。
- 合入时由 integrator 在 `public_registration.rs` 添加 `#[path = ...] mod ...;` 注册；
  **不要在 leaf 任务里改 Cargo.toml / lib.rs / public_registration.rs**。
- 测试内可引用 WBS 规定的 issue/evidence 路径；禁止写入 spec/ 与 reference/ 产物。
