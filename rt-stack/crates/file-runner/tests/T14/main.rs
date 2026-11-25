//! T14 私有测试套件（crate `khaen-file-runner`；自动发现 `tests/T14/main.rs`）。
//!
//! 覆盖 WBS T14 DoD：自有 file 解析/解包（WAV）、`khaen-fir/v1` manifest（T55 字段面）、
//! `khaen-cal-ir-draft/v1`（T13）、session、runner 执行 core+IR/flush/metadata、固定精度
//! （f32-strict）与版本锁、完整图版本（规范 SHA-256）、identity 保护、host 固定 buffer
//! source 接口与确定性元数据。
//!
//! 测试全部离线、确定性、零第三方依赖；不碰板端/ALSA/服务。运行：
//! `cargo test -p khaen-file-runner`（或 `cargo test -p khaen-file-runner --test T14`）。

mod common;
mod dod;
mod fir_cases;
mod graph_cases;
mod ir_cases;
mod json_sha;
mod runner_cases;
mod session_cases;
mod source_cases;
mod wav_cases;
