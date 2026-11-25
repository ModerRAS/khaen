# calibrate/ 草稿 review（设计侧，2026-11-15）

> 对象：projects/tspi-audio-dsp/calibrate/（audio-stack 主写；本次 review 对应 2026-11-15 17:56 左右的版本）。
> 方式：只读 + 用契约校验器/独立检查器验证示例产物；**未修改 calibrate/ 任何文件**。
> 结论：**算法骨架与 YAML 契约已基本对齐**（7/7 单测通过、离线示例通过我的 v1 校验器），
> 但距 ACCEPTANCE.md 的 T1–T5 验收仍有若干 P0 缺口，主要是**跨通道相位测量前提、64/128 帧、
> 合并响应判据**三块。

## 0. 已验证的好消息

- cargo test 7/7（含 Farina 已知延迟/反相恢复、PEQ 拟合、合并抵消负/正例）。
- @examples/offline/calibration.yml@、@examples/offline-invert/calibration.yml@ 通过
  @calibration-spec/tools/validate_calibration.py@（0 error 0 warning）。
- 契约字段已落地：schema/sample_rate/reference_index/layout/tool/version/note；channels[].name 唯一、
  polarity 字符串、filters Peaking、delay_samples 已归一化 min=0、reference_index 指向修正=0 通道。
- 离线端到端可复现：simulate → analyze → merge → YAML/validation 报告。

## 1. P0（板端实测前必须解决；否则验收必挂）

| ID | 位置 | 问题 | 影响 | 建议 |
| --- | --- | --- | --- | --- |
| P0-1 | @src/raw.rs:114@（measure_channel） | aplay 硬编码 `--period-size=256 --buffer-size=2048`，没有 CLI/参数；README 却说“64/128 合法” | ACCEPTANCE T5 要求 64/128 帧可测且设备拒绝时报错；现在无法按 64/128 测，且属于静默固定值 | 加 `--period-size/--buffer-size` 参数（默认 128/512 或按 mode 注入），设备拒绝时返回错误；measure 命令记录实际 period/buffer |
| P0-2 | @src/main.rs:350-367@（cmd_merge 的抵消检查） | 只做 @margin = sum_db - max_single_db@ 全频带检查；没有交叉对、没有 Δφ(fc)、CD(fc)、陷波余量、纹波 | 与 ACCEPTANCE §6 的判据不一致；无法发现“单个 pair 反相/错延迟但全局和仍 >-6dB”的情况，也无法通过负向用例 N1–N5 | 按 §6.1 实现 pair 检查（可用 calibration-spec/tools/check_pair_response.py 的公式）：Δφ(fc)≤30°、CD(fc)≥-3dB、minCD≥-6dB、notch≥-6dB、ripple≤3dB；负向用例必须能 FAIL 并报 pair |
| P0-3 | @src/main.rs:130-149@（cmd_measure） + @src/raw.rs:91-124@ | 每通道单独起 arecord+aplay（sleep 350ms），各通道没有公共采集时间基准 | 跨通道相对相位/延迟不成立（ACCEPTANCE §0）；T2/T4 的相位抵消判据无法用于分次采集结果 | 两种方案任选：①在一次 arecord 会话里顺序播放所有通道扫频（通道间静音间隔），一次采集再按时间窗切分；②保留分次采集，但增加明确的公共参考（如每次同时录一路参考脉冲/loopback），并在分析里对齐。验收必须写明采用哪种 |
| P0-4 | @src/dsp.rs:149@（polarity 判定） | 极性直接取反卷积脉冲峰值符号，未扣除测量链（UMIK/话放/USB）自身极性 | 若麦克风链路整体反相，所有通道会被判 invert，校准文件整体极性错误 | 用 T1 的 1:1 电学环回先标定链极性（录一次电学回路，求其符号），之后所有测量减去该参考；或在 measure 里加入 loopback 参考通道 |
| P0-5 | @src/raw.rs:105@ / README | UMIK-1 只有 48 kHz；工具没有阻止 96 kHz 采集，也没有时钟漂移处理 | 96k（Dante）场景直接不可用/测错；任务要求支持 96k 链路验证 | 显式拒绝 capture 96k + UMIK-1（报错并提示改 48k 或换麦克风）；若支持 96k，须估计播放/采集漂移（≤100 ppm）并重采样补偿（ACCEPTANCE §0/T2） |

