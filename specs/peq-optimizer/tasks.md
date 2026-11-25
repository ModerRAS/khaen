# 实施计划

状态：**已实施（2026-11-16）**。`khaen-calibrate` 42 项测试通过，`khaen-audio-web` `cargo check` 通过。分频器设计与 ART MIMO 系数不在本清单内。分析文件里的 `peak_db` 是脉冲幅度，不是文件 dBFS；削波门限由 `position_usable(snr, peak)` 提供，加载测点时只按 SNR < 25 dB 排除。

- [x] 1. 滤波器模型改为可校正的双二阶
- `Peq` 增加 `type`（缺省 `peaking`，另含 `low_shelf` / `high_shelf`）
- 求值函数给出复数 `H(f)`，不再把 Q 夹到 10、增益夹到 ±18
- 删除「让 ΣH 模仿实测」的 `fit_peq` 路径；`fit_synthetic_peaking` 改为断言校正方向
- _需求: 1, 2_

- [x] 2. 目标曲线
- 新增 `target.rs`：`khaen-target-curve/v1` 的加载、校验、对数频率插值
- 内置 `flat`；`--target flat|id|@file`，缺省 `flat`
- 曲线只进入报告，不进入校准 YAML 的应用字段
- _需求: 4_

- [x] 3. 频响网格与倍频程平滑
- `fr_from_h` 支持 `--fr-points` / `--fr-oct`，缺省仍为 300 点
- 模态路径另写不低于 1/48 oct 的频响，检测用 1/24 oct 平滑
- 宽带用 1/6 oct 平滑；不再用 7 点滑动平均做检测
- _需求: 3, 5, 6_

- [x] 4. 测点
- `--positions` 读取 `<run>/positions/<name>/`
- analysis 记录 `meta.position`（名称、可选权重）
- 没有该目录时，现有 analysis 视为权重 1 的 `listening`
- SNR < 25 dB（200 Hz–8 kHz）或峰值 > −1 dBFS 的测点标 `invalid` 并排除
- 单测点仍出滤波器，模态置信度 `low`
- _需求: 3_

- [x] 5. 模态均衡
- 突出度、只切、`max_cut=12`、`target_margin=1`
- Q 由 −3 dB 带宽估计；窄于网格时标 `q_unresolved` 并采用网格最细 Q
- 多测点：幅度平均、6 dB 降权、至少两处同向
- 搜索带为 `[20, 250]` Hz 与通道工作频带的交集；<1/6 oct 合并；每通道 ≤10 段
- _需求: 5_

- [x] 6. 宽带优化
- 功率平均后开方；Q 0.3–2.0；1/12 oct 网格；单段 |gain| ≤ 6 dB
- 倾斜超过 1 dB 时最多一个 low_shelf 和一个 high_shelf（Q=0.7），计入 12 段预算
- `--boost-cap-db` 缺省 0，大于 +3 时按 +3 并 warning
- 与模态段相距 <1/6 oct 的候选跳过
- `--match` 声明的组在 1/3 oct 差 > 3 dB 时联合重拟合一次，仍超差只 warning
- 搜索带为 `[80, 16000]` Hz 与工作频带的交集
- _需求: 6_

- [x] 7. 合并、预测与 pair 回退
- 模态与宽带按频率升序合并，总段数 ≤22，重算预测
- 写出预测表（频率、目标、预测、逐测点、残差）和报告字段 `optimizer`
- `channel_h` 加上双二阶相位；声明了 `--pairs` 时，使 pair 失败的段回退
- _需求: 5, 7_

- [x] 8. 接上 `fit` / `merge` 的命令行
- 无新参数时也走校正（目标 flat、不抬升），不再模仿实测
- `--band` / `--hp` / `--lp` 只接收工作频带，不在程序里选择分频
- _需求: 1, 5, 6_

- [x] 9. 契约 v1.1 与 Rust 校验
- `CONTRACT.md` 增加 v1.1：shelf、Q ≥ 0.1 且有限；v1 旧规则保留；校验命令写 `khaen-calibrate validate`
- 新增 `check.rs`：独立 RBJ 与 schema 校验。`validate` 拒绝未知 type；只含 Peaking 且 Q ≤ 10 时注明 v1 可读
- `check-pair` 用同一套复数响应做 Δφ、CD、陷波、纹波
- `web/src/channelmap.rs` 的 `PeqBand` 接受 type，并按 v1.1 放宽 Q
- CamillaDSP 片段把 type 映射为 `Peaking` / `Lowshelf` / `Highshelf`
- 不新增、不调用 Python。现有 `.py` 不作为本功能的验收入口
- _需求: 2, 7_

- [x] 10. 合成夹具与验收测试
- 8 个已知模态（至少两个 Q>10）+ 倾斜 + 噪声：频率 ±3 Hz、Q ±30%、模态增益 ≤ 0
- 深谷且 boost cap = 0：无正增益
- `flat` 与一条非平直曲线：1/6 oct 残差 RMS ≤ 1 dB
- pair 被破坏时回退；`khaen-calibrate validate` 的 v1 / v1.1 用例
- `peq.rs` 与 `check.rs` 的幅度差 ≤ 0.1 dB
- _需求: 7, 8_
