# 校准规范（设计侧）—— khaen-calibration/v1 / v1.1

> 维护：upmix-dev（设计/契约/验收）；实现：audio-stack（projects/tspi-audio-dsp/calibrate/，Rust）。
> Lead 要求：本 task 只出**规范和 review**，不改 calibrate/ 代码。
> 版本：v1（2026-11-15 初稿）+ v1.1（2026-11-16 Rust 已实现；v1 文件继续按 v1 规则校验，差异见 CONTRACT.md §10）。

## 文件

| 文件 | 内容 |
| --- | --- |
| CONTRACT.md | khaen-calibration/v1 / v1.1 YAML 契约：字段/语义/归一化/到 channelmap v2 与 CamillaDSP 的映射/校验规则 |
| ACCEPTANCE.md | 验收口径 T0–T6：1:1 环回、UMIK 实测、**应用后复测（T3 必须，K4）**、holdout（T6）、**合并响应/相位抵消判据**、64/128 帧实时链路、负向用例；话筒方向/校准文件、kMainTrim/电平/噪声底、报告 schema |
| examples/ | 5.3.2、4.1.2，以及 v1.1 两通道 high_shelf（`calibration-v11-highshelf.example.yml` + 同名 `.camilladsp.yml`） |
| tools/validate_calibration.py | 契约校验器（支持 v1/v1.1；**独立复核**入口；stdlib + PyYAML；可板端跑）。v1.1：peaking q≥0.1 无上限；shelf q∈[0.1, 1.0] |
| tools/apply_calibration.py | 应用到 channelmap profile，或把 CamillaDSP 片段合并进完整配置（见下） |
| tools/check_pair_response.py | 合并响应/相位抵消检查器（纯 stdlib；读 FR CSV） |
| review/ | 对 calibrate/ 的 review 与逐轮签收（问题清单 + 建议，不改文件） |

## 校验入口

`khaen-calibrate validate` 是**主入口**（producer 自带，板端可跑）；Python 校验器保留为**独立复核**
（支持 v1/v1.1，不依赖 Rust 二进制；额外提供 `--layout-presets` 映射交叉校验和逐条 warning 明细）。

```bash
# 主入口（Rust）
khaen-calibrate validate calibration.yml

# 独立复核（Python）
python3 tools/validate_calibration.py calibration.yml \
  --layout-presets /etc/tspi-audio/layout-presets.yml
```

退出码：0 合法（可带 warning）、1 不合法、2 用法/依赖/读取错误；`--strict` 把 warning 视为失败，
`--json` 输出机器可读结果。

v1.1 的 q 按 type 分流（裁决 5）：peaking 为有限数且 ≥0.1（窄模态可 >10）；
`low_shelf`/`high_shelf` 的 q 是 RBJ slope S，必须在 [0.1, 1.0]（CamillaDSP `slope=12*S` 上限 12 dB/oct）。

## 应用

`tools/apply_calibration.py` 按 CONTRACT §4 把校准写到消费方，不改输入文件。
退出码同样是 0 / 1 / 2。`--dry-run` 只打印差异。

```bash
# 模式 1：channelmap v2（板端 T3/T4 先走这条；输出可直接 tspi-channelmap apply）
python3 tools/apply_calibration.py \
  --channelmap ../upmix/examples/profiles/upmix-4.1.2-movie.yml \
  --calibration examples/calibration-v11-highshelf.example.yml \
  --out /tmp/channelmap.cal.yml

# 模式 2：把 producer 片段合并进完整 CamillaDSP 配置
python3 tools/apply_calibration.py \
  --camilladsp ../upmix/examples/camilladsp/upmix-4.1.2-movie.yml \
  --fragment ../calibrate/examples/offline/calibration.yml.camilladsp.yml \
  --out /tmp/camilladsp.cal.yml
```

