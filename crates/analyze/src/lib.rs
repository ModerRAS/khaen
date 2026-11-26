//! khaen 分析工具链（纯 Rust，无外部数值依赖）：频谱 / 包络 / 相对频带电平。
//!
//! 设计目标：把真机测量后的离线分析（Welch 频谱、短时包络/掉音检测、两段录音相对频带电平）
//! 做成仓库内可复现的工具，而不是一次性脚本或 numpy 环境。

pub mod envelope;
pub mod spectrum;
pub mod wav;
