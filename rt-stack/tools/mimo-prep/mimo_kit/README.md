# mimo_kit — fixed TSPI MIMO planrt analysis kit (task-249)

修复 2026-11-19 route-a INVALID 的两个 kit bug，并补上单测与噪声/静音防护。

## 修了什么

| # | 文件 | bug | 修法 |
|---|---|---|---|
| 1 | `extract.py`（原 `planrt_extract.py`, sha 5d024af7…） | MA1/ARC 字节映射错：`mono[0::3]=data[ch::60]`、`[1::3]=data[ch+20::60]`、`[2::3]=data[ch+40::60]`（按“3 组 20 字节”读，实际 ALSA S24_3LE 是每样本 3 字节交织）→ 混入其他通道字节，MA1/ARC 峰值恒 -42.14/-42.25 dBFS、SNR≈0 | 通道 c 取帧内偏移 3c/3c+1/3c+2（`s24.extract_channel_file`），流式分块，字节级精确 |
| 2 | `analyze.py`（原 `planrt_analyze.py`, sha 7e7f8bcd…） | `schedule()` 是 role-major 5 行，但 `estimate_H/compare` 用容器槽（SUB1=7）索引 starts/H/irs → IndexError | 行按 role 索引，H/IR 行按容器槽（SUB1=7），slots 4/5/6 不写；调度/roles/slots 改从 `<arm>/meta/session.json` 读 |
| 3 | `analyze.detect_offset` | 无界圆形 xcorr 的 `argmax(|c|)` + 任意 ±1 s 钳位：route-a identity UMIK raw_lag=-2,933,280 → 钳到 -48000，g +45600 → 假“两臂不一致” | 与已知 sweep 活动包络做**有界线性**（零填充、去均值）互相关；route-a 两臂均为 +48960（1.020 s），g 正确 MA1 +49440（+10 ms 采集偏差） |
| 4 | marker 契约（R3） | 旧 extract 写 `<arm>/playback.json`，analyze 读 `<arm>/meta/playback.json`，且缺 path/backend/level_db/spl_target_db/file_sha256 | 两处都写 v2 marker（必填字段齐全；`file_sha256`=会话输入文件 sha，两臂相同；`play_file_sha256`/`render_sha256`=各自渲染），analyze 读 meta 优先并做 A/B 门 |
| 5 | RT60/CSD | 旧输出 52.6–77.0 s（噪声尾主导） | 噪声底扣除（IR 前导/尾部估计）+ 动态范围门（T20 需高出噪声 ≥25 dB）+ 合理性上限 10 s，不满足返回 null；CSD T20 同样去噪 |
| 6 | 静音通道 | ARC -116.5 dBFS 仍被当有效麦 | `mic_stats.available`（peak>-90 dBFS 且 SNR≥6 dB）；3 麦 pilot 判定要求 UMIK/MA1/ARC 两臂都可用，否则 INVALID 并如实列出原因 |

## 用法

板端（stdlib；两个文件放同目录）：
```bash
python3 planrt_extract.py <work> <ev> <arm> [render.raw] \
  --backend aplay-direct-alsa --level-db -12.58 --spl-target-db 75 \
  --session-meta /dev/shm/planrt/session.json
python3 planrt_extract.py --selfcheck      # 板端字节映射自检（无 numpy）
```

PC 端（numpy）：
```bash
python3 analyze.py --before <ev>/identity --after <ev>/g --out <analysis> --require-mics 3
python3 -m pytest tests/test_mimo_kit.py -q
```

## 单测覆盖

- extract 字节映射逐字节精确（20ch，含负值/满量程）、流式与内存结果一致；旧映射对 -20 dBFS ch0 产出 -42.1 dBFS 伪影（回归）
- 符号扩展、静音通道全零、CLI 端到端（marker 必填字段/l两处一致/session v2 归一化）、selfcheck
- analyze role-major vs 容器槽（SUB1=7，slots 4/5/6 恒 0，无 IndexError）
- 有界线性 offset 检测（>1 s 延迟正确恢复；max_lag 钳位不绕回）
- marker 门（缺字段/不一致）、路径一致性门（offset 差 > 容差）、静音麦可用性
- 噪声感知 RT60：已知衰减 0.5 s → 0.3–0.8 s；纯噪声/无衰减 → null

## task-252 provenance guard + real-room design chain（2026-11-19）

