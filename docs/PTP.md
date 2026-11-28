# PTP（linuxptp）接收端支持（v0.1.36+）

本文说明 khaen 接收端如何读取本机 PTP 状态，以及如何在 N150 上把 linuxptp 作为从钟挂起来。
**本版只做到“可见”**：daemon 的 `/api/status.ptp` 如实上报锁状态与 offset；音频路径
（rt-host / ASRC / ALSA）**不消费 PTP**，行为与 v0.1.35 完全一致。

## 为什么这样做

- AES67 的标准前提是媒体时钟由 PTP 锁定；khaen 之前没有 PTP，靠输入 ASRC 吸收收发时钟差。
- 先把 PTP **从钟**挂起来并让锁状态可见，是以后任何“PTP 时间轴 / 多接收端确定延迟”方案的前提；
  但 PTP 只对齐“发送端媒体时钟 ↔ 接收端主机时钟”，**不改变声卡 DAC 自己的时钟**，所以 ASRC 仍然保留。
- 不在 khaen 里自己实现 PTP 协议栈：用 linuxptp（ptp4l/pmc）做 servo/BMCA/时戳，khaen 只读状态。

## 状态形状

`GET /api/status`（daemon）新增加法字段：

```json
"ptp": {
  "schema": "khaen-ptp/v1",
  "available": true,
  "source": "linuxptp-pmc",
  "locked": true,
  "mode": "software",
  "interface": "enp2s0",
  "gm_present": true,
  "gm_identity": "ae6f0d.fffe.8e86fc",
  "domain": 0,
  "clock_class": 248,
  "offset_ns": 14546.0,
  "mean_path_delay_ns": 36268.0,
  "steps_removed": 1,
  "time_source": "0xa0",
  "current_utc_offset": 37,
  "age_ms": 1200
}
```

- `available:false` 时所有数值为 `null`，`reasons.ptp` 给稳定原因（pmc 未安装 / ptp4l 未运行 /
  没有 GM / 只在 Linux 可用），**不编造**。
- 后台探针每 2 s 刷新（`pmc -u -b 0` 只读查询），`/api/status` 只读缓存，不阻塞 HTTP。
- `locked` = `gm_present` 为 true 且读到了 `offsetFromMaster`。
- `mode`/`interface` 从 `/proc/<pid>/cmdline`（`-S`/`-H`/`-i`）或 `-f` 配置的
  `time_stamping` 推断；拿不到就是 `null`，不影响锁状态。

## N150 部署（软件时间戳）

N150 的 `enp2s0` 是 Realtek r8169，**没有 PHC**（`/dev/ptp*` 不存在），只能软件时间戳：

```bash
sudo install -m 0644 /opt/khaen/current/deploy/linuxptp/ptp4l-slave.conf /etc/khaen/ptp4l-slave.conf
# 观测模式（默认，不动系统时钟）：
sudo systemd-run --unit=khaen-ptp-slave --collect \
  /usr/sbin/ptp4l -S -s -i enp2s0 -f /etc/khaen/ptp4l-slave.conf
# 或安装模板 unit：
sudo install -m 0644 /opt/khaen/current/deploy/khaen-ptp-slave@.service /etc/systemd/system/
sudo systemctl enable --now khaen-ptp-slave@enp2s0.service
```

验证：

```bash
pmc -u -b 0 'GET TIME_STATUS_NP' 'GET PARENT_DATA_SET' 'GET CURRENT_DATA_SET'
# /api/status 需要登录会话；登录后：
curl -s -b session=... http://127.0.0.1:80/api/status | jq .ptp
```

## NTP 归属（重要）

默认配置 `clock_servo nullf`：只观测，不改 `CLOCK_REALTIME`，因此可以与
`systemd-timesyncd` 共存。若要 PTP 真正接管系统时钟（标准 slave）：

1. 把 `/etc/khaen/ptp4l-slave.conf` 的 `clock_servo` 改成 `pi`；
2. 停用 `systemd-timesyncd`（二者不能同时驯服同一个时钟）；
3. 重启 ptp4l，并用 `pmc` 观察 offset 收敛。

这一步会写系统时钟，属于需要明确许可的设备操作；在没有决定“wall clock 跟 PTP 还是跟互联网 UTC”
之前，保持 `nullf` 观测模式。

## 边界与未实现

- **不消费 PTP 做播放**：rt-host 仍按 ALSA 设备节奏拉块，ASRC 仍在输入侧吸收 DAC 时钟差。
- **不做多接收端延迟确定性**：那需要“按 PTP 时间轴排播放 + 输出侧 SRC（或 PTP 从钟 DAC）”，
  是后续独立决策；本版只提供锁状态/offset 这个前提。
- **硬件时间戳模式**：本机无 PHC，NOT_RUN；同一接口留给未来的 i226/i210 等网卡。
- **绝对时间语义**：当前实验室 GM 是 class 248 / free-running，`currentUtcOffsetValid=0`，
  只适合作频率参考，不要拿它的绝对时间做多设备排播。
- 真实 N150 长稳（小时级锁保持、与发送端并发对齐）属于部署验证项，见交接报告。
