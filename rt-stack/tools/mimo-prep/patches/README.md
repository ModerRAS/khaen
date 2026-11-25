# patches — task-252 provenance fix for the 2026-11-19 MIMO pilot runner

`run_0830-design-provenance.patch` 是针对任务证据副本
`rt-stack/evidence/mimo-pilot-3mic/prep/ops-0830/run_0830.py` 的最小 unified diff。

## 为什么存在

task-252 根因：窗内/下发的 G-4096 plan（sha256 `ceb04215…`，release-v10 fixtures 与
2026-11-20 board-window runbook 使用）是 `planrt-20261119/gen_plans.py` 用默认
`--design=prep/dryrun/out/design/g_design.npz` 生成的**合成房间 dry-run 参考设计**；
而 `run_0830.py` 的设计/编译步骤没有任何 provenance 门：

1. `step_t66_compile()` 在 run summary 没有 design source 时**静默**回退到
   `prep/dryrun/out/design`（`if not design: design = DRYRUN/out/design`）。
   用 `--from t66_compile` / `--only apply` 恢复执行时，会把合成 G 打包进真实 plan 并下发。
2. `step_design()` 用 `steps/design_from_identity.py`（从 dry-run harness import 目标函数），
   对 H 无有效性门：08:30 design3mic 的 MA1/ARC 是 -42.14 dBFS 字节映射伪影也照常设计。
3. 打包后没有 plan 级 provenance 检查。

## 补丁内容（5 个 hunk）

| hunk | 改动 |
| --- | --- |
| 1 | 顶部加入维护版 guard：`mimo_kit/provenance.py`（fail-closed） |
| 2 | LIVE `step_design` 改为 `python -m mimo_kit.design_chain --design-only`（H guard + G guard）；dry-run 保留合成 harness 并在 summary 记录 guard 结果 |
| 3 | `step_t66_compile`：`resolve_design_source()`，LIVE 无真实 design source → FAIL，**禁止**静默 dry-run 回退 |
| 4 | 打包后 LIVE 增加 `g_guard_plan()`（合成 plan sha / design 溯源 / 编译报告 sha 绑定）→ 拒绝 |
| 5 | `_run_local(..., env=None)` 以传入 `PYTHONPATH`（LIVE 调用维护版 kit） |

## 应用方式

证据副本本身**未修改**（被该 pilot 的 `SHA256SUMS.txt` 固定）。若重跑该窗口，先复制一份工作副本再打补丁：

- PowerShell（无 git）示例：`Copy-Item run_0830.py run_0830.working.py` 后手工按 patch 改；
- 有 `patch`/`git apply` 的环境：从项目根 `git apply -p1 rt-stack/tools/mimo-prep/patches/run_0830-design-provenance.patch`（当前仓库未提交 git）。

补丁生成/语法门禁：`rt-stack/evidence/mimo-provenance-fix/scripts/make_run0830_patch.py`
（输出 `patch/patch-meta.json`：源 sha `80e18555…`、补丁后参考副本 sha `b5280541…`、py_compile PASS）。
LIVE 空 summary 拒绝行为已由 `scripts/smoke_run0830_patch.py` 实测：
`provenance refusal (no silent dry-run fallback): live_no_design_source`。
