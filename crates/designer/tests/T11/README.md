# tests/T11 —— khaen-designer 私有测试（WBS T11 target 导入）

- 任务：WBS T11（team task-70，owner rt-target-import）；写域 `crates/designer/src/targets.rs` + 本目录 + `rt-stack/evidence/T11/`。
- 公共注册文件 `public_registration.rs` 属 integrator 独占，**本任务没有改它**（也未改 Cargo.toml/lib.rs/CLI/README/ci/spec/reference）。
- 载体：`target_import_cases.rs` 由 `crates/designer/src/targets.rs` 文末
  `#[cfg(test)] mod targets_tests { use super::*; include!("../tests/T11/target_import_cases.rs"); }`
  在单测构建中编译执行（cargo 不自动发现 `tests/<子目录>/*.rs`；该 include! 模式与 T33/T35/T36/T38/T41 一致）。
- 运行：在 `rt-stack/` 下
  ```
  cargo test -p khaen-designer targets
  ```
  过滤词 `targets` 命中模块路径 `targets::targets_tests::t11_...`，当前 **60 tests / 0 failed**。
- 覆盖：CSV+sidecar 与单 JSON 两种形式、28 个 E_TGT_* 拒收码与 §6.9.1 唯一码优先级、单位/采样率/
  归一化（none/ref_1k/peak）/reference_gain_db/boost、phase_ref none/absolute/delay_removed、
  unwrap unwrapped/auto、target_delay_samples 整数与 200 ms 界、log-f 插值、hold/reject 端点与
  20 Hz/20 kHz 取值来源、flat/tilt10 内置目标（tilt10 参考值 20 Hz 0 → 20 kHz −10 dB）、
  非模态 solver 读取接口（`NonModalTargetSource`）、确定性 JSON 报告、sha256/规范序列化 oracle。
- 独立 oracle：sha256 已知向量与规范点序列哈希值来自 Node.js `crypto` 独立复算（见 evidence/T11/MANIFEST.md §5），
  不是用被测实现自证；tilt10 参考值取自 spec §6.8。
- 临时文件：两个文件读写测试用 `CARGO_MANIFEST_DIR/tests/T11/tmp-*`（不用 %TEMP%：本机 TEMP 指向内存盘 R:\Temp，
  测试进程无写权限），测试结束删除。
- F-23 终审修复（2026-11-17，Cursor v0.9）：`t11_targets_phase_fs2_ruling` 覆盖 `E_TGT_PHASE_REAL`
  唯一静态可判子条件 fs/2（实系数 FIR 在 Nyquist 的响应必为实数 H(fs/2)=Σh[n](−1)^n，非 0/180°
  相位不可实现；Lead 裁决把该频点 phase 目标整体拒收）；新增 `t11_targets_phase_real_ownership`
  断言 f<fs/2 的任意有限相位静态层不判（不猜测、不误拒）并核对 spec §6.6/§6.9 责任层归属
  （T37 设计层 + T55 报告层，含测试互引）非空。
- 未测：板端/ALSA/CueMix/真实测量/校准应用/复测/holdout；未与 T37/T55/T64 联调（接口按 spec §6 提供）。