## 2. P1（契约/正确性；建议板端前修）

| ID | 位置 | 问题 | 建议 |
| --- | --- | --- | --- |
| P1-1 | @src/main.rs:326,339@（cmd_merge） | `reference_index = i`（文件循环下标），契约要求=channels[].index（播放通道号）；文件排序/缺通道时会错 | 记录 `reference_index = channels[k].index`（即 m.channel），并用 m.channel 排序/校验唯一性 |
| P1-2 | @src/main.rs:247@（cmd_fit） | 单通道 fit 直接写原始 `delay_samples`（未归一化）。单通道文件按契约 min 必须=0 | fit 产物要么归一化为 0（单通道参考=自身），要么标记为中间产物（不同 schema）；建议 fit 输出 `delay_samples: 0`，merge 再做相对修正 |
| P1-3 | @src/main.rs:324@（cmd_merge 默认名） | 未给 `--names` 时默认 @ch{index}@（小写），不符合角色 id 正则，也无法映射到 layout-presets | 强制要求 --names，或在给了 --layout 时按 layout-presets 角色/输出通道自动映射；缺失时报错 |
| P1-4 | @src/peq.rs:13@ | @Biquad::peaking@ clamp 到 freq 10..22000、q 0.1..20，超出契约/渲染器范围（freq≥20、q≤10） | clamp 改为 `freq ∈ [20, min(20000, fs/2-1)]、q ∈ [0.1,10]`；并检查 32 段上限与正向提升总和（≤12 dB，>6 警告） |
| P1-5 | @src/peq.rs:60-107@（fit_peq） | 目标曲线隐含“拉平到 0 dB”（残差=target-current），无 --target-curve/限幅策略；容易把房间谷过度提升 | 明确 v1 目标=平坦（写文档+报告），加最大正向提升/总 boost 限制（避免 0 dBFS 削波），并优先削峰而非填谷 |
| P1-6 | @src/main.rs:263-297@（cal_to_camilladsp） | 极性只写进 `channelmap_roles@ 提示，独立 CamillaDSP 片段里没有实际反相（Gain -1 / mixer inverted） | 独立配置片段应包含极性实现（Gain -1 或 mixer source inverted），否则“可直接消费”不成立 |
| P1-7 | @src/main.rs:217@（phase_sum_margin） | @dly = delays[i] + m.delay_samples@ 把“归一化修正”和“原始绝对延迟”相加；虽然当前恰好相加为常数，但语义重复、对舍入/其它调用会出错 | 只用归一化后的 delay（相对参考），或只用原始延迟差；不要二者相加 |
| P1-8 | @src/dsp.rs:64-96@（fractional_delay_samples） | 相位斜率固定用 200–8000 Hz；SUB（30–80 Hz）与高频分频（2 kHz）的延迟误差/群延迟可能被带外相位污染 | 延迟估计按目标频带（pair crossover 附近）或做多频带估计；验收时报告 delay@band |
| P1-9 | @src/dsp.rs:171-173@（SNR） | SNR 用 IR 尾部粗估，未做带限（200 Hz–8 kHz）、未做重复统计 | 按 ACCEPTANCE T2 做带限 SNR + ≥3 次重复/中位数；SNR <25 dB 时拒绝该次测量 |
| P1-10 | @src/model.rs:99-117@（ValidationReport） | 报告结构与 ACCEPTANCE §8 的 `khaen-calibration-report/v1` 不一致（无 pair 指标/负向用例/环境/文件哈希） | 统一报告 schema；至少含 pairs[]、metrics、thresholds、negative_controls、artifacts sha256 |
| P1-11 | @src/main.rs:408-448@（simulate） | frac_delay 线性插值且截尾（丢掉末尾），仅作离线演示；不能作为 1:1 环回验证 | 明确 simulate≠T1；新增 1:1 电学环回测量/验证路径（play→capture 同一接口），用于 T1 映射/隔离/延迟/极性精度 |
| P1-12 | @src/main.rs:130-149@ | 默认设备 `hw:4,0 / hw:3,0` 硬编码；板端 card 编号变化会错 | 从 tspi-mode/参数读取 device，或强制要求显式传入并打印核对 |

## 3. P2（改进/文档）

- @src/dsp.rs:120@：`coarse < 0@ 时 @start=0@ 的处理会丢掉“早到”信息；建议注释/处理负 lag 并对齐段长度加保护。
- @src/peq.rs:45-56@：`smooth` 用固定点数窗口（与频点密度绑定）；文档写清 300 点网格下 half_window=3 ≈ 0.25 oct，避免换网格后语义漂移。
- @src/main.rs:385-398@：`frac_delay` 只在 simulate 用；若后续测试 1:1 环回，需要更准确的分数字延迟（sinc/相位斜率）。
- README “块长：64/128 合法”与实际硬编码 256 矛盾，需要随 P0-1 一起改。
- @layout: synthetic-4@ 示例不是真实布局；最终验收示例请用 4.1.2/5.3.2 + 真实 layout-presets 交叉校验。
- 缺少与 @calibration-spec/tools/validate_calibration.py@ 的 CI 集成；建议 cargo test 之外加一条“生成示例→validator→check_pair_response”的脚本。