背景：2026-11-19 下发的 G-4096 plan（sha256 ceb04215…，release-v10 fixtures + 2026-11-20 board-window runbook）
逐位等于 `prep/dryrun/out/design/g_design.npz` 的合成房间 dry-run 参考设计（`planrt-20261119/gen_plans.py` 的
`--design` 默认值），且设计链路对 H 无任何有效性门。`provenance.py` 是 fail-closed 门：

- **H guard**：3 麦缺失/静音、MA1/ARC S24 字节映射伪影（peak≈-42.14 dBFS, SNR≈0）、IR 峰超出 60 ms 直达窗、
  峰位跨 role/麦 spread >40 ms、participant 行死、NaN、dry-run H 来源 → 拒绝。
- **G guard**：注册表内合成 fixture 文件 sha、G 系数与 dry-run G_ideal/其 realize 结果 ≤1e-12、design-report
  synthetic/dry-run 标记、缺 H provenance、identity-only、NaN → 拒绝；plan 层再查编译报告 sha 绑定与合成 plan sha。
- **chain**：`resolve_design_source()` 对 LIVE 无真实 design source 直接 REFUSE（不再静默回退
  `prep/dryrun/out/design`；旧 `run_0830.step_t66_compile` 的潜在 bug）。

用法（PC，numpy；测试机 %TEMP% 在 R: 上，spawn 的 packer 不能写 R:，测试用 C: 临时目录）：

- `python -m mimo_kit.provenance registry --out registry.json`
- `python -m mimo_kit.provenance check-design --design <design dir> --live`
- `python -m mimo_kit.provenance check-plan --plan <plan.bin> --design <design dir>`
- `python -m mimo_kit.design_chain --arm <measured arm> --out <dir> [--design-only]`
- `python -m unittest discover -s tests -p 'test_provenance.py' -v`

`reference_design.py` 是旧 pilot 目标（3 麦空间复均值 + per-DAC 门）在维护目录内的副本，与 dry-run
`design_g` 数值逐位一致（`test_reference_design_parity_with_dryrun`），但不再从 dry-run harness import、
报告带真实 H provenance。证据：`rt-stack/evidence/mimo-provenance-fix/`；ops-0830 补丁：
`tools/mimo-prep/patches/run_0830-design-provenance.patch`。


## task-255 12k domain provenance guard（2026-11-19）

把上面的 fail-closed 门扩到 **12k / [mimo12k] artifact / plan**（release-v11 上板前置）：

- **H 门** `provenance.h12_source_check()`：接受 mimo-v2 8-slot 或 48k 7-output H；3 麦克、finite、静音麦、
  IR 直达峰 60 ms 窗、跨 role/麦 spread 40 ms、participant 行死亡 >60 dB → 拒绝。
- **artifact 门** `provenance.g12_artifact_guard()`：params 必填字段 + 12k 域一致性（48k/12k/group4/taps4096/
  D12=120/dsys=574）、g f32 尺寸/finite/identity-only、`g12_v2_4096.f64.npz` 内容绑定
  （`g_pure_sha256 == params.g_sha256`，f64↔f32 反量化一致）、resamp/params/g sha 与 provenance 一致、
  H provenance + 有效性复算、design-summary 绑定（H sha/g_pure sha/taps/D12）、注册 synthetic sha 与
  **内容容差 ≤1e-12**、synthetic/dry-run 标记；缺 provenance/缺 f64 副本 → 拒绝。
- **plan 门** `provenance.g12_plan_guard()`：plan sha == compile-report.plan_sha256、plan-provenance 存在且
  绑定 artifact（params/g/resamp/H sha）+ 编译报告 sha + artifact-guard passport；缺绑定 → 拒绝。
- **发证链**：`design12k.py` 的 `redesign_artifact()`（从 measured H 重跑交付目标函数并逐位复现交付 sha，
  写 f64 内容副本/design summary/design script/provenance 后过 guard）、`stamp_plan()`、`make_synthetic_fixture()`。
- **CLI**：`tools/mimo-prep/mimo12k_guard.py`（registry/redesign/check-artifact/make-synthetic/stamp-plan/
  check-plan/smoke）；**板端纯 stdlib 门** `tools/mimo-prep/mimo12k_guard_lite.py`（渲染/apply 前 rc=0）。
- 单测：`python tests/test_provenance12k.py`（26 项；48k 回归 `test_provenance.py` 20 项）。
  证据：`rt-stack/evidence/mimo-12k-provenance/`（REPORT/smoke 16 项/单测日志/guarded artifact+plan/负例 fixture）。
