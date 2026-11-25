# tests/T22 —— T22（format）边界参考向量与语义测试

- 归属：WBS T22 / team task-93，owner rt-format2/rt-format；写域仅
  `crates/rt-host/src/format.rs` + `crates/rt-host/tests/T22/` + `rt-stack/evidence/T22/`。
- 测试文件：`format_vectors.rs`（36 个 `#[test]`，函数名统一 `t22_` 前缀；模块名 `format_tests`）。
- 运行（在 `rt-stack/` 下）：

```
cargo test -p khaen-rt-host format     # WBS T22 DoD 原文命令（40 executed：36 t22_ + 4 个 T17 名字含 format 的用例）
cargo test -p khaen-rt-host t22_       # 仅 T22 自身（36 executed）
```

- 期望：`running 40 tests ... 40 passed; 0 failed` / `running 36 tests ... 36 passed; 0 failed`
  （executed test_count > 0，WBS 非空测试门；exit 0）。
- 离线：纯内存切片转换，不打开设备、不需要声卡；T17 联动只用 `DuplexSession::offline`（fake backend，
  T01 E1 MOTU 实测参数），硬件路径不被触及（T17 HwOpenGate 默认 Denied）。

## 为什么用 include! 而不是 public_registration.rs

`rt-stack/README.md` §3.1/§5 规定：`tests/public_registration.rs` 与公共注册**只由 integrator 写**；
cargo 只把 `tests/*.rs`（顶层文件）当集成测试目标，**不会自动发现** `tests/T22/` 子目录。

因此 T22 沿用 T16/T17/T18 的合法方式：`crates/rt-host/src/format.rs` 末尾

```rust
#[cfg(test)]
mod format_tests {
    include!("../tests/T22/format_vectors.rs");
}
```

本文件因此参与 lib 的单测构建，真实被 `cargo test -p khaen-rt-host format` 执行（executed = 36 t22_ 用例；
测试路径 `format::format_tests::t22_*` 含 format）。**integrator 不要**再把它加到
`public_registration.rs`（会重复注册/编译失败）。

## 测试覆盖（对照 core-v1 §9.2/§11.9、F-42/F-43、DSP-HOST-DECISION §6 与 WBS T22 DoD）

