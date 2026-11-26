# khaen-analyze：纯 Rust 离线分析工具链

> 状态：Windows 本机实现 + 测试通过（48 个测试，三条门禁绿）；用 2026-11-26 真机闭环录音的
> **合成等价件**（9 s / 48 kHz / 24-bit / 2ch，2.59 MB，与设备录音同规格同长度）验证；
> 真机录音本体在设备上，本机重放 = NOT_RUN（见 §11）。工具只读 WAV、只写 JSON，不碰设备、不联网。

## 1. 为什么有这套工具

2026-11-26 真机闭环复测（见仓库外 `HANDOFF/20261126-khaen-deploy-v015/CLOSED-LOOP-20261126.md`）
暴露了两件离线分析需求：

1. **首次基线录音中途掉音**（4.3–4.9 s，覆盖扫频 630–1000 Hz）让 30 带全频判 FAIL（最大 Δ 30.7 dB），
   录音作废重录后才回到 ±0.5 dB。当时只能临时写 numpy 脚本看包络，脚本不入库、不可复现。
2. **Welch 频带 / 低频诊断**同样只能临时脚本，口径（窗长、归一化、1/3 oct 边界）无法追溯。

本 crate 把这些一次性脚本变成仓库内一条**纯 Rust、确定性、可测试**的分析工具链：
`spectrum`（Welch 频谱 → 1/3 oct 带电平，可选原始 PSD）、`envelope`（短时 RMS 包络 + 掉音检测）、
`compare`（两段录音逐带相对电平与 Δ）。**不需要 Python / numpy**。

## 2. 与既有工具的关系（口径复用，不复制数值逻辑）

| 分析 | 实现来源 |
| --- | --- |
| WAV 解码（PCM 16/24/32、float32、1..=8 声道混单声道） | `khaen-calibration::wav`（与 `khaen-band-levels` 同一份） |
| Welch 带功率 / 相对带电平 | `khaen-calibration::band_levels`（`khaen-band-levels` 用的同一份 Hann/50% 重叠/频带积分代码） |
| 1/3 oct 默认中心频率（25 Hz–20 kHz，30 带） | `khaen-calibration::DEFAULT_CENTERS_HZ` |
| 默认窗长 4096、步长一半、Hann 窗 | `khaen-calibration::band_levels::DEFAULT_WINDOW_LEN` |

本 crate 新增的只有 **PSD 数组输出**（realfft 按同一 bin 功率归一化逐 bin 计算，单元测试锁定
「PSD 按 bin 带宽积分 = 带功率」）与 **包络/掉音检测**。因此：

- `compare` 的 `delta_db` 与 `khaen-band-levels` 的 `level_db` **逐位相同**（同一对 WAV、同一组中心频率时；
  集成测试锁死）；
- `spectrum` 的带电平与 `khaen-band-levels` 的带功率是同一份算法，只是多了一步 dB 换算；
- 判定路径不变：闭环判定仍然交给 `khaen-closed-loop` / `khaen-closed-loop-run`（见 `docs/CLOSED-LOOP.md`、
  `crates/cal-tools/README.md`）。

## 3. 构建与运行

```bash
cargo build -p khaen-analyze --offline --release    # 产物 target/release/khaen-analyze
cargo run  -p khaen-analyze --offline -- spectrum --wav baseline.wav --out spectrum.json
```

三条门禁（仓库根执行）：

```bash
cargo fmt -p khaen-analyze
cargo clippy -p khaen-analyze --offline --all-targets -- -D warnings
cargo test  -p khaen-analyze --offline
```

## 4. `spectrum`：Welch 频谱 → 1/3 oct 带电平

