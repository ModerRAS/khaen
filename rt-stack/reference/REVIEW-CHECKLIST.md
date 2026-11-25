# REVIEW-CHECKLIST — T03 人工 review 清单（reference-author → Lead）

2026-11-16 · 交付：`projects/tspi-audio-dsp/rt-stack/reference/`（team task-23）。
本文是 WBS T03 DoD 的**逐条可判检查表**：机器可判的条目给出命令/exit code/证据文件，人读条目给出手算/规范页码。
reviewer 只需按顺序勾选；任一 "FAIL" 即整包不进 PASS。

> 作者自检：rt-reference（2026-11-16，本机 Windows / Python 3.14.6 / 未触板/未 git commit）——**全部 PASS**。
> 独立 review：**Lead（待签）**。

---

## A. 输入是否冻结、hash 是否绑定

- [x] A1 权威文档在生成/验证前已读取：WBS T03 行 + §1 写域/§7 交接；DSP-HOST-DECISION 核心模型/RT/FIR/校准；CPU 预算 §5 的 F_ji 公式。
- [x] A2 所有外部输入记录 sha256/bytes：见 `evidence/T03/manifest.json` 的 `inputs_external`（WBS、DECISION、CPU-BUDGET、calibration CONTRACT、layout-presets、upmix profile、API、E2E-ACCEPTANCE、MIMO 计划/路线图）。
- [x] A3 14 个 fixture 的 sha256/bytes 在 `fixtures/index.json`，验证器逐字节核对（`T03-HASH-*` 检查）。
- [ ] A4 reviewer 复核：manifest 中 WBS/DECISION 的 sha256 与当前工作区一致（若 Lead 又更新了规划文件，hash 应变化并在 manifest 注明时间点；T03 任务行本身未变）。

命令/证据：`python tools/gen_fixtures.py --check`（exit 0）；见 `evidence/T03/manifest.json`。

## B. 规范（SPEC.md）是否完整、可判

- [x] B1 §1 范围/非目标：明确不做上混/下混、不定义 API/RT/测量/IIR；不是 Cargo 包。
- [x] B2 §2 模型：`A_map`（行/列至多一个 1，纯选择/零填）、`A_i` 输入列 FIR、`G=δ_D I+g` 嵌入、`P` 行对角、scatter 单射、factorized 与 fused 等价、行/列不可交换、identity baseline。
- [x] B3 §3 长度：`L_G=max(D+1,1+max_g_tap)`（D 计入）、`L_F=L_A+L_G+L_P−2`、`L_out=L_in+L_F−1`、尾/声明支撑、4096/8192/16384 运行时上限、流式 flush 契约与 block-size 不变性。
- [x] B4 §4 精度：exact（binary64 精确可表示、任意求和顺序安全）与 tolerance（`atol+rtol·max(1,max|ref|)`，C11=1e-12）；f32 informative 指引与 E2E-ACCEPTANCE 工程默认一致。
- [x] B5 §5 布局：4.1.2 表（FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1 role6→DAC7、ch6 空、participants [0,1,2,3,6]）、stereo_strict、高度全频无默认 HP/LP/18ms、layout-presets 为唯一权威（T57）。
- [x] B6 §6 夹具 schema/校验规则/标签矩阵；§7 工具与命令；§8 未来消费者契约；§9 版本与变更纪律。
- [ ] B7 reviewer 复核：B2/B3 公式与手算 H2/H5/H7/H9/H13 一致，B4 判据足够严格但不会误伤合理 f64 实现（可自行运行 verify）。

## C. 手算向量是否独立、可复现、覆盖 DoD

- [x] C1 `HAND-CALC.md` H1–H13 全部为小整数/二进制分式，逐项写出 G/F/卷积表与错误实现反例（H11 圆周混叠、H6 行列、H12 输入 FIR 输出侧）。
- [x] C2 机器解析：15 个 fenced json 块（handcalc 11 / handcheck 1 / handtrap 3），全部通过解析并与 fixture 逐值一致（检查 ID `T03-HAND-*`）。
- [x] C3 手算覆盖 offdiag（H3/H6/H7/H12）、tail（H4/H7/H9/H10）、D（H2/H5/H7/H9/H13）——验证器 `T03-HAND-TAGS` 强制。
- [x] C4 每个手算块声明的 supports 与 §3.1 公式一致（`l_f` 含 D 与 P/A 长度）。
- [ ] C5 reviewer 复核：任选 2 个 handcalc 块（建议 H5、H7）按正文重新手算一遍，数值应与 fixture expected 相同。

## D. 夹具数据

