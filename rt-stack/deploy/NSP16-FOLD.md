# nsp16 对照

2026-11-23 只读了 dsp-host 上正在用的两份文件：

- `/etc/systemd/system/khaen-rt-host-nsp16.service`
- `/etc/khaen-rt-stack/rt-host-nsp16.toml`

对照仓库里的 `service/khaen-rt-host.service` 和 `deploy/config/rt-host.toml.example`。没有改板上的进程，也没有重启。

## 收进部署文件的

通用模板里没有、当前 16 声道播放还在用的项，写在 `deploy/config/rt-host-nsp16.toml`。内容与板上那份 toml 一致。

- `graph = "plan-arena"`，plan 指向 `/var/lib/khaen-rt-stack/plans/plan-h16-16in-identity.bin`
- ALSA：`backend = "alsa"`，`io_mode = "synth"`，`hw:UltraLitemk5,0`，采集 20 声道，播放 22 声道，周期 128，缓冲 1024
- 这台机器的 DSP CPU 是 `0-3`。通用模板写的 `4-7` 是另一块板的核号
- `[capture]` 整段：用户态 AES67、SDP、接口地址、失流和重锚定参数
- `last_good` 用 `last-good-nsp16.json`

## 故意不收的

- 服务名 `khaen-rt-host-nsp16`。下次安装的服务名用 `khaen-rt-host`。现在这块板仍由 `khaen-rt-host-nsp16.service` 拉起
- `ExecStart` 前面的 `nsp16-wait-stream.sh`。它只等 UDP 5010 上出现 RTP，再执行同一份 `khaen-rt-host`。不是第二套 DSP
- 二进制路径 `/usr/local/lib/khaen-rt-stack/16ch/khaen-rt-host`。名字仍是 `khaen-rt-host`
- `khaen-rt-host-nsp7.service` 和 nsp7 的配置。7 声道那份不动
- 通用模板里已经有的块长、采样率、22 声道播放、周期 128、缓冲 1024、优先级数字
