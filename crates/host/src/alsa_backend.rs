//! 真实 ALSA 播放后端（T70a）：打开 MOTU 播放 PCM、按块写 S24_3LE、xrun 恢复。
//!
//! 占位：实现由对应任务填充；公共 API 见 docs/DESIGN.md（T70 设备生命周期）。

#![allow(dead_code)]