| 组 | 覆盖 |
|---|---|
| A 格式表/错误码 | S24/S32/S16/F32 的 `device_sample_bytes`；F32_PLANAR/F64/未知值拒绝；与 T17 `alsa::sample_bytes` 常量表逐项一致（不漂移）；§9.2 冻结码 101/104/105/106/109/118/122 与 209/210 为计数事件（非 Result 错误） |
| B capture 解码 | S24 符号扩展（bit23=1 解负）、满量程 ±2^23、LE 字节序（LSB 在 byte0）；S32 全 32-bit 量程（0x7FFFFFFF→1.0、i32::MIN→-1.0）与 int→f32 round-ties-even（0x7FFF_FF01→2147483392）；S16 满量程；F32 直取位（-0.0/次正规保留）+ NaN/±Inf sanitize 0 + 越界饱和 ±1；MOTU 20ch planar 映射与 Toslink ch12/13（T01 E1） |
| C capture stride/边界 | 显式 stride=80 只写 [0,64) 有效区、填充区保持调用方 NaN；stride<period / 非 16 倍数 / dst 过短 / src 少于一个 period 全部 `ERR_STRIDE_ALIGN`；空缓冲不 panic |
| D validate 拒绝矩阵 | 全设备格式/48k/96k 接受；abi≠1→101；F32_PLANAR/F64/未知→122；44.1k→104；period 256/32→105；buffer<period 或非 period 整数倍→118；n_in/n_out 为 0 或超 20/22→106；stride 小于 period×ch×bytes 或非 bytes 整数倍→109；未知 flags 位/reserved[1..4] 非 0→118 |
| E playback 编码 | S24 边界 12 向量（含 ±1.0→±满码、0.5 LSB ties-to-even：0.5→0、1.5→2、2.5→2、-1.5→-2）；S32/S16 满码；clip 饱和统计（|x|>1 计 clip、NaN/±Inf 先 sanitize 不计 clip、x=±1.0 不算 clip）；F32 输出 clip/位保真/NaN→0；MOTU 22ch 交错字节布局逐字节断言；strided playback 尾部 padding 不被写；整数格式 code→f32→code 逐位可逆（S24/S16） |
| F dither | flags bit0/bit1 与 DitherCfg 一致性矩阵（半开配置→118，F32+dither→118，错误在转换入口返回不静默）；Off 逐位确定；无状态入口同种子可复现、换种子不同、种子 0/1 经 SplitMix64 不退化；stateful 跨 period 前进、dither off 不推进状态、reseed 回起点；TPDF 三值统计（P(±1)=1/8、P(0)=3/4、均值 0、方差 0.25、无符号偏置）；S24/S16/S32 均施加 dither；S32 高声压 dither 被 f32 24-bit 有效位吸收（如实记录，不伪造改善）；**golden 序列钉死**：seed=42/0xDEADBEEF0BADF00D 的 S24 与 seed=99 的 S16 前 N 码由独立 Python 参考实现生成并冻结（F-43 跨版本可核对） |
| G planar 缓冲契约 | `validate_planar_buffer`：stride 16 样本倍数 + 基址 64 B 对齐 + 长度覆盖末通道 period；违反→109；64 B 对齐 + stride 80 下转换正常 |
| H T17 actual 联动 | T17 fake 协商 MOTU actual（20ch/22ch S24_3LE 48k period128 buffer1024、stride 7680/8448、flags=capture_sanitize）被 T22 `validate` 接受；`in_min_bytes/out_min_bytes` 与 T17 `DuplexActual::period_bytes` 逐项相等（跨任务口径不漂移）；用该 actual 跑 capture/playback 全链（满码、ch12 第一帧、末样本） |
| I 统计/RT 纪律 | FormatStats merge 饱和不溢出/is_clean；源码级自证 `format.rs` 无 Vec/Box/String/format!/std::/alloc::/unsafe/extern "C"/println!/thread::/fs::/File::/Mutex/RwLock/HashMap/Arc/Rc/Cell/panic!/.unwrap()/.expect( 且非 RT-MODULE、带 `[IO]` 标记 |
| J 边界/健壮性 | 空 src/dst 返回错误不 panic；超长 src/dst 只消费/写一个 period，其余不动；128 帧 × 20in/22out × 全设备格式不 panic、stats clean |

## 关键边界参考向量（摘要）

- S24_3LE decode：`0x000000→0.0`、`0x000001→2^-23`、`0xFFFFFF→-2^-23`、`0x800000→-1.0`、`0x7FFFFF→1-2^-23`。
- S32_LE decode：`0x7FFFFFFF→1.0`（f32 无法表示 1-2^-31）、`0x80000000→-1.0`、`0x7FFFFF01→2147483392/2^31`。
- playback ties-to-even（S24，0.5 LSB 网格）：`+0.5→0`、`+1.5→2`、`+2.5→2`、`-0.5→0`、`-1.5→-2`。
- 饱和：`+1.0→2^23-1`、`-1.0→-2^23`；输入 `1.0000001/±1e30` 先 clip 计统计；NaN/±Inf 先 sanitize 0 计 nonfinite。
- TPDF：u1,u2 ~ uniform[0,1) 24-bit 网格，d=u1-u2∈(-1,1) LSB，峰峰 2 LSB；取整后 P(±1)=1/8、P(0)=3/4。

## 已知修复/限制（NOT_RUN）

- 2026-11-17 接续（前次 agent 被宿主机休眠中断）：修复 2 个**测试侧**向量 bug（实现未改）：
  1) `t22_roundtrip_integer_formats` 原分配的 planar/back 缓冲只有一个通道长度，违反 §11.9
     stride>=period 契约（转换入口正确返回 109）；改为完整 period（channels × 64）缓冲。
  2) `t22_t17_negotiated_motu_actual_matches_t01_e1` 把 interleaved sample index 写成 `128*12`
     （应为 frame 0/ch12 = sample index 12）。
- dither golden 向量的独立参考实现：`evidence/T22/gen-dither-golden.py`（numpy.float32 逐步模拟 IEEE binary32）
  + `evidence/T22/golden-dither.txt`（生成输出）；Rust 实现在首轮逐码全等，冻结进 `t22_dither_golden_sequence_pinned`。
- 真实 ALSA/MOTU 采集与播放、真实 xrun、96k 设备、板端 RT：**NOT_RUN**（T17/T19/T26/T71 层）。
- 24-in-32 容器、大端格式、平面设备格式、噪声整形、重采样/ASRC：非 T22 目标（模块文档 §7）。