# 实施计划

状态：**已实施（2026-11-22）**。现场复测 `NOT_RUN`。未提交、未部署。

- [x] 1. 抽头按可测尾巴求解
- `rt-stack/crates/mimo-design/src/emperor.rs` 删除把抽头截到 8192 的路径
- `taps = D12 + ceil(可测尾巴秒数 × 12000)`。可测尾巴是主峰到控制频段包络落入噪声底的时间
- 代价保持 `|| W (h_p ∗ (δ_D + g_pp) + Σ h_s ∗ g_sp − d) ||² + λ Σ ||g_sp||²`。导出的 `g_primary` 不含 `δ_D`
- 从喇叭允许为空。上沿缺省 150 Hz，高于 150 Hz 返回错误。`t_keep = 20 ms`，`w_tail = 10`，`λ0 = 0.01`，`λ` 只加在从路径
- 全部音箱共用一个预衰减，等于其中最高正增益的相反数，G 不超过 20。各音箱一起衰减，相对声压不变。超过 +20 dB 整套返回错误，不削顶。能量衰减到不了 −25 dB 返回错误；到 −35 dB 记 T30，否则记 T20
- 一套音箱按到座位的实测路程差对齐，声速 343 m/s。口头估算的距离不进测量、不进播放。低频建立时间不拿来补这段延迟。延迟加在直通和侧路上，不写进系数
- 抽头或分区数超过当前侧路上限时 `engine_playable = false`，系数保持完整抽头
- _需求: 1, 2, 3, 4.1, 4.4_

- [x] 2. 求解测试与报告
- 合成单模态：上沿 150 Hz 与 80 Hz 都能求解，151 Hz 被拒绝
- 未选从喇叭时不导出从路径；选中的从喇叭在上沿内有系数，上沿以上的主修正和从修正接近 0
- `g_primary` 的首点不是 1
- 衰减只到 −25 dB 时指标为 T20，到 −35 dB 时为 T30，不到 −25 dB 时求解失败
- 抽头等于可测尾巴对应的样本数加 `D12`。`delta_sys_48(95, 120)` 在抽头变长后仍是 574
- `report.json` 写出单座位、上沿、`t_keep`、`w_tail`、`λ0`、从喇叭、可测尾巴、抽头、分区数、`Δsys`、T20 或 T30、`engine_playable`、座位半径 `343 / (10 × 上沿)`。声明只有 `PREDICTED_ONLY` 或 `REFUSED`
- 系数 sha 以 `b56b3426` 开头时拒绝标成皇帝位衰减。不改 `proto_g.py`，不实现补谷、电声吸声、前后墙低音阵列或第一种模态均衡
- _需求: 2.3, 2.4, 4.3, 4.4, 6.2, 6.3_

- [x] 3. 侧路抽头上限与分区上限
- `MIMO12K_MAX_TAPS` 从 4096 改为 24000。24000 / 12 kHz = 2.0 s，去掉 `D12 = 120` 后有效支撑 1.99 s
- `rt::MAX_PARTITIONS` 从 256 改为 1500。块长 64 时 `ceil(24000 / 16) = 1500`，块长 128 时 `ceil(24000 / 32) = 750`
- `direct` 的抽头上限保持 4096。`prepare_mimo12k_branch` 对 24000 点仍要求 plan 恒等，`Δsys` 仍为 574 个 48 kHz 样本
- `mimo12k.rs` 模块注释去掉「4096 点覆盖 T60」
- soak 结束前不把改过的引擎装到 sender-host、N150 或 MOTU
- _需求: 4.2, 4.3, 6.1, 6.4_

- [x] 4. 现场脚本给出复测结论
- `rt-stack/tools/analysis/emperor_focus_field.py` 在没有 `--play` 时只跑求解示例的 `--self-check`
- `--play` 必须同时带 `--rec` 和 `--yes-this-bench`，否则退出且不出声
- 扫频 48 kHz、20–500 Hz。顺序为逐喇叭测量、反卷积、求解、渲染、录恒等、录修正、对两条复测脉冲响应调用 `score`
- 变短打印 `FIELD_SHORTER` 并以 0 退出；没变短打印 `FIELD_NOT_SHORTER` 并以 4 退出；到不了 −25 dB 打印 `FIELD_UNFALSIFIABLE` 并以 3 退出
- 结论不读 `predicted_tail_ratio`。没变短或不可证伪之后不再求解
- `--supports` 缺省为空。通道表由 `--map` 传入
- _需求: 5, 6.4_

- [x] 5. 测试与证据
- `TEMP`/`TMP` 指向 `C:/WorkSpace/Claw/scratch/lead/testtmp`
- 跑 `cargo test -p tspi-mimo-design emperor`，以及侧路布局测试：24000 点在块长 64 和 128 下都能建布局，`Δsys = 574`
- 日志和 `field_summary.json` 放进 `rt-stack/evidence/emperor-focus-field/`。现场还没跑的写成 `NOT_RUN`
- 不提交、不 push、不部署
- _需求: 5, 6.4_