```
khaen-analyze spectrum --wav FILE --out FILE [--window 4096] [--centers 25,31.5,...] [--psd 0|1]
```

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `--wav` | 必填 | 输入 WAV（只读） |
| `--out` | 必填 | 输出 JSON（必须不存在，拒绝覆盖已有证据） |
| `--window` | 4096 | FFT 窗长（样本，2 的幂，256..=65536）；步长固定为一半 |
| `--centers` | 25 Hz–20 kHz 共 30 带 | 1/3 oct 中心频率（逗号分隔，严格升序、低于 Nyquist） |
| `--psd` | 0 | 1 = 额外输出单边 PSD 数组 |

**算法**（与 `khaen-calibration::band_levels` 同一口径）：

- Hann 窗 `w[n] = 0.5 - 0.5·cos(2πn/N)`，50% 重叠，功率域平均；
- 单边 bin 功率 `P[k] = c(k)·|X[k]|² / (N·Σw²)`，`c(0)=c(N/2)=1`，其余 `c=2`；
- 单边 PSD `S[k] = P[k]/Δf = P[k]·N/fs`（单位 满幅²/Hz）；
- 1/3 oct 带 `[c·2^(-1/6), c·2^(+1/6))` 内 bin 求和 = 带功率；带电平 = `10·log10(带功率)`。

**dBFS 口径**：满幅幅度 = 1.0，参考功率 = 1.0（满幅方波均方）。**幅度 1.0 的正弦读 −3.01 dBFS**
（即它的 RMS），与通常「0 dBFS 满幅正弦」的 dBFS 定义一致。

**输出 schema**（`khaen-analyze-spectrum/v1`，键字典序、pretty、末尾换行；示例为实际运行结果）：

```json
{
  "bands": [{ "center_hz": 100.0, "level_dbfs": -32.20534848122677 }],
  "channels": 2,
  "command": "spectrum",
  "duration_s": 9.0,
  "hop": 2048,
  "level_unit": "dBFS",
  "sample_rate_hz": 48000.0,
  "samples": 432000,
  "schema": "khaen-analyze-spectrum/v1",
  "segments": 209,
  "tool": "khaen-analyze",
  "tool_version": "0.1.5",
  "wav": "baseline.wav",
  "window": "hann",
  "window_len": 4096
}
```

`--psd 1` 时多一个 `psd` 对象：`{ "bin_hz": 11.71875, "segments": 209, "window_len": 4096,
"unit": "full_scale_squared_per_hz", "values": [ ... 2049 个单边 PSD ... ] }`，下标 k 的频率 = k·bin_hz。

**stdout**（一行 JSON 摘要，含输出文件 sha256）：

```json
{"bands":7,"command":"spectrum","levels_sha256":"466ae9ce...","missing":0,"ok":true,"out":"spec.json","psd":false,"segments":209,"tool":"khaen-analyze","tool_version":"0.1.5"}
```

> ⚠ 数值底噪：无信号的带**通常不是 null**，而是 FFT 舍入产生的极低电平（约 −130 dBFS 级，随幅度/窗长变化）；
> null 只出现在带功率**精确为 0** 的情况（全零输入等）。这与 `khaen-band-levels` 的 `power > 0` 口径一致。
> 判读时不要把低于约 −100 dBFS 的带当真实信号。

## 5. `envelope`：短时 RMS 包络 + 掉音检测（复测前置质量门）

```
khaen-analyze envelope --wav FILE --out FILE [--window-ms 50] [--dropout-db -20] [--min-windows 3]
```

| 参数 | 默认 | 含义 |
| --- | --- | --- |
| `--wav` / `--out` | 必填 | 输入 WAV / 输出 JSON（后者必须不存在） |
| `--window-ms` | 50 | 短时 RMS 窗长（毫秒，> 0）；步长固定为一半 |
| `--dropout-db` | -20 | 掉音阈值（相对中位电平，dB，必须 < 0） |
| `--min-windows` | 3 | 连续掉音窗数达到多少才记一个 gap |

