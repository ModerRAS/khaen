# khaen-bench

khaen 实时 DSP 链性能基线、host graph SIMD 派发对照与 kernel 级 SIMD 对照
（T38 基线 → T39a kernel SIMD → T56 host graph 接 SIMD 派发）。

## 用法

```
khaen-bench [选项]
  --json                  只输出机器可读 JSON（schema khaen-bench/v2）
  --quick                 快速模式：warmup=10 measure=60 repeats=2
  --lanes N               通道数 = lane 数（默认 22，最大 32）
  --block-frames N        每块帧数（默认 128）
  --sample-rate N         采样率 Hz（默认 48000；支持 48000 / 96000）
  --fir-taps N            crossover 每 lane 抽头数（默认 2048，最大 4096）
  --matrix-taps N         matrix 每格抽头数（默认 8）
  --factor N              matrix 抽取 / 插值因子（默认 16 = 48k/16 = 3 kHz）
  --active-lanes N        非恒等 matrix 子块边长（默认 16）
  --kernel MODE           graph 档位与 kernel 级对照的列：auto（默认，两列）/ scalar / optimized
  --warmup N              每档预热块数（默认 50）
  --measure N             每档每次重复的测量块数（默认 300）
  --repeats N             每档重复次数（默认 3）
  -h, --help              显示帮助
```

## 两类档位

1. **graph 档位**（`A-identity` / `B0-crossover96` / `B-crossover2048` / `B2-crossover4096` /
   `C-matrix3k` / `D-crossover+matrix3k` / `E-matrix-full-rate` / `C1-matrix3k-1tap` /
   `E1-matrix-full-rate-1tap`）：计时对象是 `PlanGraph::process` 每块耗时（DSP 链本体），
   预算 = 帧数 / 采样率（128 帧 @ 48 kHz = 2.667 ms）。同一方案用
   **`KernelChoice::Scalar`（标量参考内核，bit-exact 口径）与 `KernelChoice::Optimized`（core 的
   SIMD 派发入口）两列**分别编译，报告每列 p50/p99/min、budget 占比、加速比（spd = scalar /
   optimized）与 parity（两列输出的最大绝对差异）。用于回答"整条链离预算多远"和"host 接上
   SIMD 后整条链快多少"。
2. **kernel 档位**（`K-direct-crossover` / `K-direct-crossover4096` / `K-part-3khz` /
   `K-part-fft256` / `K-part-1024tap` / `K-part-4096tap`）：绕过 host 图，直接计时内核入口，
   同一 workload、同一输入下给出标量参考内核与 SIMD 内核两列 + 加速比 + parity。用于回答
   "内核本身被 SIMD 化后快多少"。

两类档位都在 `--kernel scalar` / `optimized` 下只测一列（另一列输出 `-`）；默认 `auto`
两列都测。本机实际派发后端见报告 `graph_kernel.backend` / `kernel_backend` 字段
（`scalar` / `avx2+fma` / `neon`，来自 host graph 的 `KernelDispatch` 真实探测值），
`graph_kernel.auto_path` 是 `KernelChoice::Auto` 在本机解析出的路径。

**读报告注意**：`parity ≠ 0` 且 `spd > 1` 才是该档位真实的 SIMD 收益。`parity = 0` 表示两列
走完全相同的实现（identity 档、matrix 档），那里的 `spd` 只反映测量噪声（应接近 1，min 列比
p50 稳；共享 Windows 桌面上 p50 会被抬升）。T56 只接了 crossover 的两条卷积路径（≤ 96 taps
的 direct 直算与 > 96 taps 的 partitioned 分区卷积）；matrix 段（含 3 kHz 低速率域的重采样器
与矩阵 FIR）目前仍是标量实现，`C/E/C1/E1` 与 `D` 的矩阵部分不会因 T56 变快（实测拆分与
"暂不接线"的依据见 `crates/host/src/graph/matrix.rs` 模块文档）。

## 计时口径

- 每档预热 `warmup` 块丢弃，再逐块 `Instant` 计 `measure` 块，重复 `repeats` 次取中位数；
  计时器空转开销已扣除。
- **不含**后端 capture/playout、线程调度、daemon 与设备开销；是链路成本的下界。
- 共享 Windows 桌面会抬升 p50（其它构建/OneDrive 干扰）；报告同时给 p99 与 min：min 是最安静
  单块，比 p50 更接近无争用能力。
- 只信实测：报告里 p50/p99/min 都来自本次运行；NOT_RUN 的项（真机 NEON/N150、端到端 xrun）
  不写成预测值。

## kernel 档位的 workload 构造

- `K-direct-crossover` / `K-direct-crossover4096`：镜像 `crates/host/src/graph/crossover.rs`
  的每 lane 一次 1-lane direct 内核调用（每 lane 一条 taps-tap A-FIR；g=[0]、p=[1]）。
- `K-part-3khz`：3 kHz 低速率域矩阵形态（B = chunk/factor、M 参与者 m×m G、共享 FFT 分区卷积）。
- `K-part-fft256`：FFT 主导形态（lane 数、L_F=8、Q=1，每块 2×lanes 次 K 点 FFT + IFFT）。
- `K-part-1024tap`：中等 taps（L_F=1031、默认 Q=9），FFT 与频域乘加各半的混合负载。
- `K-part-4096tap`：partitioned extended 口径（L_F = 1 + 8 + 4096 - 2 = 4103、Q = 33、
  64 条融合路由），镜像校准链 4096-tap P_post FIR 融合进 G 的形态。

两个大档位只在 `--fir-taps >= 1024`（默认完整规模）时启用，避免小配置（含 `cargo test`）
在 debug 构建下过慢。