模式 1 默认跑 `tspi-channelmap validate`。模式 2 默认跑 `camilladsp --check`；
可执行文件不在 PATH 时 WARN 并仍返回 0。片段里的 `schema`/`note`/`channelmap_roles` 不会写进完整配置。

离线示例（peaking）在 `../calibrate/examples/offline/calibration.yml`（及同名 `.camilladsp.yml`）。
v1.1 shelf 示例是 `examples/calibration-v11-highshelf.example.yml`：两通道 `high_shelf`，q=0.7，
片段里是 `Highshelf` + `slope: 8.4`。

## 快速自检

```bash
python3 tools/check_pair_response.py --h1 FL-fr.csv --h2 SUB1-fr.csv --fc 80
```

## 契约与现有实现的对应

- 现有 Rust 草稿 model.rs 的 @@Calibration/ChannelCalibration/Peq@@ 与本契约基本同构；最小改动见 CONTRACT.md §6
  （加 schema/tool、channels[].name、delay 归一化 min=0、polarity 序列化为 normal|invert）。
- 消费方（WebUI/渲染器/独立 CamillaDSP）按 CONTRACT.md §4 合并；校准是“渲染层之上的 trim”，
  不覆盖上混等 render 层延迟/极性（XOR 与相加规则）。
- 验收按 ACCEPTANCE.md 执行；合并响应用 tools/check_pair_response.py 或 Rust 内嵌同公式。

## 状态

- 2026-11-15：v1 初稿完成（CONTRACT/ACCEPTANCE/校验器/检查器/review）。
- 已用 validator 交叉检查 audio-stack 离线示例 @calibrate/examples/offline{,-invert}/calibration.yml@：
  0 error 0 warning；其 cargo test 7/7 通过。
- Round 1 review：@review/CALIBRATE-DRAFT-REVIEW.md@（P0：64/128 帧硬编码、merge 判据不足、跨通道相位无公共基准、极性链、UMIK 48k）。
- Round 2 review：@review/CALIBRATE-ROUND2-REVIEW.md@ —— 发现 P0-2 极性双重应用、arecord 无 period/buffer、measure-multi 切窗风险。
- Round 3 review（P0 修复后）：@review/CALIBRATE-ROUND3-REVIEW.md@ —— **P0 全部关闭**（极性只取校准字段、arecord 参数化、
  measure-multi 逐槽互相关检测、FR re/im+phase_ref 可独立重建）；外部 checker 复算与其 pairs.json 一致；余 P1（reference_index、
  fit 归一化、角色名、PEQ clamp、报告 schema、SNR 等）不阻塞板端。待板端 T1/T4/T5。
- 2026-11-16：**v1.1 已实现**：Rust `khaen-calibrate` 写出 `schema: khaen-calibration/v1.1`；`filters[].type` 允许
  `peaking|low_shelf|high_shelf`，v1.1 的 `q` 只要求有限且 ≥ 0.1（无上限）；v1 文件仍只允许 peaking 且 Q≤10。
  校验入口分工：`khaen-calibrate validate` 为主入口，Python 校验器为独立复核（支持 v1/v1.1）。
- 2026-11-16：**校验器收尾**：修复「v1.1 纯 Peaking 但 Q>10 仍被标注『v1 可读取』」的误报（v1 消费方会拒绝 Q>10），
  该提示改为只在文件无 error 时给出。用例矩阵（v1 示例 ×4、v1.1 high_shelf、v1.1 Q>10、Q=0.1/Q=10 边界、Q<0.1、q=.nan、
  freq 19/20000/20001、v1+shelf、未知 schema、未知 type）与 Rust `validate` 交叉核对结论一致；
  证据见 `review/CALIBRATE-V11-VALIDATOR-RECEIPT.md`。
- 2026-11-16：**裁决 5**：v1.1 shelf 的 q 收紧为 [0.1, 1.0]（peaking 仍 ≥0.1 无上限）。
  Python 校验器、CONTRACT §2.1/§3.4/§10 与 `tools/apply_calibration.py` 按此落地；
  新增 `examples/calibration-v11-highshelf.example.yml`。
