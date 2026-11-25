# T71 板端测试注记（−50 dB 起步 / 192k 软警告已修）

> 来源：Cursor 终审审计 v0.9（`scratch/lead/cursor-audit/CURSOR-AUDIT-REPORT-v0.9.md`）F-08 / F-07；team task-174（证据/注记修复批次）；**2026-11-17 task-206 / Cursor CLI v1.0 N-05 更新：F-07 已修，撤销「未修」表述**。
> 日期：2026-11-17（Asia/Shanghai）。状态：**纯文档注记，未改任何脚本逻辑；板端仍未部署/未出声（NOT_RUN）**。
> 说明：本文件是 `deploy/README.md` 的补充注记。task-174 执行时 README 正被其它工作流写入（mtime 2026-11-17 18:54:52），按避让规则未改写 README；T71 owner 可将本文件要点并入 README §3/§7 后归档（LEAD-TODO）。

## 1. 板端测试从 −50 dB 起步（Cursor F-08）

- **T71 板端出声/真实播放测试一律从 −50 dB 起步**：首次给声用 −50 dB 起始音量，确认信号链/路由无异常后，再按需小幅上调；出现异常立即回静音（`KHAEN_HOST_SAFE_MODE=mute` / `08-rollback.sh`）。
- 为什么写成人工项：本目录脚本（00–08、verify-artifact.sh）不控制播放音量（音量属 CueMix/MOTU 与播放端），无法做成脚本硬门；因此列为 T71 checklist 人工项。
- 适用范围：T71 首次切换后的出声验证、T72/退役切换前后的任何真实播放；**不是**离线 dry-run/qemu 自测的必选项。
- 限制：Cursor 审计时真实播放为 NOT_RUN（与 `deploy/README.md` §7 一致）；本注记只声明起始电平纪律，不能作为板端可听验证已通过的证据。

## 2. 01-preflight 的 192k 软警告（Cursor F-07：**已修**，识别稳定拒绝码）

- **现状（现行即已修，2026-11-17 20:35 版本）**：`deploy/bin/01-preflight-switch.sh` = 16,243 B / `1be6923922e3ab2e97dfb532508558fd17dc5aebb466c0db2492297e3e3a9541`，其 I 门「已安装程序 192k 审计」（`:220-232`）按下列顺序判定：
  1. 二进制含稳定拒绝码 `E_UP_FS_192K_DISABLED` / `E_CM_FS_192K_DISABLED` → **PASS**（日志「已实现 176.4/192k 拒绝（稳定错误码；F-07）」）；
  2. 含 `192000` 但伴随「已禁用 / 禁止 192k / FORBIDDEN_SAMPLE_RATES / forbidden」措辞 → **PASS**（按拒绝路径，不误报）；
  3. 仅当只剩裸 `192000` 且未发现任何拒绝码/禁用措辞时才记 **soft** 软警告（owner 需限定 48/96k，keeper 只兜底设备速率）。
- **修复来源**：task-172（rt-deployfix，Cursor v0.9 F-07）随 F-43 同批改 `01`；证据 `rt-stack/evidence/cursor-fix-deploy/README.md` §F-07（`evidence/cursor-fix-deploy/after-hashes.txt` 记录 19:38 版本，其后 20:35 又按 M2b/dry-run 修复更新，现行 hash 以 `evidence/migration-plan/SHA256SUMS` 本批重算值为准）。
- **历史（已过时，N-05）**：task-174 创建本文件时（19:06）该脚本仍是 19:05 未修版本、只做裸 `192000` 软警告，故原文写「F-07 未修 / LEAD-TODO」；Cursor CLI v1.0 审计按该旧文再报 N-05。本段现按现行脚本更新，旧表述作废。
- **验证与未测**：本节为静态核对（`grep -nE 'E_UP_FS_192K_DISABLED|E_CM_FS_192K_DISABLED'` 命中 `:225`）+ dry-run 自检（`deploy/tests/run_dryrun_tests.sh`；Cursor v1.0 审计 2026-11-17 22:53 **PASS=22 FAIL=0**，`scratch/lead/cursor-audit/evidence/v1.0/dryrun-cli.log`）。**真实板端二进制上的误报是否消除仍是 T71 复跑观察项（NOT_RUN，禁止当已验证）**。

## 3. 证据

- 本文件创建前后哈希、命令与复核输出：`rt-stack/evidence/cursor-fix-evidence/EVIDENCE.md`（team task-174）；**F-07 状态更新（N-05）的 before/after 哈希与命令见 `rt-stack/evidence/cursor-fix-v1-evidence/EVIDENCE.md`（team task-206）**。
- 相关文件现状：`deploy/bin/01-preflight-switch.sh`（现行 16,243 B，F-07 已修；hash 见 `evidence/migration-plan/SHA256SUMS`，不在本文件写死以免与并发写入赛跑）、`deploy/README.md`（本批未改；现行 hash 见 `evidence/migration-plan/SHA256SUMS`）。
