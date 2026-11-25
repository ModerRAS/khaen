# rt-stack/tools/bench —— KHAEN RT-first DSP 离线性能工装

> 任务：team task-156（WBS **T29 / T46 / T30** 的性能门禁准备）。写域：本目录 + evidence/perf-harness/。
> 状态（2026-11-17）：**离线工装已落地并在本机 x86 真实执行**（executed>0，CSV/JSON 见
> evidence/perf-harness/）；**板端 RT/IRQ/cyclictest/osnoise、真实 ALSA/MOTU、A72
> 1.3333ms/0.6667ms/3.2x 与 1h soak 全部 NOT_RUN**。x86 数字只是工装/趋势参考，不是板端门。

## 1. 这里有什么

| 文件 | 用途 |
| --- | --- |
| src/perf_harness.rs | 单文件 Rust bench 工装（[NONRT]）：DAG 1/2/4 worker wall、尾值、per-worker busy/barrier/CV/range、direct/partitioned 单线程参考、T56 schedule_plan CPU 预算核算、gates 判定、CSV/JSON 输出 |
| Cargo.toml.template | 临时 staging crate 模板（**不在 rt-stack 内建 Cargo.toml**） |
| run-bench.ps1 | 生成 staging crate → cargo build --release --offline → 真实运行 → 写证据（环境/日志/CSV/JSON） |
| CPU-BUDGET-ACCOUNTING.md | 四 worker 静态 DAG 的 CPU 预算核算（与 DSP-HOST-CPU-BUDGET/DECISION 对齐）+ 本机实测偏差 |
| BOARD-MEASUREMENT-RUNBOOK.md | 板端测量 runbook（T29 RT/IRQ/cyclictest、T46 1/2/4、T30 soak、冷启/切换恢复、判据/失败处理） |
| ../../../evidence/perf-harness/ | 真实运行证据：两轮 local x86 run 的 CSV/JSON/日志/环境 + MANIFEST/哈希 |

## 2. 为什么不在 rt-stack 内加 crate

rt-stack/README.md §1/§3 冻结：**rt-stack 只允许唯一 Cargo 根**（rt-stack/Cargo.toml），
新 crate/Cargo.toml 只能由 integrator 串行合入（T06/T26/T32），leaf 不得另建。T47
（crates/bench）是 T46 的正式依赖，但属于另一 leaf 的写域；本任务不能改它。

因此工装采用 **staging crate**：run-bench.ps1 在 %TEMP% 下复制 src/perf_harness.rs →
src/main.rs + 生成 Cargo.toml（[workspace] 独立，path 依赖只读引用 rt-stack/crates/core），
cargo build --release --offline 后运行。仓库内**没有**新增任何 Cargo 根/依赖/构建产物；
staging 目录与 target 都在 %TEMP%（R: 内存盘，重启即弃）。后续若 T47 落地，可把本工装的
bench 场景迁进 crates/bench（需 integrator 合入）。

## 3. 运行（本机 x86 离线）

~~~powershell
# 在 rt-stack/ 或任意目录；OutDir 可相对/绝对
powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench/run-bench.ps1

# 指定证据目录/样本数（推荐正式跑法：3 reps）
powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench/run-bench.ps1 -OutDir evidence/perf-harness/local-x86-run1 -Blocks 96 -Warmup 8 -Reps 3

# 快速冒烟（16 blocks / 1 rep）
powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench/run-bench.ps1 -Quick
~~~

前置：cargo/rustc 在 PATH；首次离线构建 crates/core 约 6 s（staging target 空时；本机实测）。
不需要板端、不需要 libasound（Windows 不编译 alsa target-specific 依赖）。

退出码：0 = 构建+运行成功（**不代表任何板端门通过**）；非 0 = 构建/运行失败。

## 4. 输出（写到 -OutDir）

