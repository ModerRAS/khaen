# calibrate/ 复审 Round 2（P0 修复核验，2026-11-15）

> 对象：audio-stack 修复后的 calibrate/（release 构建 @target/release/tspi-calibrate.exe@，
> 本地实测 sha256=@10b237528ba8d6cfb8eb488fc7462ff50b244a03b35698273ca3e10dbce8be63@，2026-11-15 18:03:55；源码 main.rs 18:02:54）。
> 方法：只读 review + 用 release 二进制复现 + 契约校验器交叉检查；未改 calibrate/。
> 上轮 review：@CALIBRATE-DRAFT-REVIEW.md@；契约/验收：@../CONTRACT.md@ / @../ACCEPTANCE.md@。

## 0. 结论

P0 五项**结构和入口已补齐**（period/buffer 参数、--pairs + pairs.json、measure-multi、polarity-offset、
UMIK 48k 限制），cargo test 11/11 通过，新示例 YAML 通过 validator。
但 P0-2 的验证实现有一个**判定逻辑错误**（极性双重应用），导致同一份校准 YAML 的验证结果取决于
测量相位的编码方式；另外 P0-1 只改了 playback（arecord 仍无 period/buffer），P0-3 的测量窗口存在
截断/偏移风险。这几项建议在板端 T1/T4 前修掉。

## 1. P0 核验结果

| P0 | 状态 | 证据/说明 |
| --- | --- | --- |
| P0-1 period/buffer | ⚠ 部分 | aplay 已参数化（@raw.rs:113-119@），默认 128/512，32..4096 + buffer≥period 检查；但 **arecord 仍未传 period/buffer**（@raw.rs:104-110@）。T5 要求 capture 也用 ≤128 帧；board 实测前补 arecord 参数并记录实际值 |
| P0-2 merge --pairs | ❌ 逻辑错误 | 入口/输出已实现（@main.rs:385-407@，pairs.json），N1/N2/N3/N5 单测通过；但 @channel_h@（@main.rs:498-506@）和 @phase_sum_margin@（@main.rs:203-231@）用的是 **实测 polarity** @meas.polarity@，而 FR 相位本身已包含物理极性 → 极性被乘两次。复现：同一份校准（FR polarity=invert），若 FR 实测相位=180°（真实反相测量的形态）→ pair/PASS；若 FR 实测相位=0°（合成简化）→ pair/FAIL。验证结果依赖测量相位是否含极性，而不是 YAML 是否包含修正。**修法**：验证时使用校准字段的极性（@cal.channels[k].polarity == "invert" ? -1 : +1@）乘到 H 上；N1 单测改成“实测反相 + 校准 polarity=normal → 必须 FAIL；校准 polarity=invert → PASS”。另外建议 --pairs 在给了 --crossovers 时必填（否则只剩全局 margin，无法覆盖 §6.1） |
| P0-3 measure-multi | ✅ 已加（⚠ 风险） | 一次 arecord + 槽位顺序扫频已实现（@main.rs:568-637@）。风险：arecord 先启动、sleep 350ms 后 aplay（@raw.rs:110-111@），而切窗用 @expected = pre + i*slot@ 后 @start=expected-0.2s@、尾部只留 0.5s（@main.rs:607-612@）——真实采集起点比播放时间轴晚 ~0.35s+驱动延迟，可能把扫频尾部截掉 0.15s 以上，且不同设备延迟会变。建议：整段 capture 先做能量/互相关检测出每个 sweep 的实际起点再切窗（或把尾部余量加到 ≥1s 并记录/补偿 350ms 偏移）；日志打印每通道检测到的 sweep 位置 |
| P0-4 polarity-offset | ✅ 已加 | measure/measure-multi/analyze 支持 @--polarity-offset normal|invert@；注意需先用 1:1 电学环回标定链路极性并在报告中记录；验证逻辑仍需按上面 P0-2 用“校准极性”而不是实测极性 |
| P0-5 UMIK 48k | ✅ 已加 | @--mic umik --rate != 48000@ 在 measure/measure-multi 直接报错（实测 rc=1）；建议 analyze 不涉及设备可不限制，README 写明 96k 需换麦克风/漂移补偿 |

