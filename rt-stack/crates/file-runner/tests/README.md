# tests/ —— tspi-file-runner 私有测试占位

- 公共注册文件：`public_registration.rs`（integrator 独占）。
- T06 自证测试（integrator 写）：abi_layout.rs / const_contract.rs 等。
- leaf 测试请放 `tests/<Txx>/<name>.rs`，测试函数名带任务关键字以便 `cargo test -p <crate> <keyword>` 过滤。
- 合入时由 integrator 在 `public_registration.rs` 添加 `#[path = ...] mod ...;`；不要在 leaf 改动 manifest/lib.rs/注册文件。
