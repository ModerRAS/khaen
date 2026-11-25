//! RoomForge 鞋盒镜像源法（ISM）仿真移植（calib-chain-20261121 / mimo-opt-20261122）。
//!
//! **SIMULATED / SYNTHETIC ONLY**：本 crate 不读任何音频设备，不参与实时路径；
//! 它只把 RoomForge 的确定性声学模型移植进 TSPI，供离线夹具加载、跨语言校验与 MIMO 设计研究使用。
//!
//! 12 个模块（complex/room/ism/rir/wav/sha256/json/fft/matrix/fixture/metrics）已全部实现；
//! 公开 API、口径与跨语言验证结果见 `crates/room-sim/API.md`。
//!
//! 验证状态（2026-11-22，mimo-opt-20261122 夹具）：5 场景 / 141 个 C# 生成 WAV 的
//! `SceneFixture::load`（逐 WAV SHA256）→ `h_matrix`（65536 点 rfft）→ `rerender_check` 全部通过，
//! max|Δ| ≤ 2.12e-8（f32 量化级；把期望值按 f32 舍入后与 WAV 逐位相等），门限 1e-5。
//! `cargo test -p tspi-room-sim --offline`：54 单元 + 3 mini-fixture 集成测试通过。
pub mod complex;
pub mod fft;
pub mod fixture;
pub mod ism;
pub mod json;
pub mod matrix;
pub mod metrics;
pub mod rir;
pub mod room;
pub mod sha256;
pub mod wav;