**算法**：矩形窗 RMS，每窗电平 `20·log10(rms)` dBFS（整窗全零记 null）；参考电平 = 全部有信号窗的
dB 中位数；**在有信号区间内**（第一个到最后一个非 null 窗），电平 < 参考 + 阈值且连续 ≥ min_windows
的窗段记一个 gap。文件头/尾的静音（闭环协议的 1.5 s 前后静音）算**边界**、不算掉音。

**判定**（`verdict`）：`clean` / `dropout` / `silent`（全部窗无信号；缺数据不冒充通过）。

**输出 schema**（`khaen-analyze-envelope/v1`，节选；`levels_dbfs` 为每窗电平数组，null = 整窗全零）：

```json
{
  "channels": 2,
  "command": "envelope",
  "dropout_db": -20.0,
  "duration_s": 9.0,
  "gap_count": 1,
  "gaps": [
    { "depth_db": -53.990106302933704, "end_s": 4.9, "start_s": 4.3, "windows": 23, "worst_dbfs": -69.04208905540284 }
  ],
  "hop_samples": 1200,
  "level_unit": "dBFS",
  "max_gap_s": 0.6000000000000005,
  "min_windows": 3,
  "reference_median_dbfs": -15.051982752469137,
  "sample_rate_hz": 48000.0,
  "samples": 432000,
  "schema": "khaen-analyze-envelope/v1",
  "signal_end_s": 6.525,
  "signal_start_s": 1.475,
  "tool": "khaen-analyze",
  "tool_version": "0.1.5",
  "verdict": "dropout",
  "wav": "baseline-dropout.wav",
  "window": "rect",
  "window_ms": 50.0,
  "window_samples": 2400,
  "windows": 359
}
```

上面这份就是 2026-11-26 事故的**合成等价件**：9 s = 1.5 s 静音 + 5 s 指数扫频（−12 dBFS）+ 1.5 s 静音，
4.3–4.9 s 注入 −69 dBFS 本底 —— 工具报出 `verdict=dropout`、gap `4.3–4.9 s / 23 窗 / 深度 −54 dB`；
同结构无掉音版本报 `clean`、`signal_start_s=1.475`、`signal_end_s=6.525`、中位电平 −15.05 dBFS
（扫频幅度 0.25 的 RMS）。集成测试 `closed_loop_style_recording_with_gap_is_flagged` /
`..._without_gap_is_clean` 锁死这两条。

**边界（诚实说明）**：

- 窗口只有**完全落在掉音段内**才判掉音，报告的 gap 比真实掉音段**向内收缩**（最多各缩一个窗长/步长），
  不会往外扩；
- 参考电平是全部有信号窗的中位数：对 9 s 闭环录音（静音占 1/3）仍落在扫频电平上；若静音占比超过一半，
  中位数会被静音拉低（报告给出 `reference_median_dbfs`，可人工核对）；
- 若掉音紧贴扫频起点/终点、与头尾静音连成一片，会落进边界静音区而**漏检**；用 `signal_start_s` /
  `signal_end_s` 与 `levels_dbfs` 人工核对。

## 6. `compare`：两段录音的相对频带电平与 Δ

```
khaen-analyze compare --reference FILE --measured FILE --out FILE [--window 4096] [--centers ...]
```

参考 = 无方案基线录音，实测 = 施加方案后的录音（与 `khaen-band-levels` 的输入语义相同）。
`delta_db = 10·log10(实测带功率 / 参考带功率)`，带功率直接调用 `khaen-calibration::band_powers`；
同时给出两侧绝对带电平（dBFS，口径同 `spectrum`）与差值统计。两段采样率必须一致（否则
`E_ANALYZE_ALIGNMENT`）。

**输出 schema**（`khaen-analyze-compare/v1`，节选；实际结果 = 施加 +6.02 dB 的合成件）：

