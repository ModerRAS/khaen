# multipoint-prep —— 多点 measured spec v2 离线生成/校验工具

> 目标：为真实多点测量窗口（design>=6 + validation/holdout>=3）做**离线**预演与交接工具准备。
> 纯离线：不打开设备、不出声、不碰 MOTU/dsp-host。真实 6+3 测量窗口 / T6 / T68 仍 **NOT_RUN**。
> 工具版本：`make-multipoint-spec 1.0.0`（Python>=3.8，仅标准库）。

## 1. 它解决什么

旧工具 `tspi-calibrate measure-multi` 每个话筒位置产出一组
`<CH>-chNN.analysis.json` / `<CH>-chNN.diag.json`；新栈 `tspi-cal` 的 measured 设计输入是
spec v2（`tspi-cal-measured-design/v2`，见 `rt-stack/spec/measured-design-v2.md`）。
本工具把“按位置命名的 analysis 目录”转成 spec v2 + sha256 manifest，并在生成前做 fail-closed
质量门检查（schema/fs/fr 频点/SNR/repeats/clock_ppm/onset/90deg/T6 点数/时钟一致性）。

## 2. 输入目录布局

推荐（tree，测量交接包根目录 = spec 所在目录）：

    PKG/
      D1/FL-ch00.analysis.json  D1/FL-ch00.diag.json     # 旧工具 measure-multi 原样命名（推荐）
      D1/FL.analysis.json       D1/FL.diag.json          # 或重命名成 <CH>.analysis.json
      D1/FL.json                D1/FL.diag.json          # 或 <CH>.json
      ...
      D6/...
      V1/... V2/... V3/...
      spec.json                                          # 工具输出
      spec-manifest.json                                 # 工具输出
      synthetic-manifest.json                            # 仅 synthetic 合成夹具

兼容（flat，旧 `multipoint_probe` 夹具命名）：`D1-FL.analysis.json` + `D1-FL.diag.json`。

位置角色默认由 label 前缀推断：`D*`=design、`V*`=validation；也可用
`--design-labels/--validation-labels` 或窗口前冻结的 `--positions` 表
（`tspi-cal-multipoint-positions/v1`，含 label/role/weight/mic_orientation）。

## 3. 命令

    # 0) 工具自测（正向/负向/确定性；写 SELFTEST.json）
    python make_multipoint_spec.py selftest --out SELFTEST-DIR

    # 1) 合成 6+3 夹具（仅离线预演；manifest 标 SIMULATED）
    python make_multipoint_spec.py synthetic --out PKG --design 6 --validation 3 --seed multipoint-prep-20261122

    # 2) analysis 目录 -> spec v2 + manifest
    python make_multipoint_spec.py build --input PKG --out PKG/spec.json       --manifest PKG/spec-manifest.json       --session-id multipoint-<window>-<layout>       --provenance <real_umik1_per_position_... | simulated_...>

    # 3) 校验已有 spec（文件/质量门/点数）
    python make_multipoint_spec.py check --spec PKG/spec.json --require-t6

    # 4) 逐位置合成 IR（仅 RT60 工具链预演；SIMULATED）
    python make_multipoint_spec.py synthetic-ir --out PKG-ir --design 6 --validation 3

    # 5) 离线全链证据机读校验（measured -> FitP -> compile -> plan-arena fake）
    python make_multipoint_spec.py verify-e2e       --run RUN --report REPORT --package PKG       --perturbed-run RUN-HOLDOUT-PERTURBED --perturbed-report REPORT-HOLDOUT-PERTURBED       --rt60-by-position RT60-BY-POSITION --out checks/VERIFY.json

## 4. 质量门（fail-closed）

| 检查 | 门槛 | 错误码 |
| --- | --- | --- |
| T6 最低点数 | design>=6 + validation>=3（pilot 需显式 `--allow-below-t6`） | `E_MP_T6` |
| analysis schema | `tspi-channel-measurement/v1` | `E_MP_ANALYSIS` |
| 采样率 | `meta.sample_rate == spec.sample_rate_hz`（同一 session） | `E_MP_ANALYSIS` |
| 频响 | fr >=8 点、频率单调、频带 >2:1 | `E_MP_ANALYSIS` |
| SNR | 默认 >=25 dB（analysis `snr_band_db` 优先，否则 `snr_db`）；降门会记 warning | `E_MP_QUALITY` |
| diag repeats | >=3 遍（design/off） | `E_MP_DIAG` |
| diag clock | `clock_ppm_ok=true`、\|ppm\|<=100；位置间 ppm 跨度 <=5 | `E_MP_QUALITY` |
| onset / peak | `onset_invalid=false`、`ir_peak_suspect` 无 true | `E_MP_DIAG` |
| 通道集合 | 每个位置覆盖同一组通道，通道名 ^[A-Z][A-Z0-9]*$ | `E_MP_POSITIONS` |
| 话筒朝向 | 90deg（T63 听音位 plan 只接受 90deg） | `E_MP_POSITIONS` |
| 缺 diag | 默认拒绝；`--allow-missing-diag` 放行并记 warning | `E_MP_DIAG` |

错误码前缀 `E_MP_*`；stdout 输出单行 JSON 摘要，stderr 输出
`ERROR [code] message`（结构化失败，便于窗口脚本判据）。

## 5. 输出

- `spec.json`：`tspi-cal-measured-design/v2`（analysis/diag 路径相对 spec 目录、POSIX 分隔符）；
  `validation = independent_validation_positions`；design/validation 分列。
- `spec-manifest.json`：spec sha256、逐位置逐通道 analysis/diag sha256、points、delay、
  snr、repeats、clock_ppm、T6 点数状态、warnings、`simulated` 标记。
- 合成数据一律标 `SIMULATED`（synthetic-manifest 的 `simulated:true` + spec.provenance 含
  `simulated`）；**不得**把合成 spec 当真实测量。

## 6. 与测量流程/窗口的关系

- 真实测量步骤、电平/重复/clock/session 纪律、失败判据与回滚：见
  `docs/MULTIPOINT-MEASUREMENT-PROCEDURE.md`；
- 窗口 RUNBOOK 的多点章节：`rt-stack/evidence/calib-chain-20261121/day-window-20261122/RUNBOOK.md` §12；
- 离线 6+3 预演证据：`rt-stack/evidence/calib-chain-20261121/multipoint-prep-20261122/`。