| 文件 | 内容 |
| --- | --- |
| bench-kernel.csv | DAG（worker_phase_b1/b2/b3 真线程）每 (graph, block, workers) 一行：samples/min/p50/p95/p99/p99.9/max/mean/CV、speedup(T1/T4、best-rep)、busy_fraction、barrier_wait_fraction、per-worker load/CV/range、max_phase_ns、planned vs dense-executed MAC、gate_verdict、executed_blocks、tiles/block、output_hash |
| bench-scalar.csv | 单线程 direct_block_f32 与 partitioned_block_f32 参考（同 graph/block） |
| bench-budget-deadline.csv | T56 schedule_plan 静态 DAG deadline 准入：每 profile × 每核算力假设（8/9.23/10/12/15 GFLOPS→4.0–7.5 GMAC/s）一行：work/block、MAC/s、FLOP/s、total_worst_ns、per-worker work、plan/dual_graph 字节、串行比例、admitted |
| bench-budget-swap-fallback.csv | hot-swap crossfade 双图 CPU/RAM 预算 + G=I fallback 预算 |
| gates.json | 门清单：哪条是本机参考（PASS/UNDER/OVER）、哪条是 harness 自检、哪些是 NOT_RUN_BOARD_ONLY |
| summary.json | 环境/配置/执行计数/模型常量/NOT_RUN 清单 |
| environment.json / toolchain.txt | 机器/OS/CPU/rustc/cargo/staging path |
| cargo-build.log / run.log / stage-path.txt | 构建与真实运行日志（UTF-8） |

## 5. 场景与图形（与 T09/T56 同构，不是校准系数）

- **matrix_prod_g4096_p1**：4.1.2（7 角色/8 容器/5 participants=FL,FR,SL,SR,SUB1）+
  G=m×m×4096 稠密系数 + P=每角色 1 tap（总 fused 支撑 4096，恰好 direct/workers 上限）；
  block 128 与 64 × worker 1/2/4。这是当前可执行 kernel 能表达的“最重矩阵”形状。
- **diag_p1024_g1**：G=1 tap（identity 基线）+ 每角色 1024 tap 对角 P；隔离输出 FIR/归约成本。
- **gate_selfcheck_g256 / g128**：缩小 taps（G=256@B128、G=128@B64）用于验证“门计算链路”
  在本机非 RT 环境能判 PASS（**不是生产性能结论**）。
- **bit-exact**：同输入/系数/block 下 W=1 与 W=4 输出 FNV-1a 哈希必须相等（T09 确定性
  契约），本机两轮均 PASS（a2cf3cc1ed4570b0），证明工装没有静默跑错图。
- 系数是有界伪随机占位值（±0.002 等），只为性能；**不是**任何真实校准/目标曲线。

## 6. 线程/DAG 复刻与测量口径（重要）

- 线程循环用 T09 的 [RT] phase 函数 + KhaenBlockAckV1 phase 位图 + wait_phase_done
  有界 spin：worker 每块 B1→ack→barrier→B2→ack→barrier→B3→ack→barrier；master 在块边界
  清零 partials、发布 block_start，并测“发布 → 全部 all-ack”的块完成时间（含块内串行准备）。
- 共享 arena（state/partials/input/output）用 raw pointer 在 worker 间传递，与真实 host 的
  固定 arena 同语义（phase 间 ack 同步，同 phase 内各 worker 只写 plan 归属的不相交区域）。
  **这不是 RT 安全证明**，只是离线工装的共享内存模型；生产 RT 走 T26 host。
- busy_ns 只累计 phase 函数调用时间（不含 barrier 等待/spin），符合 core-v1 §8.4
  “spin 不算 busy_compute”；busy_fraction = 各 worker busy/块 wall 的均值；
  barrier_wait_fraction = 1 − max(worker busy/wall)（最忙 worker 之外的等待占比）。
- 本机**没有**：RT 内核、FIFO/亲和实写、IRQ 隔离、温控/定频、PMU；Windows 调度抖动
  会放大 max/CV（两轮 run1/run2 的 max 离散即为证据）。所以 x86 的 max 不能当板端 worst，
  只能验证工装链路与暴露静态负载/算法问题（见 §7）。

## 7. 本机两轮结论（详见 evidence/perf-harness/MANIFEST.md 与 HOST-LOAD-CONTEXT.txt）

