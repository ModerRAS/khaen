# MIMO FIR 板载 benchmark（RK3576 泰山派3M，RT baseline 6.1.99-rt36）

> 2026-11-15，CamillaDSP 4.1.3 aarch64 官方二进制；负载 capture=WavFile 10s 多声道 WAV、playback=File(/dev/null 等价 raw)；chunksize 64；每条 15s 观察窗，processed = out.raw 字节 / (M*4*fs)。
> 环境：Linux TaishanPi-3M 6.1.99-rt36 #3 SMP PREEMPT_RT；`/sys/kernel/realtime` 存在；cyclictest（Lead 实测）idle max 1–2 µs、hackbench 满载 avg 3–4 µs / max 7–13 µs。

## 结果（单线程默认）
| 矩阵 | taps | 观察窗内处理音频 | 实时因子（processed/15s） | 结论 |
| --- | --- | --- | --- | --- |
| 2×10 | 512 | ~5.8 s | 0.39 | 不到实时（单核 100%） |
| 2×10 | 1024 | ~3.9 s | 0.26 | 不到实时 |
| 2×10 | 2048 | ~2.4 s | 0.16 | 不到实时 |
| 12×10 | 512 | ~0.2 s | 0.01 | 远低于实时（~50–100× 不足） |
| 12×10 | 1024 | ~0.1 s | 0.01 | 同上 |
| 12×10 | 2048 | ~0.1 s | 0.00 | 同上 |

## multithreaded: true / worker_threads: 4（快速对照）
| 矩阵 | taps | 实时因子 | 处理负载 | 结论 |
| --- | --- | --- | --- | --- |
| 2×10 | 512 | 0.67（10 s 文件在 15 s 窗内基本跑完） | 150% | 仍不到实时，多线程改善有限 |
| 12×10 | 512 | 0.02 | 4435% | 远不可用 |

## 结论（重要）
1. **CamillaDSP 的 N×M 展开（每路径一个 Conv 滤波器）在 RK3576 上不可行**：12×10×512 @48k 即远超实时（实测 ~50–100× 不足）；2×10×512 单线程 0.39×、4 线程 0.67×，仍达不到稳定实时。
2. 原因估算：每路径独立 FFT Conv 的开销随路径数近似线性（120 路径×512 taps ≈ 13.8 GFLOP/s FFT 路径量级），单核/4 核 A72 跑不动；与 x86 估算一致但板上更差。
3. **需要自定义 Rust MIMO FIR 引擎**（共享 FFT 计划、partitioned convolution、SIMD、多线程/绑核，或直接 FIR+SIMD+多核），不能靠 CamillaDSP 展开交付 ART 式全 MIMO；下一步做 Rust 引擎原型（2×10/12×10 × 512–4096）对照。
4. 2×10（PC 立体声）也需优化/多线程+降 taps 才可能实时；低延迟 32 帧下 CPU 余量更紧。

## 备注
- 本次为 file backend 原型（非 ALSA 实时链路），数字用于相对比较；正式验收应在 ALSA loopback/USB 声卡 + xrun/cyclictest 下复测。
- 观测窗 15s、文件 10s；12×10 慢到 15s 只处理 0.1–0.2s，`timeout` 截断，实时因子为下限估计（实际更差）。