## 4. 与 ACCEPTANCE.md 的差距矩阵

| 验收项 | 当前 | 差距 |
| --- | --- | --- |
| T0 YAML 校验 | 有 schema/字段；示例已通过 validator | 缺 CI；fit 单通道延迟未归一化（P1-2） |
| T1 1:1 环回（映射/隔离/延迟精度/极性） | 只有 simulate 离线 | 无电学环回测量与隔离/精度验证（P1-11）；极性链未标定（P0-4） |
| T2 UMIK 重复性/漂移/SNR | 单次测量 | 无重复/漂移/带限 SNR（P1-9、P0-5） |
| T3 应用后复测 | 未实现 | 无“应用校准→复测→比对预测”流程 |
| T4 合并响应/相位抵消 | 全局 margin（-3/-6 dB） | 缺 pair 判据/负向用例（P0-2） |
| T5 64/128 帧实时 | period 固定 256 | 硬编码（P0-1） |

## 5. 建议的修复顺序（最小闭环）

1. 先修 P0-1（period/buffer 参数化）+ P0-3（公共采集会话/参考）——否则板端测出的相位不能用。
2. 再修 P0-2（pair 判据）+ 跑负向用例 N1–N5（可直接用 @check_pair_response.py@ 或移植同公式）。
3. P0-4/P0-5（极性链标定、UMIK 48k 限制/漂移）作为板端实测前置。
4. P1-1/P1-2/P1-3/P1-4 是低风险代码修正，建议一起做，避免生成的 YAML 被 validator 拒绝。
5. 最后补 T1/T2/T3 的脚本化验证与报告 schema。

## 6. 本次 review 用到的工具（都在 calibration-spec/，未改 calibrate/）

- `tools/validate_calibration.py@：契约校验（示例 0 error）。
- `tools/check_pair_response.py@：pair/group 合并响应与相位抵消判据（本地已用理想 LR4/反相/延迟合成数据自测：
  正例 PASS、反相/错延迟 FAIL）。
- 本文件 + CONTRACT.md/ACCEPTANCE.md 为设计侧基线；audio-stack 修完后建议把
  “示例生成 → validator → pair checker → 报告” 串成一个可复跑脚本。
