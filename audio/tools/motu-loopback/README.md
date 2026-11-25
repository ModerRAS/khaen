# MOTU + snd-aloop 连续实时 xrun 冒烟（静音）

> 用途：MOTU UltraLite-mk5 与 CamillaDSP 的连续实时链路 xrun/CPU 测量。**输入为全零静音**（不产生可听声音），MOTU 输出也是静音。
> 依赖：RT 内核带 `snd-aloop`（OEM v7 起）。

## 链路
```
aplay(全零 raw) -> hw:Loopback,0,0 ==snd-aloop==> hw:Loopback,1,0
  -> camilladsp (chunksize 32/64/128/256) -> hw:3,0 (MOTU 22ch S24_3LE) -> 静音
```

## 用法（板端，lckfb + sudo）
```bash
sudo modprobe snd-aloop
bash loopback-xrun.sh 60 32        # 60 秒，chunksize 32
```
- 自动识别 Loopback 卡号；CamillaDSP 配置：22ch/48k/S24_3LE pass-through。
- 统计：wall、CPU（/proc 采样）、xrun（日志 underrun/overrun/XRUN 计数）、hw_params（period/buffer）。
- 长期测试：秒数设大（如 86400 做 24h），日志留在 /tmp/motu-loopback/。
- **只发静音，不会出声**；改 MOTU_DEV 可用其它卡。