- 2026-11-17：**B2 校准契约/验收修订**（依据 `scratch/lead/doc-audit/CONSOLIDATED.md` §3.1/§4 B2，证据 `B2-EVIDENCE.md`）：
  - **C-13** delay_samples 方向改正：D_i=round(max_j t_j−t_i)，“相对最早到达”改为“施加到该通道的延迟补偿”；
    reference_index 应指向 D=0（到达最晚）通道；Python 校验器 warning 文案与 CONTRACT §2/§3.1/§5 同步。
  - **C-14** N2 负向用例 0.5 ms→0.25 ms（=1/(2·fc)，180°@2 kHz；48k=12 samples），报告注入值同步。
  - **C-15** T4 pair 收紧：仅同源+互补 LR4；4.1.2 只有 FL:SUB1:80、FR:SUB1:80；5.3.2 标 NEEDS_HW；高度/环绕/中置不参与 pair。
  - **C-16** T1 电平/隔离：固定 −12 dBFS、CueMix kMainTrim 记录（最好置 0）、隔离判据增加“低于噪声底 ≥10 dB”、报告加 noise_floor。
  - **C-17** T3 保留（K4）+ holdout（T6）定义；禁止 PASS_OPEN_LOOP；§9 三档（PASS / PASS_WITH_T1_PARTIAL / INCOMPLETE）。
  - **C-18** 话筒 90° 校准/麦轴朝天花板（0° 仅单只近场），报告记录 cal 文件 sha256/方向；未加载不得声称 ±3 dB。
  - B2 组 P1/P2：漂移显著性、pair 阈值标 engineering default、全局求和只 warning、report tests[]/holdout、T3 可校正频段口径（2026-11-17 D7 改为允许 +3 dB boost）、
    `band_dev`/`level_diff` 降级、`--inject-*`/`--mic-cal` 缺口、单座位、§3.1/§9 一致、merge 示例修正、目标曲线文档化、shelf 参数化说明等。
  - 工具侧（Rust）待 audio-stack 修：global_validation 进 overall、negative_controls 不参与 overall、capture_peak/SNR 门、
    `--inject-*`、`--mic-cal`、Rust 校验规则补齐（C-P1-7）；列表见 `B2-EVIDENCE.md`。
- 2026-11-17：**用户裁决 D8/D3/D7/D4 落地**（证据 `scratch/lead/decisions/DECISIONS-2026-11-17-EVIDENCE.md`）：
  - **D8** 4.1.2 SUB1 唯一物理口 = 槽位7/Line Out 8；Line Out 10（槽9）只是历史临时测试，**不是候选**（CONTRACT §2.1/§5/§11、ACCEPTANCE §0.8、examples、README）。
  - **D3** 5.3.2 中置按设计与主箱相同为全频；C 的 HP80/重定向/C:SUB pair/C 测试全部 **deferred（NEEDS_HW，不在 P1 验收内）**；4.1.2 pair 仅 FL:SUB1:80、FR:SUB1:80（ACCEPTANCE §6.3、CONTRACT §11、README）。
  - **D7** 允许 **+3 dB boost**（单段帽 +3.0 dB；每通道正向总和 >6 warning/>12 拒绝；预测峰值 ≤−6 dBFS、实测 ≤−1 dBFS；`unfilled_dips`）——ACCEPTANCE §5、CONTRACT §3.3/§5/§11、README、validator warning。
  - **D4** 目标曲线仅 **flat（默认）+ tilt10**（20 Hz 0→20 kHz −10 dB，log-f，参考增益独立）；旧 −3 dB@16k `tilt` 明确 superseded；其它曲线仅导入扩展（CONTRACT §10/§11、README；target-curves.yml 由 audio-stack 更新）。
