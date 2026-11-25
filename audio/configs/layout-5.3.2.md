# 5.3.2 输出布局（10 路）与通道映射

> 用户口径（2026-11-15）：输出布局 = **5.3.2**（5 主 + 3 低音 + 2 高置 = 10 路），用满 MOTU UltraLite-mk5 的模拟输出：
> **Main 1-2 + Line 3-10 共 10 路**；USB 硬件共 22 playback 通道，其余（S/PDIF/ADAT/Phones）不用、在 CamillaDSP 中补零。
> 输入 = NuPrime H16-AIP（HDMI→AES67/Dante 16ch 发送端）：AES67 固定 48 kHz、Dante 96 kHz；16ch 容器当前为 7.1.4/9.1.4。低音配置 / 高置位置 / 输入通道顺序 **TBD**（待用户与 H16 实测确认）。

## 1. 输入（H16-AIP 16ch）

- H16-AIP：HDMI 音频 → AES67/Dante 16ch 发送端；AES67 模式固定 **48 kHz**、Dante 模式 **96 kHz**（不能 AES67 96k）。
- 容器：当前 7.1.4 / 9.1.4（16ch）。CamillaDSP `capture.channels = 16`；输入通道顺序 **TBD**（WAVE/7.1.4 常见顺序假设见下表，待 H16 逐路实测核对）。

| 假设 in ch | 7.1.4 元素 | 说明 |
| --- | --- | --- |
| 0/1 | Front L/R | |
| 2 | Center | |
| 3 | LFE | 低音管理 TBD |
| 4/5 | Surround L/R（Ls/Rs） | |
| 6/7 | Back Surround L/R（Lrs/Rrs） | 5.3.2 下混策略 TBD（暂不用） |
| 8/9 | Top Front L/R（Tfl/Tfr） | 5.3.2 的 Height 暂用这两路 |
| 10/11 | Top Rear L/R（Trl/Trr） | 暂不用 |
| 12..15 | 9.1.4 的 Lw/Rw 或保留 | 暂不用 |

> 注意：7.1.4 严格只占 12ch；H16 的 16ch 容器可能把 9.1.4（14ch）+ 2 保留，或 7.1.4 + 4 保留。**必须用 H16 逐通道测试信号 + box 回读确认顺序**（验收 A18），再改 mixer `sources[].channel`。

## 2. 输出通道映射（默认假设；**上板必须逐通道核对**）

| USB ch | 物理输出（MOTU） | 扬声器（5.3.2） | 默认 mixer 源（占位） | 备注 |
| --- | --- | --- | --- | --- |
| 0 | Main 1 | Front L | capture ch0 | 主左 |
| 1 | Main 2 | Front R | capture ch1 | 主右 |
| 2 | Line 3 | Center | capture ch0 | 占位，实际需中置分频/音量 |
| 3 | Line 4 | Surround L | capture ch0 | 占位 |
| 4 | Line 5 | Surround R | capture ch1 | 占位 |
| 5 | Line 6 | Sub 1 | 0.5x(L+R) | 占位 -6 dB；低音管理 TBD |
| 6 | Line 7 | Sub 2 | 0.5x(L+R) | 同上 |
| 7 | Line 8 | Sub 3 | 0.5x(L+R) | 同上 |
| 8 | Line 9 | Height L | capture ch0 | 占位 |
| 9 | Line 10 | Height R | capture ch1 | 占位 |
| 10-21 | S/PDIF 1-5 / ADAT / Phones | - | 不定义（CamillaDSP 补零） | 模板保持 mixer out = 22 且 dest 10..21 不出现 |

注意：MOTU UltraLite-mk5 的通道顺序假设来自 alsa-ucm-conf 的 UltraLite-mk5-HiFi.conf（PlaybackChannel 0=Main1-2 L ... 10=Phones）；**上板必须用逐通道扫频核对**（ACCEPTANCE-MATRIX §5 A14）。若顺序不同，只改 CamillaDSP mixer 的 dest 映射，不动硬件。

## 3. CamillaDSP 实现口径

- **playback.channels = MOTU 硬件通道数（22，运行期探测，最多 22）**，format: S24_3_LE；mixer out 同值，dest 10..21 不定义（CamillaDSP 自动补零）→ 满足“硬件 22ch 补零”。
- 分频/PEQ/延迟加在 mixer 之后的输出通道 0..9：pipeline 里 `Filter channels:[0..9] names:[...]`，每路独立。
- 极性：mixer 的 sources[].inverted: true（或 Gain 取反），逐路可配。
- 输入固定 16ch（H16-AIP）：capture.channels=16、mixer in=16；7.1.4/9.1.4 元素按假设映射到 5.3.2 十路（见 §1）；**CamillaDSP 不做 Atmos/DTS 解码**，H16 必须送已解码 PCM。

## 4. 文件对照

| 文件 | 用途 |
| --- | --- |
| overlay/etc/camilladsp/dante.yml / aes67.yml | 默认模板：2ch→10ch 占位 upmix + 每路 PEQ/Delay 占位（0 dB / 0 ms） |
| configs/fir/example-532-crossover.yml | 5.3.2 完整示例：IIR 分频（Linkwitz-Riley）+ 每路 PEQ/Delay + 10 路映射 |
| configs/fir/gen-fir-wav.py | 生成 FIR 系数（如用短 FIR 做房间校正） |

## 5. FIR/CPU 预算（10 路，估算；上板实测为准）

直接卷积（2 FLOP/MAC，10 路同时）等价算力：

| taps/ch | 48 kHz | 96 kHz |
| --- | --- | --- |
| 512 | 0.49 GFLOP/s | 0.98 GFLOP/s |
| 2048 | 1.97 GFLOP/s | 3.93 GFLOP/s |
| 4096 | 3.93 GFLOP/s | 7.86 GFLOP/s |
| 16384 | 15.73 GFLOP/s | 31.46 GFLOP/s |

说明：
- CamillaDSP Conv 用 FFT 卷积，长 FIR 实际乘加远低于上表；按 overlap-save 粗估每样本每声道约 30*log2(2*taps) FLOP（16384 tap → 约 450 FLOP/sample/ch），10 路 96k 合计约 0.43 GFLOP/s 量级——**纯算力不是主要瓶颈，内存带宽/多线程调度/延迟才是**。
- IIR 分频：约 10 biquad/声道 → 10 路 96k 合计 <0.1 GFLOP/s，可忽略。
- **建议**：分频用 IIR（Linkwitz-Riley，相位对齐简单）+ 每路 512–4096 taps 短 FIR 做房间校正；16384 taps ×10 路 ×96k 只作压力档/局部使用，必须实测 24h 0 xrun。
- A72 单核/多核实测数据回填 ACCEPTANCE-MATRIX。

## 6. USB 带宽（风险提示）

- 22ch × 96 kHz × 24 bit = **6.34 MB/s ≈ 50.7 Mbps**（仅播放方向；含 USB 帧开销实际更高，若设备输入端点也工作则再叠加）。MOTU mk5 是 USB2 High-Speed（480 Mbps 理论，等时传输可用 ~280–350 Mbps），单看播放可跑；叠加 10ch DSP 与 USB 中断抖动需实测。
- 若 96k 下 22ch 出 xrun：降 48k（AES67 本来 48k）或减少播放通道数（破坏“22ch 补零”口径，需记录）。
