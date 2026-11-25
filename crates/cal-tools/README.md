# khaen-single-point（离线单点校准 CLI）

把合成场景的单点求解跑成一组**可复现、可校验**的文件。全部离线，不碰设备。

    khaen-single-point --scene scene.json --out out/

## 退出码

| 码 | 含义 |
| --- | --- |
| 0 | 跑完；安全钳位启用时 outcome = FULL（或未启用钳位） |
| 3 | 跑完；outcome = PARTIAL（有钳位/残差，见 report.json） |
| 4 | 跑完；outcome = NONE（限值内无可行解） |
| 2 | 输入/用法错误（`E_USAGE` / `E_INPUT`） |
| 1 | 运行错误（`E_RUN` / `E_WRITE`） |

stdout 是一行 JSON（`schema = khaen-single-point-scene-result/v1`）；错误写 stderr，一行 JSON
（`{"ok":false,"error":"E_...","message":"..."}`）。

## 输入 schema（`khaen-single-point-scene-input/v1`）

```json
{
  "schema": "khaen-single-point-scene-input/v1",
  "scene": {
    "scene_id": "demo",
    "fixture_dir": "synthetic",
    "ground_truth_sha256": "000...0",
    "seed": 7,
    "rir_scale": 0.5,
    "sample_rate_hz": 48000.0,
    "freqs_hz": [41.0, 53.0],
    "source_ids": ["SUB1"],
    "mic_ids": ["P01", "P02"],
    "design_mics": [0],
    "holdout_mics": [1],
    "h": [
      [[[1.0, 0.0]], [[0.9, 0.1]]],
      [[[0.8, 0.0]], [[0.7, -0.2]]]
    ],
    "band_centers_hz": [40.0, 50.0],
    "extended_centers_hz": [25.0, 31.5, 40.0, 50.0]
  },
  "run": {
    "taps": 64,
    "delay_samples": 16,
    "nfft": 8192,
    "quality": { "enabled": false },
    "safety": { "enabled": true }
  }
}
```

- `h[mic][bin][source]` 是复数 `[re, im]`；维度必须与 `mic_ids` / `freqs_hz` / `source_ids` 一致。
- `run.quality.enabled = true` 时必须给 `band_snr_db`（31 个 1/3 oct 频带，`null` = 该带无证据 → Unknown）。
- `run.safety.enabled = true` 才会出现 FULL/PARTIAL/NONE（4.4 安全钳位）；限值可在 `run.safety` 里覆盖
  `tolerance_db` / `max_predicted_peak_dbfs`。
- 未给的 `run` 字段取 khaen-mimo-design 的冻结默认值。

## 输出

```
out/
├── manifest.json                      # 逐文件 sha256 + 整包 bundle_sha256（确定性）
├── summary.json                       # 三态/质量门/measured=false 等摘要
└── single-point/
    ├── scene-report.json              # 完整场景报告（含质量门/权威/钳位残差）
    ├── metrics.csv
    └── fir/*.csv
```

整包摘要公式：`sha256(按路径排序的 "path\tsha256\n" 拼接)`；同输入两次运行逐字节相同。

## 与 daemon 联调（可选）

产物里的 `single-point/plan.bin`（来自 `plan_compile` 的确定性字节）可以先交给 daemon 的只读试运行接口验一遍，再决定要不要准备/提交：

    curl -s -X POST http://127.0.0.1:8730/api/plan/inspect \
      -H 'content-type: application/json' \
      -d "{\"plan_path\": \"/var/lib/khaen/out/single-point/plan.bin\", \"plan_sha256\": \"<sha256>\"}"

返回 `admission.accepted`、`admission.rejections`、`compile.compiled` 与 `latency_samples`，且 `staged:false`（不产生草稿、不改变正在跑的图）。本工具本身不联网、不访问 daemon。

## 诚实边界