> 本机是共享开发工作站（AMD Ryzen 7 7840HS，8 物理核/16 逻辑核；另有 VBoxHeadless VM 与
> 其他并行 agent 会话），CPU 总占用在 18–100% 间波动。两轮 run 内部 run_quality=OK
> （同一 run 内 rep_p50 离散 ≤25%），但跨窗口绝对时间差异大；以下为结构性结论，
> 绝对数只作趋势，**板端门仍全部 NOT_RUN**。

1. **静态 lane 负载不均（真实 kernel 疾病）**：workers.rs 的 k_lane=[0,0,1,1,2,2,3] +
   lane_worker=lane%W 使 B2 列分片在 W=4 为 **2/2/2/1**（2:1 不均）、W=2 为 4/3（1.33:1）。
   实测 per-worker load（busy/wall）W4 = 0.89–0.94 / 0.90–0.92 / 0.91–0.94 / 0.45–0.47，
   W2 = 0.99–1.00 / 0.73–0.78；load_cv 0.235–0.250、range 0.56–0.59，
   **不满足 T46 工程默认 CV≤0.15/range≤0.30**。纯算术 speedup 上限 49/14=**3.5×**（不是 4×）；
   两轮实测 speedup4 = 1.95–3.00（best-rep 2.09–3.01），**全部 < 3.2**（B64 有一窗 3.00，
   仍不达标且非板端门）。
2. **B2 是 dense 49 列-角色对，而预算模型是 25 路径**：当前 kernel 对 7 列 × 7 角色都跑满
   l_g tap（非 participant 用 0 系数），每块 dense MAC 25,691,904 vs 计划 13,108,992
   = **1.96×**；这是 CPU 预算表（9.8304 GFLOP/s）之外的真实膨胀，板端门之前应先把
   participants/路径稀疏化或换 tile 计划。
3. **FFT 内核在本机同图快约 90–120 倍**（partitioned B128 p50≈0.14–0.17 ms vs direct
   p50≈15.4–17.3 ms，均含相同 4.1.2 图）；与 CPU 预算 §10.2 “8192/16384 direct 不可守门、
   需共享 FFT/hybrid”一致。partitioned 是单线程 scalar 参考，不是四 worker 生产实现。
4. **budget 模型与 T56 一致**：4.1.2/4096 direct/B128 在 4 GMAC/s（8 GFLOPS）下
   total_worst=983,168 ns < 1,333,333 ns 准入；15 GFLOPS 端 524,355 ns；64 帧 491,584 ns
   < 666,666 ns；2 核 direct 在 8 GFLOPS 拒绝（1,966,304 ns），12 GFLOPS 才准入
   （1,310,868 ns）→ 与 WBS T46 “2 核需换 profile”一致。
5. **direct 与 DAG 单线程开销差**：同窗口 DAG W1（p50 16.3–23.9 ms）比 direct 单线程
   （p50 15.4–17.3 ms）慢约 5–40%（B1/B2/B3 分阶段 + partials 归约 + barrier；窗口负载
   影响大）；四 worker 前先吃掉一部分收益。这属于当前实现事实，需板端再核。

## 8. 边界（NOT_RUN，禁止当已验证）

- 板端 RT/IRQ/cyclictest/osnoise 100us 工程门（T29）：NOT_RUN（需独占窗口+RT 内核）。
- 板端同 kernel 1/2/4 wall speedup≥3.2 + CV/range + busy_compute 正式验收（T46）：
  NOT_RUN；本机 x86 只是工装/趋势。
- 板端 128 帧 worst<1.3333ms、64 帧<0.6667ms、四 worker 1h 0 xrun（T30）：NOT_RUN。
- 真实 ALSA/MOTU/CueMix/PTP/AoIP、真实校准系数图、冷启/切换真实恢复：NOT_RUN。
- PMU cache/cycles/迁移、内核亲和/FIFO/IRQ 实写、热控/定频：NOT_RUN。
- git：未执行任何 git 操作（任务纪律）。
- 板端证据目录请由 e2e-runner 在授权窗口按 BOARD-MEASUREMENT-RUNBOOK.md 落盘；
  T29 的 plans/rt-window.md 由 T29 owner 按该 runbook 生成（本任务写域不含 plans/）。