```json
{
  "bands": [
    { "center_hz": 100.0, "delta_db": 6.02059990135921, "measured_dbfs": -26.18474857986756, "reference_dbfs": -32.20534848122677 }
  ],
  "command": "compare",
  "compared_bands": 7,
  "hop": 2048,
  "level_unit": "dBFS",
  "max_abs_delta_db": 6.0205999339239025,
  "measured_wav": "applied.wav",
  "missing_bands": 0,
  "reference_wav": "baseline.wav",
  "rms_delta_db": 6.020599921582467,
  "sample_rate_hz": 48000.0,
  "samples": 432000,
  "schema": "khaen-analyze-compare/v1",
  "segments": 209,
  "tool": "khaen-analyze",
  "tool_version": "0.1.5",
  "window": "hann",
  "window_len": 4096
}
```

任一侧带功率精确为 0 时该带 `delta_db` / 对侧 `*_dbfs` 记 `null`（缺数据；闭环判定算 missing，绝不算通过）。

## 7. 与闭环复测流程怎么配合

推荐顺序（**诊断在前、判定在后**；掉音录音绝不进入判定）：

```bash
# 0) 拿到 baseline.wav / applied.wav（真机采集，9 s / 48 kHz / 24-bit / 2ch）
# 1) 前置质量门：任何一段有中途掉音就重录，不要往下走
khaen-analyze envelope --wav baseline.wav --out envelope-baseline.json
khaen-analyze envelope --wav applied.wav  --out envelope-applied.json
#    判读：verdict 必须是 clean；dropout 时看 gaps[].start_s/end_s 与 depth_db 决定重录区间

# 2) 诊断（可选）：两段录音的逐带 Δ 与绝对电平，确认方案效果方向对不对
khaen-analyze compare --reference baseline.wav --measured applied.wav --out compare.json
khaen-analyze spectrum --wav baseline.wav --out spectrum-baseline.json     [--centers 100,125,...,8000] [--psd 1]

# 3) 正式判定（既有工具，口径未变）
khaen-closed-loop-run --plan plan.bin --baseline baseline.wav --applied applied.wav     --out out/ --point L00 --lane 0 --centers 100,125,...,8000 --tolerance 2.0 --min-bands 3
```

- `envelope` 是**前置门**：2026-11-26 的教训是掉音录音会让全频判定 FAIL 且原因难定位；
- `compare`/`spectrum` 是**诊断**：`compare` 的 Δ 与 `khaen-band-levels` 的 observed 侧逐位一致，
  所以诊断数字和最终判定数字不会互相打架；
- `khaen-closed-loop-run` 仍是唯一 PASS/FAIL/INCOMPLETE 判定与证据包来源（见 `docs/CLOSED-LOOP.md`）。

## 8. 确定性与可复现

- 所有 JSON 由键字典序对象（BTreeMap）序列化、pretty 两空格缩进、末尾换行；
- 浮点用 serde_json 的最短往返十进制（ryu）：同一个 f64 永远同一字节；
- stdout 摘要里的 `levels_sha256` = 输出文件字节的 sha256，可写进交接/证据清单；
- `--out` 已存在直接拒绝（`E_WRITE`，退出码 1），防止覆盖上一次的证据；
- 集成测试 `*_output_is_deterministic` 对三个子命令断言「同输入两次运行：文件逐字节相同 + stdout 相同」。

## 9. 性能（实测，非预测）

测法：Windows 本机（开发机，其他 agent 同时在跑构建），`cargo build -p khaen-analyze --offline --release`，
输入 = 9 s / 48 kHz / 24-bit / 2ch（2.59 MB）WAV，每个命令跑 3 次取最小值；耗时含 WAV 解码与 JSON 写出。

| 命令 | 配置 | 实测 |
| --- | --- | --- |
| `spectrum` | 默认 30 带、window 4096 | 9.0 ms |
| `spectrum --psd 1` | 3 带 + 2049 bin PSD | 9.3 ms |
| `envelope` | 50 ms 窗（359 窗） | 8.6 ms |
| `compare` | 20 带、window 4096 | 8.8 ms |