- [x] D1 14 个 case：C01/C02/C03/C04/C05/C06/C07/C08/C09/C10/C11/C12/C13/C14；每例含 input/expected/supports/comparison/streaming/traps（适用时）。
- [x] D2 覆盖：offdiag（8 例）、tail（7 例）、D（8 例）、identity（3 例）、stereo_strict/scatter（C08/C09/C13）、column_fir（C14）、five_by_five（C09）、dense（C12）、tolerance（C11）、streaming（C10；其余多数也声明流式自检）。
- [x] D3 验证器每例执行：schema/长度公式、exact 有理逐样本复算（独立实现）、f64 参考比较、所有声明 block_size（1/2/3/64/128）流式一致、**语义强制 tag 断言**（17 个；`baseline`/`random_exact` 为描述性索引，不单独断言，见 SPEC §6.5）、trap 反断言。
- [x] D4 结果：14 fixtures / 108 checks / 0 failures；报告 `evidence/T03/verify-report.json`。
- [ ] D5 reviewer 复核：确认 `traps` 的错误值确实与线性正确值不同（C04 [5,2,1,4] vs [1,2,1,4,4]；C06 [12,26] vs [6,35]；C14 输出侧顺序）。
- [x] D6 负向/公式校验：`python tools/verify_reference.py --negative-validation` 实际执行 12 项（8 类非法夹具必须被 validate_fixture 拒绝、3 个 trap 错误公式独立重建、1 个 handcheck 对 ref_f64 独立复查），exit 0；C04/C06/C14 错误值与公式重建值一致。

## E. 独立性与流程纪律

- [x] E1 三套实现互证：gen（多项式乘精确）、verify（逐样本精确直接卷积）、ref_f64（binary64 状态机）互不 import 核心逻辑（verify 仅 import f64 参考作为第二实现）。
- [x] E2 不依赖 T06/rt-check/Rust：无 Cargo 根、无 cargo test、无网络；只 Python 3 标准库。
- [x] E3 未触板：未运行 board.py、未开 ALSA、未改 CamillaDSP/S1/服务；未 git commit。
- [x] E4 写域：仅 `projects/tspi-audio-dsp/rt-stack/reference/`（含其 evidence 子目录）；未写其它目录、未建 Cargo.toml。
- [x] E5 证据纪律：manifest 记录输入/输出 hash、实际命令、cwd、exit code、未测项、假设、not_claimed；无 "只编译/0 测试" 声明，实际执行 108 项非零断言。
- [ ] E6 reviewer 复核：抽查 manifest 中 3 个输出文件的 sha256 与磁盘一致；确认无板端/服务改动。

## F. DoD 逐条结论（WBS T03 行）

| DoD | 结论 | 证据 |
|---|---|---|
| P0 独立 f64 参考规范 | PASS | SPEC.md §1–§9；无 T06 依赖 |
| 手算 impulse 向量与夹具数据 | PASS | HAND-CALC.md H1–H13；fixtures/cases C01–C14 |
| 含 offdiag / 尾 / D | PASS | tag 覆盖 + 手算覆盖（验证器强制） |
| 此时不依赖 T06 或未来 rt-check | PASS | gen/verify/ref 均标准库独立运行；无 Cargo 文件 |
| 文档解析卷积手算向量与输入 hash 人工 review | PASS（作者侧）/ 待签（Lead） | HAND-CALC 机器块解析 PASS；manifest 输入 hash；本清单 A/C 节 |

## G. 已知局限（不是隐藏项）

- 仅 P0 数学/夹具；T07/T08 等 Rust 消费者尚未实现，本包未被 cargo test 消费（未来任务必须实际跑 fixture，不能只看本清单）。
- 5.3.2/7.1.4/22ch 布局未单列 fixture（模型布局无关；4.1.2 与合成 2×2/5×5 已覆盖 DoD 要素）。
- C09 5×5 全量 expected 由精确复算验证，手算为 17 个代表样本点检；未把 25×6 个值全部人算展开。
- f32 门/跨架构 FMA 行为/96k/ASRC/IIR/limiter 不在本规范；不得把本包当这些项的 PASS 证据。
- 未做真实测量/房间效果/板端 RT；不影响 T03 的 P0 资料验收，但 P1 最终门仍需后续任务。

## H. 签收

| 角色 | 结论 | 日期/方式 |
|---|---|---|
| 作者 rt-reference | 自检 PASS（A–F 机器项全绿；C5/D5/E6 留 reviewer） | 2026-11-16，运行 `tools/make_evidence.py`（exit 0） |
| 独立 reviewer（Lead） | ☐ PASS ☐ FAIL（FAIL 请附检查项 ID 与证据） | 待签 |
| 后续动作 | 若 PASS：T03 完成；若 FAIL：按项回修后重跑 gen --check + verify + make_evidence | — |
