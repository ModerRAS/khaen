# khaen-asrc —— AES67 输入异步采样率转换（ASRC）

> T41：用 rubato 5.1.0 把 AES67 输入从"抖动环 + 本地 DAC 时钟硬消费"改造成
> "抖动环 + 异步重采样 + fill 漂移控制"，消除长流下的时钟漂移（原实现实测 fill 从
> ~1900 帧一路掉到 0 → fail-closed 静音）。

## 为什么是独立 crate

- `khaen-core` 是**零依赖** DSP 内核层（只允许纯数学 + 标准库）。rubato 会带进
  `audioadapter` / `audioadapter-buffers` / `realfft` / `rustfft` 等依赖和平台相关 SIMD 代码，
  放进 core 会破坏这条约束；
- ASRC 属于**输入适配层**能力（网络媒体时钟 ↔ 本地音频时钟），不是引擎内核算子；
- 独立 crate 让 `cargo tree -p khaen-core` 保持干净，host 只加一条 `khaen-asrc` 依赖，
  行为可由 `KHAEN_ASRC=off|poly|sinc` 整体开关（默认 off，保守上线）。

## 依赖 / 许可 / MSRV

| 项 | 值 |
| --- | --- |
| 重采样库 | `rubato` **5.1.0**（`Async` 异步重采样器 + `FixedAsync::Output` 固定输出块） |
| rubato 许可 | **MIT OR Apache-2.0** |
| rubato MSRV | **rustc 1.87**（依赖 `audioadapter` 5.0 也是 edition 2024/1.87） |
| 本仓库 workspace | `rust-version = "1.80"` → 集成时需由 lead 提到 **1.87**（根 Cargo.toml） |
| 其他依赖 | `audioadapter = 5.0`（rubato 的缓冲抽象；planar 适配器实现需要它） |
| 离线构建 | 本机 cargo 缓存齐全：`cargo test -p khaen-asrc --offline` 可用 |

## 数据流

```text
RTP/L24 ─► 抖动环（媒体帧域） ─► 按 input_frames_next() 取帧 ─► rubato Async（固定输出 128 帧）
   ▲                                                                       │
   └───────────── DriftController（fill 误差 → ppm） ◄── fill 快照 ◄────────┘
```

host 每个音频块调用一次（`crates/host/src/aes67_input.rs`）：

```text
need = asrc.input_frames_next();          // 随当前比率变化
把 need 帧从抖动环拷进 asrc.input_mut();   // 缺帧写 0
asrc.process();                            // 消耗 need 帧 → 输出 block_frames 帧（planar）
fill = written_upto - read_frame;          // 消费后的抖动环水位
asrc.update_fill(fill);                    // PI 控制器 → set_resample_ratio_relative(ppm, ramp=true)
```

## 组件

| 类型 | 职责 |
| --- | --- |
| `DriftController` | fill 误差 → 比率修正 ppm：一阶低通 + PI + 限幅 ±`max_ppm` + 积分限幅抗饱和 + 启动冻结 + `reset()` |
| `AsyncResampler` | 包装 rubato `Async`：`FixedAsync::Output`（输出恒 `block_frames`）、planar 缓冲全预分配、`input_frames_next()` 暴露、`set_ratio_ppm()`、`process()`、`reset()` |
| `Asrc` | 控制器 + 重采样器的组合，host 直接持有；`status(fill)` 返回 `AsrcStatus` |
| `AsrcConfig` | enable / interp / channels / block_frames / sample_rate / target_fill_frames / max_ppm / 控制增益 / sinc 参数；`validate()` fail-closed |
| `AsrcStatus` | `{enabled, ratio_ppm, fill_frames, ppm_estimate, last_error}`；host 挂到 `input.asrc` |

`process` / `update_fill` / `status` 全部是纯算术 + 对预分配缓冲的拷贝：**零分配、零锁、无 IO、不 panic**
（错误经 `Result` + `last_error` 上报）；所有缓冲（planar 输入/输出、rubato 内部缓冲、sinc 表）在构造期一次分配。

## 控制律与默认参数

fill 的物理模型（本地输出固定速率、媒体按发送端时钟到达，`fs` = 名义采样率）：

```text
d(fill)/dt = fs·[(1 + d) − 1/ratio] ≈ fs·(d + ppm)
```

`d` = 发送端相对本地的时钟偏差（ppm），`ppm = ratio − 1`。fill 低于目标 → 正 ppm（每块少消费输入帧）；
fill 高于目标 → 负 ppm。控制器输出被限幅在 ±`max_ppm`，积分项独立限幅，保证发送端偏差正好等于
`max_ppm` 时也能把 fill 拉回目标（没有余量时冻结积分会留下永久偏置）。

