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
