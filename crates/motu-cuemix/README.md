# khaen-motu-cuemix —— MOTU UltraLite-mk5 CueMix 5 控制库 + CLI（T40）

目的：给 khaen daemon 一个**可被简单调用**的 MOTU 控制通道，并提供一个诊断 CLI：
检测设备 → 读状态（静音 / 采样率 / Main 推子 / Main trim / 总线静音）→ 做**单键写 + 立即回读**。

设计约束（任务书）：

- **纯 std，零外部依赖**：手写最小 RFC 6455 客户端、datastore 消息层编解码、base64/SHA-1/SHA-256、RSA-OAEP(SHA-1) 认证。不新增 Cargo 依赖，离线可构建。
- **绝不批量写**：任何一次 `set_*` 调用最多只发 **1 个 datastore 值、1 个 WebSocket 帧**；立体声 Main 音量是两次独立的单键写，各自回读。
- **绝不假设成功**：一次写只有在设备把同样的值重新 stream 回来后才会报告成功；超时/值不同分别返回稳定错误码。
- **不 panic**：库代码（非测试）没有 `unwrap`/`expect`/`panic!`/`assert!`（用 grep 审计，见报告）；所有索引有边界检查。
- **错误码稳定**：daemon 只应分支 `CueMixError::code()`（`E_IO` / `E_TIMEOUT` / `E_HANDSHAKE` / `E_PROTOCOL` / `E_CLOSED` / `E_AUTH_REQUIRED` / `E_AUTH_FAILED` / `E_NOT_DETECTED` / `E_RANGE` / `E_READBACK_TIMEOUT` / `E_READBACK_MISMATCH` / `E_UNSUPPORTED`），不要匹配 Display 文本。

## 状态

| 项 | 状态 |
| --- | --- |
| Windows 本机构建 / 129 个自动测试 | **实测通过**（`cargo fmt` / `cargo clippy -D warnings` / `cargo test` 全绿） |
| 真实 MOTU 读写、控制网、密码认证、音量键最终语义 | **NOT_RUN**（本机是 Windows，无 MOTU；由 lead 在 N150 上做单键写 + 回读验收） |
| 真实 WS 集成测试 | 提供 `tests/live_device.rs`（`#[ignore]`），N150 上手动执行 |

## 协议摘要（来自设备侧参考实现 `cuemix_client.py`，只读参考）

- 传输：`ws://169.254.111.126:1280/`（MOTU USB-CDC 网口，设备侧固定地址；本机侧网卡是 `enx0001f2fe7e6f`）。
- datastore 消息层（大端）：
  - 设备→主机：`id:u16 index:u16 value`（长度由类型隐含）
  - 主机→设备：`id:u16 index:u16 len:u16 value`
  - 类型：`B`=u8、`I`=s32、`I16`=u16、`F`=8.24 定点（raw/2^24）、`S`=32 字节 NUL 填充、`A`=变长 bytes
- 特殊 id：`0xFFFE` 代理/认证消息、`0xFFFF` 代理状态、`6000` 电平表（控制路径忽略）。
- 认证（仅当设备启用控制网密码）：设备推送 `0xFFFE` 子消息 6（公钥 PEM）、2（密码启用标志）、0（认证请求 + 10 字节序列号）；客户端回子消息 1，内容是 **RSA-OAEP(SHA-1)** 加密的 `sha256_hex(password)` ASCII（64 字节）。OAEP 使用 MGF1-SHA-1、空 label，与参考实现逐字节一致（见下）。
- WebSocket：标准 RFC 6455 客户端握手（校验 `Sec-WebSocket-Accept`）、客户端帧掩码、ping→pong、close、分片消息重组；只接收 binary 消息（text 帧忽略并计数）。

## 主输出音量键与静音键：证据与结论

基线快照 `C:\WorkSpace\Audio\tmp\cuemix-baseline.json`（2498 值，`authenticated=true`，model `UL5L`）与 2026-11-26 的两次对照快照：

| 键（datastore） | id | 类型 | 基线值 | 语义结论 |
| --- | --- | --- | --- | --- |
| `kMuteEnable[0]` | 5019 | B | `[0]` | 全局静音；对照快照里唯一变化就是 `[0] -> [1]`（前一次会话单键写 + 回读成功），**`set_mute` 写这个键** |
| `koBusFader[0..13]` | 1027 | F | `[1.0 ×14]` | 14 条输出总线推子；索引 0/1 是 **Main 1 / Main 2**（主输出 L/R），`set_volume` 写 `koBusFader[0]`、`koBusFader[1]` |
| `koBusMute[0]` | 1028 | B | `[0]` | Main 1 总线静音标志（只读进 `read_state.ko_bus_mute`） |
| `kSampleRate[0]` | 10 | I | `[48000]` | 设备采样率 |
| `kMainTrim[0]` | 5011 | B | `[31]` | 面板 Main trim（只读） |

键映射全部做成了常量：`props::keys::*`、`props::ID_*`、`props::MAIN_FADER_LEFT_INDEX`(0) / `MAIN_FADER_RIGHT_INDEX`(1)、`props::BUS_NAMES`。
推子是 0.0..=1.0 线性幅度（快照基线 1.0=unity=0 dB），dB = 20*log10(linear)；库提供 `fader_to_db` / `db_to_fader` / `quantize_linear`（8.24 定点量化，写与回读用同一量化值，保证比较精确）。

**NOT_RUN**：`koBusFader[1]` 在真实设备 stereo Main 上的最终语义（是否与 index 0 联动/被忽略）需要 lead 在 N150 上做“两次单键写 + 分别回读”验收；库在此失败时会如实报错，不会假装成功。

## 库 API（给 daemon）

