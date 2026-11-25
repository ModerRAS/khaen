//! 采集层常量占位（T62 硬件采集模块未随本 crate 迁入）。
//!
//! v1 里 `UMIK_ORIENTATION_VERTICAL` 由 `khaen-cal-dsp::capture` 提供、`calibration::capture`
//! 再导出；T62 capture（ALSA/UMIK 设备打开）依赖重写中的 host，按 T11 范围不迁入。
//! 这里保留该字面值常量以维持 `crate::capture::UMIK_ORIENTATION_VERTICAL` 的公开路径，
//! 供 T63 多点位置契约使用（ACCEPTANCE §0.1.2：多声道听音位麦克风朝向 90°）。

/// 听音位麦克风朝向字面值（ACCEPTANCE §0.1.2）。
pub const UMIK_ORIENTATION_VERTICAL: &str = "90deg";
