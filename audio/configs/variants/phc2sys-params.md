# phc2sys 参数口径（B2 板端 GM 路径 / 或调试）

> B1（默认 AES67 模式：daemon 做 PTPv2 slave）**不需要 phc2sys**：LKM 自己处理 PTP，MOTU 时钟由 CamillaDSP rate adjust 吸收。
> 下面用于 `khaen-mode b2`（板端 ptp4l 做 PTPv2 GM + phc2sys + PHC usrvclock 桥）或调试。
> 实测记录（含 -O 0 的失败现场与修复后 pmc 输出）：`rt-stack/evidence/b2-ptp-gm/REPORT.md` §1/§3。

## 1. GM 模式（khaen-phc2sys.service 用这个）

板端 ptp4l 做 GM、希望 GM 时间可溯源到系统时间（NTP 驯服）时用：

```text
phc2sys -s CLOCK_REALTIME -c ${KHAEN_IFACE} -O 37 -m
```

- `-s CLOCK_REALTIME`：以系统时钟（NTP/PTP 源）为时间源；
- `-c end0`：被调整的网卡 PHC（由 `-c ${KHAEN_IFACE}` 解析）；
- **`-O 37`：PHC 跑 TAI（IEEE1588 PTP 时标 = UTC + 闰秒差 37 s）**。GM 必须广播 TAI；
  用 `-O 0`（PHC=UTC）时 VM slave 实测看到 **~36.35 s 假偏移**、无法稳定 SLAVE——这是
  2026-11-17 现场踩过的坑（`evidence/b2-ptp-gm/REPORT.md` §3）。闰秒变化时改这里与
  `/etc/default/khaen-audio` 的 `KHAEN_GM_TAI_OFFSET`（`khaen-mode b2` 用它做自检）；
- `-m`：打印测量值（journal 里可看 `sys offset ... s2 freq ...`，锁定后 offset 应稳定在
  ±百 ns 量级）。

完全独立 GM（不溯源系统时钟）时：**不跑 phc2sys**，ptp4l 直接用网卡 PHC 自由运行
（此时 PHC 的时标由网卡自由振荡，`khaen-mode b2` 的 TAI 自检会按需跳过/告警）。

## 2. 反向驯服（调试用：系统时钟跟随 PHC）

```text
phc2sys -s eth0 -c CLOCK_REALTIME -O 0 -m
```

用途：当本机是 PTP slave（外部 GM）而需要让系统时间跟 PTP 对齐时；**不要与 B1 的 aes67-daemon 同跑**
（LKM 在 PRE_ROUTING 截走 PTP，ptp4l/phc2sys 收不到包；且系统时间跳变会影响 usrvclock/媒体时钟）。

## 3. 与 statime / PHC usrvclock 桥的关系

- **Dante 模式**用 statime 的 `virtual-system-clock`（不改系统时钟，通过 usrvclock 导出），**不要跑 phc2sys**；
  否则系统时间跳变会被 Inferno/statime 的时钟保护逻辑当成异常（statime GUARD 会延迟同步）。
- **B2 模式**下 `khaen-phc-usrvclock` 桥把 PHC 导出成 `/tmp/ptp-usrvclock`（Inferno `CLOCK_PATH`），
  与 statime 占用同一个 socket 路径，因此两者 unit 互相 `Conflicts=`；`khaen-mode b2` 不会启 statime。

## 4. 验证

```bash
pmc -s /run/ptp4l/uds -u -b 0 "GET PORT_DATA_SET"   # portState MASTER + portIdentity
phc_ctl /dev/ptp0 get                               # 读 PHC；与 date +%s 差应 ≈ +37 s（TAI）
/usr/local/lib/khaen/phc-usrvclock --probe           # 桥自检：shift/freq_scale/TAI 偏移近似值
ethtool -T eth0 | grep -E "PTP|hardware"            # 确认硬件时间戳能力
```

`khaen-mode b2` 会按 `KHAEN_WAIT_PTP_SECS` 等 GM 进 MASTER、等 phc2sys 起、做 PHC/TAI 自检，
再按 `KHAEN_WAIT_BRIDGE_SECS`（默认 15 s）等 usrvclock 桥 socket 就绪。