```rust
use khaen_motu_cuemix::{detect, DetectConfig, ClientConfig, CueMixClient, MainChannel};

// 1) 检测（Windows / 无设备：detected=false，不报错）
let detection = detect(&DetectConfig::default());
if !detection.is_ready() { /* reason: cards_unavailable / no_motu_card / control_unreachable */ }

// 2) 连接 + 认证 + 初始 datastore burst
let mut client = CueMixClient::connect(ClientConfig::default())?;

// 3) 只读状态（未收到的字段是 None + missing() 给出原因）
let state = client.read_state()?; // mute / sample_rate_hz / main_fader / main_trim / ko_bus_mute

// 4) 单键写 + 回读校验
client.set_mute(true)?;      // 1 帧，写 kMuteEnable[0]
client.set_volume(0.5)?;     // 2 帧：koBusFader[0] 然后 koBusFader[1]，各自回读
client.set_volume_channel(MainChannel::Left, 0.5)?; // 只写左声道（CLI 用，失败可分段报告）

// 5) 只读快照（与参考客户端 --save 同 shape：meta + values）
client.save_snapshot(std::path::Path::new("cuemix.json"))?;
```

返回结构：

- `WriteReport<T> { key, index, before, wrote, readback, changed, frame_sent }`；`frame_sent=false` 表示目标值已在位、未发写帧（由当前 stream 值确认）。
- `VolumeReport { linear, db, left, right }`；失败时 `VolumeError { completed, error }` 报告已完成的声道（左成功、右失败时 `completed.len()==1`），错误码委托给 `error.code()`。
- `CueMixState::missing()` 列出未收到的字段，`is_complete()` 判断齐全；绝不编数值。

为测试/daemon 注入自定义 transport：`CueMixClient::with_transport(Box<dyn Transport>, ClientConfig)`（`Transport` trait 只有 `send_binary` / `recv_binary` / `description` / `close`，一次 `send_binary` = 一帧）。

## CLI：khaen-motu

```text
khaen-motu info [options]
khaen-motu mute on|off [options]
khaen-motu volume <0..1|dB> [options]
khaen-motu save <file> [options]
```

选项：`--host` / `--port` / `--password`（默认 `$KHAEN_CUEMIX_PASSWORD`）/ `--cards` / `--connect-timeout` / `--sync-timeout` / `--readback-timeout` / `--drain` / `--no-probe` / `--json` / `--strict`。

退出码（稳定）：`0` 成功；`2` 用法/范围错误；`3` 设备缺失或控制网不可达；`4` 传输/握手/认证/协议错误；`5` 写回读校验失败。

写命令输出示例（before / wrote / readback 全部打印，`[verified]` 只在设备回读一致时出现）：

```text
mute: before=off(0) wrote=on readback=on [verified]
volume: target -6.00 dB (0.501187 (-6.02 dB))
  koBusFader[0] (Main 1 (left)): before=1.000000 (0.00 dB) wrote=0.501187 (-6.02 dB) readback=0.501187 (-6.02 dB) [verified]
  koBusFader[1] (Main 2 (right)): before=1.000000 (0.00 dB) wrote=0.501187 (-6.02 dB) readback=0.501187 (-6.02 dB) [verified]
```

`restore` **不实现**（任务明确不做）。`save` 只读：只收集 datastore 并写 JSON，不发任何写帧。

## 测试与覆盖点

- 单元测试 108 个：WS 帧编解码/掩码/126/65535 长度边界/分片与重组/控制帧/握手与 Accept 校验；datastore 消息编解码（B/I/I16/F/S/A、代理消息、未知 id、畸形/不完整帧）；base64/SHA-1/SHA-256 标准向量；PEM(SPKI+PKCS#1) 解析；模幂（Node 生成向量）；**RSA-OAEP(SHA-1) 与参考 Python 实现按固定 seed 逐字节一致**；密码摘要；推子 dB 换算与 8.24 量化；`/proc/asound/cards` 解析（N150 实测文本）、探测地址/超时注入；fake transport 上的读状态/单键写/回读不一致/回读超时/无操作写/两帧立体声/部分失败/快照/错误码。
- 集成测试 21 个（真实 loopback TCP + 独立的假设备实现，验证握手/Accept/ping-pong/掩码/分片/text 帧忽略/认证帧/回读/CLI 全流程）；真实设备测试 2 个 `#[ignore]`。

## 参考实现说明（只读参考，未修改）

- 属性表与协议来自设备上 `/usr/local/lib/tspi/cuemix_client.py`（scp 到 `C:\WorkSpace\Audio\tmp\cuemix_client.py` 阅读）。
- NOTE(legacy-defect)：参考脚本 `parse_rsa_public_key_pem` 解包 DER 时把 `_der_tlv` 的返回顺序弄反（`spki, _, _ = _der_tlv(...)` 拿到的是 tag 而不是内容），该认证路径实际不可用；这也解释了它从未在设备上被验证。本库按标准 SPKI 解析（同时兼容 PKCS#1 `BEGIN RSA PUBLIC KEY`）。
- 设备是否真的启用控制网密码、公钥具体编码（SPKI/PKCS#1）属于 NOT_RUN，需要 lead 在 N150 上用 `KHAEN_CUEMIX_PASSWORD=... khaen-motu info` 验证。

## NOT_RUN 清单

1. 真实 MOTU 写（`set_mute` / `set_volume` 的写帧、回读时序、USB-CDC 稳定性）——本机无设备；由 lead 在 N150 单键写 + 回读验收（禁止批量写）。
2. 控制网/认证实测：真实设备是否要密码、设备公钥编码、认证后 datastore burst 时序。
3. `koBusFader[1]`（Main 2 / 右声道）在真实设备上的最终语义。
4. 真实 `save` 快照与基线 `cuemix-baseline.json` 的逐值比对（需要设备窗口；格式已对齐）。