冷文件缓存首次运行约 78 ms（第一次读 2.59 MB）；上表是热缓存稳态值，仅代表本机量级
（真实设备/CI 未测，NOT_RUN）。

## 10. 错误码与退出码

| 退出码 | 含义 |
| --- | --- |
| 0 | 成功 |
| 2 | 输入/用法错误 |
| 1 | 写输出失败 |

stderr 一行 `khaen-analyze: <CODE>: <message>`；带 whitespace 的用法错误文末带 `(try --help)`。

| 错误码 | 触发 |
| --- | --- |
| `E_USAGE` | 未知子命令/参数、缺参数、`--psd` 不是 0/1、`--centers` 空项/非数字、`--window-ms` 非数字 |
| `E_WAV_IO` | 读不到 WAV 文件 |
| `E_WAV_INPUT` | 不是合法 RIFF/WAVE、格式不支持、声道数/采样率非法、文件截断 |
| `E_BAND_LEVELS_INPUT` | 样本非有限、录音短于一个窗 |
| `E_BAND_LEVELS_CONFIG` | 窗长不是 2 的幂 / 超出 256..=65536 |
| `E_BAND_LEVELS_ALIGNMENT` | 中心频率空/非升序/≥ Nyquist |
| `E_ANALYZE_INPUT` | 包络：录音短于一个窗、样本非有限、窗内均方溢出 |
| `E_ANALYZE_CONFIG` | 包络：窗长非法、`--dropout-db` ≥ 0、`--min-windows` = 0 |
| `E_ANALYZE_ALIGNMENT` | `compare`：两段录音采样率不一致 |
| `E_WRITE` | 输出已存在、父目录建不了、写盘失败 |

WAV 与带电平错误码与 `khaen-band-levels` **完全同名**（同一份 khaen-calibration 实现抛出的稳定码）。

## 11. 测试覆盖与未验证项

48 个测试（Windows 本机全绿）：

- 12 个库单元测试：PSD 积分 = calibration 带功率（4 种窗长）、满幅正弦 −3.01 dBFS、窗长/采样率校验码、
  中位数、gap 位置/深度、全静音判定、config 校验码、compare 与 `relative_band_levels` 逐位一致；
- 36 个集成测试（跑真实二进制）：
  - `tests/wav.rs`（7）：PCM16/24/32、float32、立体声混单声道、坏文件/8-bit/9 声道/零采样率/缺文件错误码；
  - `tests/spectrum.rs`（9）：带电平 = RMS dBFS、满幅 −3.01、双音相对电平、16-bit vs float32 一致、
    PSD 积分 = 带电平、自定义窗、非法窗/中心/录音过短；
  - `tests/envelope.rs`（8）：干净信号、掉音位置/深度/边界内缩、min-windows 抑制、阈值相对中位、
    全静音、**2026-11-26 事故等价件（有掉音/无掉音）**、非法配置；
  - `tests/compare.rs`（5）：−6.02 dB 增益、与 calibration 逐位一致、同文件 0 dB、全零侧 missing、采样率不匹配；
  - `tests/cli.rs`（7）：三子命令确定性、摘要 sha = 文件 sha、用法/输入错误码、`--out` 已存在退出码 1、`--help`。

**NOT_RUN**（本机做不了，不能写成实测）：

- 真机录音本体重放：设备 `/var/lib/khaen/closed-loop/{baseline2,applied}.wav` 在本机没有副本
  （HANDOFF 只留 sha256），本工具在真机 WAV 上的输出 = NOT_RUN；
- 在 N150 设备上运行 khaen-analyze：NOT_RUN（设备尚未部署 khaen，见 `docs/CLOSED-LOOP.md` §4.0）；
- Linux CI 的 fmt/clippy/test：NOT_RUN（本机 Windows 通过，CI 由集成方跑）；
- 16/24/32f/多声道覆盖来自**合成 WAV**，不是真机录音的格式多样性实测。
