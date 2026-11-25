# tspi-mimo-fir —— ART-like 全矩阵 MIMO FIR 引擎原型（task-12）

> 目标：替代 CamillaDSP 逐路径展开（板载实测不可行，见 ../mimo-bench/BOARD-BENCH-RESULTS.md）。
> 本原型先做离线 benchmark（合成信号/合成系数），验证 partitioned FFT + 多线程在 RK3576 上的实时性；不接 ALSA/CamillaDSP 全链路。

## 实现
- 语言：Rust（`realfft` + `rustfft`，f32）；单文件 `src/main.rs`。
- 模式：
  - `--mode fft`：uniform partitioned overlap-save；输入频谱每块只算 N 次 FFT，输出累加 N×M×P 次复数乘，最后每输出 1 次 IFFT；part/block 默认 128。
  - `--mode direct`：时域直接卷积（对照用，慢）。
- 多线程：按输出通道并行（`--threads`）；可用 `taskset` 绑 A72（4-7）/全核（0-7）。
- 每输出 per-output peak limiter（0.89 ≈ -1 dBFS）。
- 系数：原型内合成（每条路径一个窗函数低通 FIR，模拟低频跨馈）；后续版本从 WAV/manifest 导入。
- 系数共享/稀疏/band-limit：设计上每路径独立系数 + 稀疏可跳过；原型仅合成全矩阵，接口预留。

## 构建/运行
```bash
# x86
cargo build --release && ./target/release/tspi-mimo-fir --mode fft --n 12 --m 10 --taps 2048 --seconds 5 --threads 4
# aarch64（VM 上 cross，linker/sysroot 指向 Debian12 rootfs）
cargo build --release --target aarch64-unknown-linux-gnu
# 板端（RT baseline）
taskset -c 4-7 /tmp/tspi-mimo-fir --mode fft --n 12 --m 10 --taps 2048 --fs 48000 --seconds 5 --threads 4
```

## 首轮结果（x86，参考）
- `fft 2×10×512`：rtf 8.3；`fft 12×10×512`：rtf 5.76（x86 桌面，CPU 未记录）。

## 板端首轮（RT baseline 6.1.99-rt36，**板子同时在做 32 帧/xrun 验证，数据受干扰**）
| 配置 | 线程/绑核 | rtf | cpu% | 备注 |
| --- | --- | --- | --- | --- |
| 12×10×2048 | 8 / 0-7 | 1.47 | 155 | 可实时（受干扰环境下） |
| 12×10×4096 | 4 / 4-7 | 2.58 | 189 | 与其它组不一致，受干扰/需复测 |
| 12×10×512/1024/2048 | 4 / 4-7 | ~0.22–0.25 | 4–14 | 明显被 RT 测试进程抢占（低 CPU、墙钟长） |
| 2×10×512/2048 | 4 / 4-7 | ~0.23 | 4–5 | 同上 |

> 结论待干净环境复测：低 CPU% + 长墙钟说明进程被高优先级 RT 音频/测试线程抢占，不是算力不足。

## 下一步（原型 → 可交付）
1. 持久 worker 线程 + 屏障（避免每块 spawn 线程开销）；可选核内 spin/亲和。
2. 系数导入：WAV/raw + manifest（source→output、taps、fs、band_limit、sparse），与 channelmap v2.1 per-route FIR 对齐。
3. 稀疏/带限：零路径跳过、低带路径短 taps；CPU/延迟估算输出。
4. ALSA 直连（capture→engine→playback）或接 CamillaDSP 前后级；RT 线程优先级/锁页；与 32 帧、MOTU USB 联调。
5. 板端干净环境复测：2×10/12×10 × 512–4096 × 48/96k × threads 1/4/8，记录 RTF/CPU/cyclictest 并发余量。
6. **多速率/band_limit（task-12 后续）**：低频带抽取到 12/16 kHz 再跑 MIMO FIR（见 ../MIMO-ENGINE-EXPLAINER.md §6）：`--lowrate 12000 --decim 8`、半带抗混叠、插值回全速；系数 samplerate 与低速率一致；全带/高通交 CamillaDSP。