# khaen-bench

khaen 实时 DSP 链性能基线与 kernel 级 SIMD 对照（T38 基线 → T39a kernel SIMD）。

## 用法

```
khaen-bench [选项]
  --json                  只输出机器可读 JSON（schema khaen-bench/v1）
  --quick                 快速模式：warmup=10 measure=60 repeats=2
  --lanes N               通道数 = lane 数（默认 22，最大 32）
  --block-frames N        每块帧数（默认 128）
  --sample-rate N         采样率 Hz（默认 48000；支持 48000 / 96000）
  --fir-taps N            crossover 每 lane 抽头数（默认 2048，最大 4096）
  --matrix-taps N         matrix 每格抽头数（默认 8）
  --factor N              matrix 抽取 / 插值因子（默认 16 = 48k/16 = 3 kHz）
  --active-lanes N        非恒等 matrix 子块边长（默认 16）
  --kernel MODE           kernel 级对照：auto（默认，两列）/ scalar / optimized
  --warmup N              每档预热块数（默认 50）
  --measure N             每档每次重复的测量块数（默认 300）
  --repeats N             每档重复次数（默认 3）
  -h, --help              显示帮助
```

## 两类档位

1. **graph 档位**（`A-identity` … `E1-matrix-full-rate-1tap`）：计时对象是
   `PlanGraph::process` 每块耗时（DSP 链本体，host 当前接的内核实现），预算 = 帧数 / 采样率
   （128 帧 @ 48 kHz = 2.667 ms）。用于回答"整条链离预算多远"。
2. **kernel 档位**（`K-direct-crossover` / `K-direct-crossover4096` / `K-part-3khz` /
   `K-part-fft256` / `K-part-1024tap` / `K-part-4096tap`）：绕过 host 图，直接计时内核入口，
   同一 workload、同一输入下给出
   **scalar（标量参考内核）与 optimized（运行时派发的 SIMD 内核）两列**以及加速比与 parity
   （两路径输出的最大绝对差异，证明同一语义）。用于回答"内核本身被 SIMD 化后快多少"。

kernel 档位在 `--kernel scalar` / `optimized` 下只测一列（另一列输出 `-`）；默认 `auto`
两列都测。本机实际后端见报告 `kernel_backend` 字段（`scalar` / `avx2+fma` / `neon`）。

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