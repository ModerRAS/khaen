# configs/ —— 配置来源与变体

## 1. 权威位置

**实际进镜像的配置在 `overlay/etc/`**（单一事实来源，避免双份漂移）。本目录放：
- 说明与变体（variants），
- 非默认场景的示例（FIR 分频等）。

| 配置 | 权威文件（overlay） | 说明 |
| --- | --- | --- |
| CamillaDSP（Dante 96k） | `overlay/etc/camilladsp/dante.yml` | 模板，`__TOKEN__` 由 khaen-mode 渲染；**H16 16ch 输入 → 5.3.2 十路输出（22ch 补零）** |
| CamillaDSP（AES67 48k） | `overlay/etc/camilladsp/aes67.yml` | 同上；AES67 固定 48k、16ch |
| statime（PTPv1 follower） | `overlay/etc/statime/inferno-ptpv1.toml` | 同上 |
| ptp4l（PTPv2 GM） | `overlay/etc/ptp4l-master-aes67.conf` | 仅 B2/B3 |
| aes67-daemon | `overlay/etc/aes67-daemon/daemon.conf` | 模板 |
| ALSA（Inferno PCM） | `overlay/etc/asound.conf` | 参数走环境变量 |
| 运行期默认值 | `overlay/etc/default/khaen-audio` | 采样率/chunksize/超时/PTP 角色 |
| RT 权限 | `overlay/etc/security/limits.d/95-khaen-audio.conf` | 登录 shell 侧 |
| MOTU 固定名 | `overlay/etc/modprobe.d/khaen-audio.conf` + udev | ALSA card id |

## 2. 变体（variants/）

| 文件 | 场景 | 使用方式 |
| --- | --- | --- |
| `ptp4l-slave.conf` | 调试/对照：让本机 ptp4l 做 PTPv2 slave（例如验证网内 GM 质量）；**不要与 aes67-daemon 同跑**（LKM 会截走 PTP） | `ptp4l -H -i eth0 -f configs/variants/ptp4l-slave.conf -m` |
| `inferno-ptpv2-leader.toml` | Dante 模式备选：网内无 PTPv1 主钟、且所有 Dante 设备支持 AES67 时，statime 做 PTPv2 leader | 替换 /etc/statime 配置后 `khaen-mode dante`（未上板验证，见 RISKS D2） |

## 3. 布局与 FIR（layout / fir）

- [`layout-5.3.2.md`](layout-5.3.2.md)：**5.3.2 十路输出映射表、H16-AIP 16ch 输入假设、每路 PEQ/延迟/极性口径、10 路 FIR/CPU 预算、USB 带宽**。
- [`fir/README.md`](fir/README.md)：FIR 示例与系数生成；`example-532-crossover.yml` 是 5.3.2 完整可跑示例（IIR 分频 + 每路 PEQ/Delay）。

## 4. 修改流程

1. 只改 `overlay/` 下的权威文件；
2. 同步更新 `overlay-manifest.md`（新增文件必须登记）；
3. 需要新 token 时，改 `overlay/usr/local/bin/khaen-mode` 的 render 函数与文档（README §9 契约）；
4. 不要改 `/run/khaen/*`（每次切换会被覆盖）。
