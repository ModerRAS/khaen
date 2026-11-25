# configs/fir/ —— FIR/分频示例（验收矩阵用）

用于 `ACCEPTANCE-MATRIX.md` 的 FIR 强度档与多路分频演示。**OEM 镜像不预置业务系数**；
系数由校准流程/用户提供，放到目标机 `/etc/camilladsp/coeffs/`。

## 1. 生成示例系数

```bash
# 在构建机/PC 上（python3，无需第三方库）
python3 gen-fir-wav.py --taps 4096 --rate 48000 --cutoff 2000 --mode lowpass  --out /etc/camilladsp/coeffs/fir_low_48k.txt
python3 gen-fir-wav.py --taps 4096 --rate 48000 --cutoff 2000 --mode highpass --out /etc/camilladsp/coeffs/fir_high_48k.txt
# 96k 模式必须用 --rate 96000 重新生成（不能复用 48k 系数）
```

- 输出为 CamillaDSP `Conv type: Raw` 的 TEXT 格式（每行一个 float 系数），**默认按输入采样率归一化到 Nyquist 比例**，
  所以同一 cutoff 在 48k/96k 下都对应 2 kHz（脚本内部用 cutoff/rate 计算）。
- FIR 强度档（验收矩阵）：低 512 taps / 中 4096 / 高 16384。
- 线性相位（对称窗）+ 群延迟 = (taps-1)/2 个样本；分频时低/高通两路已自动对齐（同长度同窗），无需额外 delay。

## 2. 示例配置（2 分频 → 4 路输出）

`example-2way-crossover.yml`：2ch 输入 → 复制成 4 路 → 低通/高通 FIR → MOTU 通道 0..3
（0/1 = Main 1-2，2/3 = Line 3-4）。用 `tspi-mode dante --chunksize ...` 后手工替换 /run 配置调试，
或把它的 pipeline/filters 段合并进 `/etc/camilladsp/dante.yml` / `aes67.yml`。

## 3. 注意

- FIR 长度 × 通道数 × 采样率决定 CPU：96k/16384 taps/2ch 已接近 A72 上限（见验收矩阵“高”档）。
- AES67 固定 48k、Dante 固定 96k；两模式系数必须分别生成。
- `Conv type: Raw` 默认 TEXT；若用二进制/ WAV，请显式写 `format:`（如 `F32_LE`）或改用 `type: Wav`。