默认值（`AsrcConfig::default()` / `DEFAULT_*` 常量）：

| 参数 | 默认 | 说明 |
| --- | --- | --- |
| `max_ppm` | 500 | 覆盖消费级晶振（±100 ppm 内）加温漂/老化余量 |
| `target_fill_frames` | 2 × block_frames（128 块 → 256 帧 ≈ 5.3 ms） | host 取 `min(2×block, 容量/2)`，预填水位 = 该值 |
| `kp_ppm_per_frame` | 10 | 比例增益（ppm/帧） |
| `ki_ppm_per_frame_sec` | 2.5 | 积分增益（ppm/(帧·s)） |
| `fill_lpf_sec` | 1.0 | fill 低通时间常数：滤掉 1 ms 包 + 2.67 ms 块拍出的 ~2.3 Hz 锯齿 |
| `startup_blocks` | 16 | 启动阶段只更新低通、输出 0（避免把预填水位积分成假偏差） |
| `sinc_len` / `sinc_oversampling` | 64 / 64 | sinc 备用模式（BlackmanHarris2 窗 + 自动 cutoff + cubic 插值） |

失效/重置：调用方在换流（SSRC 变化/时间戳跳变）或 ASRC 失败时调用 `Asrc::reset()`：清空重采样器
内部状态与控制器积分、回到启动阶段；host 侧 ASRC 失败会自动 bypass 回原输入路径并计数（fail-closed 语义不变）。

## poly vs sinc 取舍

| | poly（默认，Cubic） | sinc（备用，sinc_len=64） |
| --- | --- | --- |
| 抗混叠滤波 | 无（near-1:1 比率下不需要） | 有 |
| 群延迟（输出帧） | 2 帧（≈0.04 ms @48k） | 32 帧（≈0.67 ms @48k） |
| 固定比率本征 THD+N（1 kHz，0/−200/+500 ppm） | −154.9 / −127.4 / −120.6 dB | −145.6 / −140.6 / −140.2 dB |
| 实测成本（2 声道 128 帧/块，release） | 2.0 µs/块（realtime ×1316） | 7.3 µs/块（realtime ×363） |

结论：**±500 ppm 漂移、48 kHz 的近 1:1 转换用 poly 已足够**（本征失真 ≤ −120 dB，远低于可闻阈值），
且延迟与成本都更低；sinc 仅在需要更宽比率范围/更强抗混叠（例如高倍率或大偏差）时作为备用。

## 实测数字（Windows 本机，合成信号 + 发送端时钟偏差，`--offline` 测试）

- (a) fill 稳定：±200 ppm 漂移下 fill 落在 256±25 帧（1 ms 包锯齿幅度内），无欠载/丢帧；
   ±500 ppm 边界下 fill 有界（+67/−80 帧）、仍无欠载；
- (b) 比率估计：±200 ppm 时 `ppm_estimate` 收敛到 ∓199.6…∓200.0（≤1 ppm 误差，测试阈值 ±5 ppm），
   时间常数 ~10 s（阶跃）；
- (c) 整环 THD+N（收敛后 1 s，含亚赫兹级比率抖动）：**−74.7 dB**（poly）/−74.7 dB（sinc）；
   残差由 ±30 ppm、~2.3 Hz 的音高慢抖动主导（0.03 音分，不可闻），不是插值失真；
   插值本征失真见上表；
- (d) 对照：同样 −500 ppm 漂移下原路径（无 ASRC）出现欠载，+200 ppm 下抖动环溢出丢帧；ASRC 路径两项均为 0；
- (e) RT 零分配：`process`/`update_fill`/`status` 在计数分配器下 240 块 0 次分配（poly 与 sinc）；
- (f) 配置非法 fail-closed、`reset()` 清比率/积分/bypass、±`max_ppm` 限幅、planar/块语义有测试覆盖。

## 未覆盖（NOT_RUN）

- N150 真机长流（需设备窗口）；真实 PTP/媒体时钟锚定；
- 多声道 >8 的性能实测（>8 时 sinc 走通用路径，poly 成本线性）；
- 与 daemon 状态聚合的 `input.asrc` 映射（daemon 侧字段映射不在本任务写范围，见交接报告）。

## 测试

```powershell
cargo test -p khaen-asrc --offline        # 20 个测试（含 30 s 级时钟偏差仿真）
cargo clippy -p khaen-asrc --offline --all-targets --no-deps -- -D warnings
```
