# ci/ —— TSPI rt-stack 离线检查入口（WBS T06）

本目录只由 integrator 维护（T06/T26/T32）；离线、无网络依赖、无板端动作。

## 文件

- `check.ps1`：Windows/PowerShell 全量检查（`powershell -NoProfile -File ci/check.ps1 [-Offline]`；PowerShell 5.1+/7+，脚本为 ASCII-only 以兼容任意 locale）。
- `check.sh`：POSIX shell 全量检查（`sh ci/check.sh`；`CARGO_OFFLINE=1` 强制 `--offline`）。
- `fixtures/forbid/`：RT 禁止行为 lint 正/负 fixture（正必须 PASS，4 类负必须 FAIL）。
- `fixtures/drop-audit/`：无 RT 析构审计正/负 fixture（impl Drop / Arc 字段必须 FAIL）。

## 检查项（按顺序）

1. `cargo fmt --all --check`：格式必须干净；leaf 不得并行跑 `cargo fmt --all`（会覆盖他人）。
2. `cargo test --workspace`：真实执行；T06 自证测试 > 0（ABI 布局/类型/时限/rt-check fixture）。
3. `rt-check abi-layout`：core-v1 §11 结构 size/align/offset + §3 时限/容量向量。
4. `rt-check forbid --root crates`：扫描 RT-MODULE 文件的 F-01..F-44 子集禁止符号。
5. `rt-check drop-audit --root crates`：§13.2 类型清单 + RT 模块 Drop/owned 类型扫描。

## 退出码

- `0`：全部通过。
- `1`：rt-check 发现违规，或脚本某步失败（脚本会打印步骤名与 exit code）。
- `2`：用法错误/IO 错误。
- `3`：子命令尚未实现（`plan-hash` 归 T12/T13，`state-machine` 归 T21/T26）。

## 扩展规则（lint 规则/命令）

- 新增禁止符号：在 `crates/rt-check/src/forbid.rs` 的 `RULES` 加规则（标注 core-v1 F-xx ID），
  在 `fixtures/forbid/negative/` 加至少一条可触发 fixture，并在 `crates/rt-check/tests/` 加断言。
- 新增子命令：必须在 core-v1 §12.2 或 WBS 中有 owner/DoD；T06 scaffold 只提供入口占位。
- 不得引入网络依赖/第三方 crate（离线构建）；如需依赖，先回 integrator 更新 Cargo.toml/lock。

## 依赖/平台（T90）

- T90 起 `crates/rt-host` 有 **Linux target-specific** 依赖 `alsa = 0.12`（crates.io 最新稳定 0.12.1，`Cargo.lock` 锁定）。
  Windows/macOS 与本仓 Windows 检查**不编译/不链接** libasound；T17 fake backend 保持离线可用。
- Linux 上 `cargo test --workspace` 会编译 `alsa`/`alsa-sys`，前置 `pkg-config` + `libasound2-dev`（apt）。
- `-Offline`/`CARGO_OFFLINE=1` 需要本机已有依赖缓存：首次联网环境先 `cargo fetch`（lockfile 含 alsa 及其传递依赖）。
- 本目录脚本不做板端/ALSA 设备操作；真实 PCM 打开属 T17-follow-up/T26/T74 授权窗口。

## 目标目录

构建输出默认写入 `rt-stack/target/`（未提交）。可用 `CARGO_TARGET_DIR` 指向可重建的外部目录，
避免污染仓库；不要提交任何 `target/` 内容。