输入是**合成场景**（SIMULATED / UNPROVEN），产物里 `measured/applied` 恒 false；本工具不把 predicted
写成 measured，也不做真实采集/下发设备/闭环复测（那需要出声窗口，标 NOT_RUN）。

---

# khaen-closed-loop（闭环复测判定，离线）

把「预测电平」与「实测电平」按 1/3 octave 带逐点对账，输出确定性报告与三态退出码。只做对账：
不测量、不碰设备；真实实测电平来自 T42 设备侧流程（NOT_RUN，需要出声窗口）。

    khaen-closed-loop --predicted predicted.json --observed observed.json --out out/ [--tolerance 2.0] [--min-bands 3]

| 退出码 | 含义 |
| --- | --- |
| 0 | PASS：所有可比较带 `|实测-预测|` <= 容差，且可比较带数 >= min-bands |
| 3 | FAIL：至少一个可比较带超容差 |
| 4 | INCOMPLETE：可比较带不足（数据不够；缺数据永远不算通过） |
| 2 | 输入/用法错误（E_USAGE / E_INPUT / E_ALIGNMENT / E_CONFIG） |
| 1 | 写报告失败（E_WRITE） |

输入 schema `khaen-closed-loop-levels/v1`（示例，缩进为代码）：

    {
      "schema": "khaen-closed-loop-levels/v1",
      "sample_rate_hz": 48000.0,
      "points": [
        {"name": "P01", "bands": [
          {"center_hz": 40.0, "level_db": 60.0},
          {"center_hz": 50.0, "level_db": null}
        ]}
      ]
    }

- 两份输入必须采样率一致、测点集合与频带中心完全一致；任一侧 `level_db` 为 null 记 missing。
- 输出目录必须为空或不存在，防止把旧报告混进来。
- `out/report.json` 键序固定、浮点 10 位有效数字、末尾换行；同输入两次逐字节相同（sha256 可复现）。
- 判定容差默认 2.0 dB（与目标 ④ 一致），`--min-bands` 默认 3（不足判 INCOMPLETE，不硬判 PASS）。

---

# khaen-plan-levels（从方案推预测电平，离线）

把已编译的方案（plan ABI v3）变成闭环复测的 **predicted 侧**：读方案文件，算每个保护项
（lane）在该 lane 的 PEQ + 增益下的预测带域电平，写出 `khaen-closed-loop-levels/v1`
JSON，可直接喂给 `khaen-closed-loop --predicted`。

    khaen-plan-levels --plan plan.bin --out levels.json [--centers 25,31.5,40,...]

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 2 | 输入/用法/方案错误（E_USAGE / E_IO / E_PLAN / E_PLAN_LEVELS_INPUT / E_PLAN_LEVELS_CONFIG / E_PLAN_LEVELS_PLAN） |
| 1 | 写输出失败（E_WRITE） |

- 预测口径：`gain_db` + 该 lane 全部 PEQ biquad 在中心频率处的级联幅度（dB）；biquad 稳态
  频响为 `(b0 + b1*z^-1 + b2*z^-2) / (1 + a1*z^-1 + a2*z^-2)`。
- 测点名来自 lane 序号（`L00` / `L01` / ...），因为方案里没有测点名字；实测文件必须用同一命名。
- 只接受恒等的内容布局 / 分频 / 矩阵 / 路由段；否则报 E_PLAN_LEVELS_PLAN 而不是悄悄少算。
- ⚠ 诚实边界：这是**相对频响**（0 dB = 方案外无滤波），不含房间、单元与麦克风响应；
  实测侧必须用同一参考归一化后再对账。方案系数是 f32 量化的，低频远带预测误差量级约
  1e-2 dB（远小于 2 dB 判定容差，测试里用 0.05 dB 锁住该边界）。
- 输出 JSON 与 `khaen-closed-loop` 的输入同一规范：固定键序、10 位有效数字、末尾换行，
  同输入两次逐字节相同。
