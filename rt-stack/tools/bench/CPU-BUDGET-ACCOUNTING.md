# 四 worker 静态 DAG CPU 预算核算（T29/T46/T30 性能门禁配套）

> 任务：team task-156。输入（只读）：DSP-HOST-CPU-BUDGET.md §1/§2/§3/§10.2、
> DSP-HOST-DECISION.md §4/§7、WBS T29/T46/T30、T56 schedule_plan（crates/core/src/schedule_plan.rs）、
> T09 workers（crates/core/src/workers.rs）、T20 swap（rt-host/src/swap.rs）、T45 scheduler。
> 本文件把所有数字分成三类并显式标注：
> **[MODEL]** 纯算术模型（可复算，不是实测）；**[LOCAL-X86]** 本机离线实测（非板端、非 RT）；
> **[BOARD-ONLY]** 只能板端取数，当前 NOT_RUN。
> 本机实测原始数据：../../../evidence/perf-harness/local-x86-run1、local-x86-run2。

## 1. 运算量预算（[MODEL]，与 DSP-HOST-CPU-BUDGET 第 2/10 节一致）

口径：fs=48000 Hz；4.1.2 = 7 角色 / 8 容器 / 5 participants（FL FR SL SR SUB1），
稠密低带路径 P=25；f32 实 MAC = 2 FLOP。

| 项 | 公式 | 值 |
| --- | --- | --- |
| 矩阵 MAC/sample | 25 × 4096 | 102,400 |
| 矩阵 GFLOP/s | 2 × 102,400 × 48,000 | 9.8304 |
| 输出对角 FIR P（1024 tap × 7 角色）MAC/sample | 7 × 1024 | 7,168 |
| 对角 FIR GFLOP/s | 2 × 7,168 × 48,000 | 0.688128 |
| 完整 4.1.2/4096 图 GFLOP/s（矩阵+对角） | 2 × 109,568 × 48,000 | 10.518528 |
| schedule_plan work/block（B128，含 A/reduce/protect/scatter 小项） | — | 14,031,872 MAC |
| schedule_plan 声明 FLOP/s（B128） | 2 × 14,031,872 × 375 | 10,523,904,000 |
| 4 核平均每核算术（仅矩阵） | 9.8304 / 4 | 2.4576 GFLOP/s |
| 8–15 GFLOPS/核持续假设下的算术占用 | 2.4576 / 8–15 | 16.4–30.7% |
| 6/6/6/7 最忙核矩阵（7 路径）每 128 帧 | 7×4096×128 = 3,670,016 MAC = 7,340,032 FLOP | 8 GFLOPS → 0.9175 ms；15 GFLOPS → 0.489 ms |

结论：**纯算术模型下** 6/6/6/7 + 8 GFLOPS 的 128 帧矩阵最忙核约 0.9175 ms，加上其余图
与同步后仍可能守 1.3333 ms；这是 CPU-BUDGET §2.1 的既有结论，不是实测。

## 2. schedule_plan 静态 DAG deadline 准入（[MODEL]，本机工装真实计算）