## 2. P1 剩余（上轮已列，本次复核仍未改）

- @main.rs:331@ @reference_index = i@（文件循环下标）→ 应为 @channels[k].index@（m.channel）；文件排序/缺通道会错。
- @main.rs:252@ fit 单通道 @delay_samples@ 仍写原始延迟，未归一化 min=0；单独使用会被 validator 判非法。
- @main.rs:239/329/624@ 默认名仍为 @ch{index}@ 小写，不符合角色 id 正则（FL/SUB1…），且无法映射 layout-presets。
- @peq.rs:13@ Biquad clamp 仍是 freq 10..22000、q≤20，超出契约/渲染器（freq≥20、q≤10）；拟合输出可能被 validator 拒绝。
- @main.rs:268-302@ CamillaDSP 片段仍未实现极性（只有 channelmap_roles 提示）；独立配置消费方需自行补 Gain -1/mixer inverted。
- @main.rs:222@ @phase_sum_margin@ 仍把 raw delay 与归一化 delay 相加（@dly = delays[i] + m.delay_samples@）；修复极性时一并改成“只加校准修正后的总延迟”。
- @main.rs:514-566@ pair 检查：ripple 用 5 点滑动（≈0.2 oct）而非 1/3 oct；@level_diff_db@ ≤3dB 是额外的硬 FAIL 条件（ACCEPTANCE 未要求），可能造成交叉点两侧设计电平不同的误报；@target_dev@ 与 ripple 语义重复。建议：ripple 按 1/3 oct，level_diff 降为 warning 或做成可选阈值。
- @dsp.rs:171-173@ SNR 仍是 IR 尾部粗估，未做 200 Hz–8 kHz 带限、未做 ≥3 次重复/中位数；T2 要求 SNR≥25 dB 时拒绝该次测量。
- @main.rs:374-383@ 报告仍是 @tspi-calibration-validation/v1@，与 ACCEPTANCE §8 的 @tspi-calibration-report/v1@（pairs/负向用例/环境/文件 sha256）不一致。
- @raw.rs:91-127@ 测量仍是“sleep 350ms 后播”的开环同步；无 capture 实际起点记录，建议至少把 arecord/aplay 的 period/buffer/启动时间写进分析 JSON。
- @main.rs:130-149@ 单通道 measure 只建议仅用于 T1；README 已写，OK。

## 3. 外部验收工具（我的侧）

- @calibration-spec/tools/validate_calibration.py@：你的新 YAML 示例仍 0 error/0 warning。
- @calibration-spec/tools/check_pair_response.py@：需要“相位含绝对延迟”的 FR CSV 约定；你当前 @*.fr.csv@ 的 phase 是 Farina 对齐段内相位（粗延迟已去掉），外部无法直接重建 pair 相位。
  建议在 analysis JSON 增加 @fr_phase_ref: "sweep_start"@ + 总延迟（已有 delay_samples），或导出复频响（CSV 增加 re/im 两列，或 .npz/JSON complex）；这样外部 checker/复测脚本可独立验证。

## 4. 建议的修复顺序

1. P0-2 极性：验证用校准 polarity；补“实测反相+校准 normal → FAIL / 校准 invert → PASS”单测；--crossovers 时强制 --pairs。
2. P0-1 arecord period/buffer + 记录实际值；P0-3 切窗改为检测式/加大余量。
3. P1 前四项（reference_index、fit 归一化、角色名、PEQ clamp）——都是小改，能避免 YAML 被 validator 拒绝。
4. 报告 schema 与 FR 导出约定统一后，板端按 ACCEPTANCE T1/T4 跑；我可用 checker 交叉验证。
