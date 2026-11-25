# MOTU / CueMix（UltraLite-mk5）

Web 服务里的「MOTU / CueMix」页通过本机 CueMix 5 datastore 读写 UltraLite-mk5。
线协议以 `tools/motu/cuemix_client.py` 为准；任务说明和这份脚本不一致时，以脚本为准，差异记在 [MOTU-CUEMIX-RESULT.md](MOTU-CUEMIX-RESULT.md)。

不改 CamillaDSP / PTP / AES67 的音频路径，也不重启这些服务。CueMix 只动声卡内部的监听矩阵。

## 发现与连接

1. 在 `ip -o link` 里找名字以 `enx` 开头的 CDC-ECM 口。优先 MAC 厂商标识 `00:01:f2`，否则用第一块 `enx*`。
2. 该口没有 IPv4 时，幂等执行 `ip addr add 169.254.111.200/16 dev <iface>`。地址已存在（`File exists`）算成功。长驻服务**保留**这个地址；一次性探测脚本用完会删，两边不一样。
3. 在 `0.0.0.0:1280` 上被动收 UDP 信标，从载荷里截取 `{...}` JSON（`uid` / `name` / `ip` / `ipv6` / `model` / `version` / `interval`）。信标错过但上次 IP 还在 60 秒内，就复用上次地址。
4. 用 `ws://<设备IP>:1280/` 读 datastore。总超时默认 5 秒；收到第一条之后空闲 250 毫秒即停。没有网络密码时不认证。
5. 代理子项 `0xFFFE` / 子 id `2` 表示设备已设密码。为真时页面给出「设备已设密码，暂不支持」，不做 RSA-OAEP。

`KHAEN_MOCK=1` 不跑 `ip`、不打开套接字。mock 设备是 UltraLite-mk5：`169.254.111.126`，口 `enx0001f2fe7e6f`，UID `UL5LFE7E6E`，固件 `2.1.2+3352`，Main 字节 28（−28 dB），直通含 in7→out7 与 in8→out8。

状态缓存默认 12 秒。页面每 8 秒轮询，多数命中缓存。写操作绕过缓存，写完强制重读。

## 帧格式

| 方向 | 布局 | 分帧 |
| --- | --- | --- |
| 设备 → 主机 | `u16 id BE + u16 index BE + value` | 一个 WebSocket 二进制帧一条消息。定长类型只消费自己的宽度，后面多出来的字节丢掉，不再拆成下一条 |
| 主机 → 设备 | `u16 id + u16 index + u16 len + value` | 多条可拼进一帧，单帧 ≤ 1400 字节（`chunk_messages`）。单条本身超过 1400 时单独成帧。帧间间隔 10 毫秒，写完静置 100 毫秒再读回 |

类型：

| 标记 | 值 |
| --- | --- |
| B | u8 |
| I | i32 大端 |
| I16 | u16 大端 |
| F | i32 大端 / `0x01000000`（8.24）。比较容差 `1e-4` |
| S | 32 字节 UTF-8，零填充。Web 在字符边界截到 ≤31 字节；client.py 按字节切 `[:31]`。ASCII 结果相同 |
| A | 原始字节，只读 |

未知 id、电平表 id `6000`、代理状态 `0xFFFF` 丢掉并计数，与 client.py 的 `handle()` 一样。日志只记长度和条数，不记口令、token 或整帧内容。

## 矩阵下标

`index = bus * 32 + input`。14 条总线（0–1 Main，2–9 Line 3–10，10–11 Phones，12–13 Reverb）。32 路输入：0–1 Mic/Line，2–7 Line 3–8，8–9 S/PDIF，10–17 Optical，18–19 USB Main，20–27 USB Line 3–10，28–29 USB Phones，30–31 混响返回。

模拟别名：`input < 8` 且 `bus < 10` 时写成 `in{input+1}→out{bus+1}`。

| 页面说法 | 总线 | 输入 | index |
| --- | --- | --- | --- |
| in7→out7（Line 7 ← Line 7） | 6 | 6 | 198 |
| in8→out8（Line 8 ← Line 8） | 7 | 7 | 231 |

直通：物理输入 0..17 加上混响返回 30/31，且推子 > 0、mute = 0。主机输入 18..29 不算直通。

## 写入

顺序：读全量 → 存快照 → 只算差异 → 分帧写 → 重连读回 → 核对 → 不一致则按快照写回并再读一次。

| 操作 | 写什么 |
| --- | --- |
| Main 音量 | 只写 `kMainTrim`（id 5011，类型 B）。请求里的 `trim_db` 是正数字节：28 表示 −28 dB，不是负的分贝数。写入范围 `0..=100`（依据 CueMix 前端 `kMainVolCtrlInfo {min:100, max:0, u:"dB"}`；0 = 0 dB，100 = −100 dB） |
| 全部静音（final insert） | 候选集 14×20 = 280 格 `kiMixFader = 0`。缺省推子按 0 处理，所以只把非 0 的格子写上去。拒绝任何落到主机输入 18..29 的 `kiMixFader`。写完若主机推子变了，强制回滚 |
| 取消静音 | 用最近快照把这 280 格推子写回去。不改 `kMainTrim`，不动 18..29 |
| 恢复快照 | 键集等于 client.py 的 `MIXER_KEYS`：`kiMixFader` / `kiMixPan` / `kiMixSolo` / `kiMixMute` / `kiMixStereo` / `koBusFader` / `koBusMute` / `kHostOutMixMode` / `kFPGAPatch` / `kLoopbackSource` / `kLoopbackFirst` / `kNetControlEnable`。不含 `kMainTrim`，这样监听音量和矩阵恢复分开 |

快照 id 形如 `20261116T030405006Z-0001`。真机目录默认 `/var/lib/khaen-audio-web/motu-snapshots/`，只留最近 20 份。mock 的快照在内存里，不落盘。id 只允许字母数字、`-`、`_`，长度 ≤ 80，拒绝 `..`。

`k48V`（id 5004）在页面上只读：index 0 是 Mic/Line 1，index 1 是 Mic/Line 2。写入留作后续。

## 和 client.py 的对应

| client.py | 本服务 |
| --- | --- |
| `PROPS` / `BUSES` / `INPUTS` | `src/motu/codec.rs` 同一张表 |
| `Device.handle`：一帧一条 | `decode_device_frame` |
| `chunk_messages` 上限 1400 | `FRAME_LIMIT` |
| `--mute-inputs` 计划 280 条（含已经是 0 的） | 候选集仍是 280，线上只发差异。整份 1937 条写入会超时，所以只发差异 |
| `restore` 只恢复 `MIXER_KEYS`，浮点差 < 1e-4 跳过 | 相同。另外 Web 的「取消静音」只恢复 280 格直通推子 |
| 探测脚本结束删掉链路本地地址 | 服务留下 `169.254.111.200/16` |
| 已设密码走 RSA-OAEP | 直接拒绝，文案「设备已设密码，暂不支持」 |
| SysEx（其它机型） | mk5 走 WebSocket，不实现 SysEx |

接口字段见 [API.md](API.md) 第 9.1 节。配置在 `deploy/config.toml.example` 的 `[motu]`。