口径：DeadlineBudgets::with_core_rate(r)（r 为每核 f32 MAC/s，1 MAC=2 FLOP），门 = 50% block
= 1,333,333 ns（128 帧）/ 666,666 ns（64 帧）；total_worst_ns = sum over phases of phase 最坏
worker 估计。数据见 evidence/perf-harness/*/bench-budget-deadline.csv（每个 profile × 5 档算力）。

| profile（48k） | 声明 FLOP/s | 4 GMAC/s (8 GF) worst | 7.5 GMAC/s (15 GF) worst | 门 | 判定 |
| --- | ---: | ---: | ---: | ---: | --- |
| B128 direct factorized 4w | 10.524 G | 983,168 ns | 524,355 ns | 1,333,333 | ADMITTED（全档） |
| B64 direct factorized 4w | 10.524 G | 491,584 ns | 262,177 ns | 666,666 | ADMITTED（全档） |
| B128 direct factorized 2w | 10.524 G | 1,966,304 ns | 1,048,694 ns | 1,333,333 | 8/9.23/10 GF 拒绝；12/15 GF 才 ADMITTED |
| B128 direct factorized 1w | 10.524 G | 3,507,968 ns | 1,870,914 ns | 1,333,333 | REJECTED（全档） |
| B128 direct 8192 4w | 20.354 G | 1,900,672 ns | 1,013,690 ns | 1,333,333 | 8/9.23/10 GF 拒绝；12/15 GF 边界 ADMITTED |
| B128 hybrid 8192 4w | 1.340 G | 118,416 ns | 63,154 ns | 1,333,333 | ADMITTED（全档） |
| B128 shared-FFT 4096 4w | 0.332 G | 28,360 ns | 15,124 ns | 1,333,333 | ADMITTED（全档） |
| B64 shared-FFT 4096 4w | 0.644 G | 27,120 ns | 14,462 ns | 666,666 | ADMITTED（全档） |

关键读数：
- schedule_plan 的 4w 最坏任务 LPT 分配是 3,932,672 / 3,932,672 / 3,932,544 / 2,233,984 MAC，
  最忙≈3.93M MAC；该数字**只覆盖 25 条真实路径 + 其余图小项**。
- 2 核 direct 在 8 GFLOPS 下 1,966,304 ns > 门，与 WBS T46 “2 核是独立 profile、需换 FFT/
  更短 profile”一致；12 GFLOPS 才勉强过（1,310,868 ns），板端不可依赖。
- 8192 direct 在 8 GFLOPS 下 1,900,672 ns > 门；hybrid（矩阵走 FFT）则大幅余量 → 与
  CPU-BUDGET §10.2 判读一致。

## 3. 当前 workers.rs DAG 的真实执行量与静态均衡（[LOCAL-X86] + [MODEL]）

重要偏差：上面 §2 的 3.93M MAC 是 schedule_plan 的 25 路径模型；**实际 workers.rs 内核
B2 对 7 个输入列 × 7 个角色全部跑满 l_g tap**（非 participant 用 0 系数仍走循环）：

| 项 | 值 |
| --- | --- |
| B2 实际列-角色对 | 7 × 7 = 49（预算模型 25 条） |
| B2 dense MAC/block（B128, l_g=4096） | 49 × 4096 × 128 = 25,690,112 |
| 计划矩阵 MAC/block（25 路径） | 25 × 4096 × 128 = 13,107,200 |
| dense 膨胀系数 | **1.96×** |
| B1（7 角色 ×1 tap ×128） | 896 MAC |
| B3（7 角色 ×1024 tap ×128 + reduce） | 917,504 MAC 量级 |
| dense 合计/block（近似） | ≈26.6M MAC |

静态 lane 分配（workers.rs：k_lane[k]=k×4/7 → [0,0,1,1,2,2,3]；lane_worker[lane]=lane%W）：

| worker 数 | B2 列数 | B1/B3 角色数 | 最忙/最闲 ratio | 纯算术 speedup 上限 |
| --- | --- | --- | --- | --- |
| W=1 | 7 | 7 | 1.0 | 1.0 |
| W=2 | 4 / 3 | 4 / 3 | 1.33 | ≈3.0（49/16.5 vs 理想 4；实际扣 B3 略不同） |
| W=4 | 2 / 2 / 2 / 1 | 2 / 2 / 2 / 1 | **2.00** | **≈3.5**（49/14） |

[LOCAL-X86] 实测与本模型一致（run1 = 96 blocks × 3 reps，run2 = 64 blocks × 3 reps；
共享开发机，CPU 占用 18–100% 波动，绝对数只作趋势）：

| 配置 | p50（run1 / run2） | 每 worker load（busy/wall） | load_cv | load_range |
| --- | ---: | --- | ---: | ---: |
| B128 W2 | 15.26 / 16.32 ms | 0.999/0.733 与 0.998/0.752 | 0.140–0.153 | 0.280–0.307 |
| B128 W4 | 8.99 / 11.28 ms | 0.892/0.906/0.930/0.472 与 0.903/0.921/0.940/0.461 | 0.237–0.248 | 0.572–0.594 |
| B64 W4 | 5.43 / 5.69 ms | 0.906/0.876/0.887/0.464 与 0.911/0.902/0.908/0.449 | 0.235–0.250 | 0.564–0.583 |

⇒ per-worker load 稳定呈现 0.89–0.94 三核 + 0.45–0.47 一核的 2:1 形态；
**不满足 T46 工程默认 CV≤0.15、range≤0.30**（长滤波 steady）。2:1 的 B2 列分配把纯算术
speedup 上限压到 3.5×，叠加 barrier/调度/非 RT 主机后两轮 speedup4 实测 1.95–3.00
（best-rep 2.09–3.01），全部 <3.2（非板端门；B64 一窗 3.00 仍不达标）。

## 4. 门可行性核算（[MODEL] 从实测/预算推出的板端必要条件）

按当前 dense kernel，最忙 worker 的每块 MAC（B128）：
B2 最忙 2 列 = 2×7×4096×128 = 7,340,032；B1≈256；B3≈2×1024×128 + 归约 ≈ 262,144+；
合计 ≈ **7.60M MAC/block**。要 worst<1,333,333 ns，需要最忙核持续 ≥ **5.70 GMAC/s = 11.4 GFLOP/s**
（且尚未计 barrier、I/O、cache、热降频）。按 CPU-BUDGET 的 8–15 GFLOPS 假设：
- 8 GFLOPS → 1.90 ms 超门（FAIL 预期）
- 12 GFLOPS → 1.27 ms 勉强过（余量 5%）
- 15 GFLOPS → 1.01 ms 过（余量 24%）
若先做 §3 的两项修正（B2 稀疏到 25 路径 + 6/6/6/7 或 LPT 均衡），最忙核 3.67M MAC →
8 GFLOPS 下 917,504 ns（余量 31%），12 GFLOPS 下 611,669 ns；此时 T46 的 3.2× 需要
barrier/reduce 额外成本 < 最忙工作 ~8.6%（因 3.5× 上限；若进一步细分到 25 等长 path tile
则上限回到 3.571×，允许 ~11.6% 开销，与 CPU-BUDGET §10.1 一致）。

结论（板端前必须做，均为 NOT_RUN 的改良项）：
1. B2 跳过非 participant 列/角色（或按 path tile 而非列 tile），消除 1.96× dense 膨胀；
2. 重新分配 B2/B1/B3 tile，使 W=4 每 worker 工作≈25%（6/6/6/7 或 tap-tile），把 CV/range
   降到 T46 门内，并把 speedup 上限从 3.5× 抬回 ≥3.571×；
3. 若仍用长矩阵 direct，必须先在板端实测每核 sustained GFLOPS；<11.4 GFLOPS 时 direct 4w
   在 128 帧门内不可行，需 shared FFT / hybrid（§2 预算显示余量充足）；
4. FFT 四 worker 计划（输入 FFT/频点 tile/输出 IFFT 分派）尚未在 core 实现（T08 只有
   scalar partitioned；T56 只做静态计划），板端 FFT 路径需 T08/T26/T56 follow-up，NOT_RUN。

## 5. 内存与热切/回退预算（[MODEL]，本机工装计算）

| 项 | 值（48k/128/4.1.2 direct） | 备注 |
| --- | ---: | --- |
| 系数 bytes | 438,300 | A dense + 25×4096 + 7×1024 f32 |
| 表 bytes | 1,472 | header/path/diag/role 64B 对齐 |
| 历史 bytes | 143,304 | 7×(4095+1023) samples f32 |
| scratch bytes | 14,336 | 4 worker × B × O × f32 |
| plan_bytes | **597,504** | 对齐合并 |
| dual_graph_bytes | **1,195,008** | 热切双图 RAM 峰值下限 |
| hot-swap crossfade 峰值 CPU | 2,666,667 ns（= 一个 block 窗口） | 旧+新图最坏和 1,966,336 取 max |
| hot-swap 判定 | crossfade, admitted=true | 需 D 对齐且 crossfade_frames=128 |
| G=I fallback plan_bytes | 72,640 | 保留 P_post 保护；admitted=true |

与 T20 swap.rs 的对应：crossfade 准入要求同 rate/N/O/B、total_delay_q16 严格相等、
crossfade_frames==B≤128、peak RAM ≥ 双图下界、peak CPU ≤ 可用量；预算不足必须
ERR_CROSSFADE_BUDGET(123)/RT_E_CROSSFADE_ALIGN(309) 拒绝并走硬切+静音或新 epoch。
**冷启/切换真实恢复测量属 T30 板端项：NOT_RUN。**

## 6. 本机实测参考（[LOCAL-X86]，非板端，禁止当门）

| 场景 | p50（run1 / run2） | max（本机抖动） | 说明 |
| --- | ---: | ---: | --- |
| DAG matrix_prod B128 W1 | 23.89 / 22.03 ms | 34.70 / 30.76 ms | dense 49 列-角色，x86 scalar |
| DAG matrix_prod B128 W4 | 8.99 / 11.28 ms | 12.65 / 18.97 ms | speedup4 2.657 / 1.953（best-rep 2.481 / 2.085） |
| DAG matrix_prod B64 W4 | 5.43 / 5.69 ms | 7.42 / 24.66 ms | speedup4 3.001 / 2.930（run2 max 受外部干扰） |
| direct_block_f32 单线程 B128 | 17.30 / 15.40 ms | 22.16 / 21.25 ms | 同图；无 B1/B2/B3 分阶段/归约 |
| partitioned_block_f32 单线程 B128 | 0.168 / 0.163 ms | 0.260 / 0.762 ms | 同图 FFT，约 90–120× 快（算法不同） |
| DAG diag_p1024 B128 W1→W4 | 0.76→0.27 / 0.85→0.32 ms | 2.27/1.59 与 2.78/0.49 ms | 输出 FIR 段约 2.7–3.2× |
| harness 自检 B128/B64 | 0.581/0.184（run1）、0.650/0.186（run2）ms | p99 0.77–0.85（B128）ms | 门计算链 PASS（判据用 p50） |

本机环境：AMD Ryzen 7 7840HS（8 物理核/16 逻辑核）、Windows 11 26200、非 RT、
无 CPU 亲和/定频/PMU；运行窗口内另有 VBoxHeadless VM 与其他并行 agent 会话共享 CPU
（18–100% 波动，见 evidence/perf-harness/HOST-LOAD-CONTEXT.txt）→ max/CV 只作工装证据，
跨窗口绝对值不可比。bit-exact：两轮 W1 vs W4 输出哈希均 a2cf3cc1ed4570b0（PASS），
说明工装真实执行且确定性一致。

## 7. NOT_RUN（本核算未覆盖，禁止当已验证）

- 真实 A72 每核 sustained GFLOPS/MAC/s：NOT_RUN（本机 x86 与 RT 内核不同）。
- 板端 1/2/4 同 kernel speedup、CV/range、busy_compute/phase/worst：NOT_RUN。
- 板端 128 帧 worst<1.3333ms、64 帧<0.6667ms、1h 0 xrun：NOT_RUN。
- 板端 RT 延迟/IRQ/cyclictest/osnoise、热降频、cache/PMU：NOT_RUN。
- FFT 四 worker 实现与实测：NOT_RUN（kernel/集成 follow-up）。
- 真实校准生产图与真实 ALSA/MOTU 链：NOT_RUN。
