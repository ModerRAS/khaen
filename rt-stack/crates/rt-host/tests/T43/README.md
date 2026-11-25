# tests/T43 —— AoIP 端点接入私有测试（khaen-rt-host）

- 任务：WBS T43 / team task-110（aoip-endpoints，deps T17/T19/T05）。
- 实现文件：`crates/rt-host/src/aoip.rs`（[NONRT]/[IO]）；本目录只放私有测试。
- 注册方式：`src/aoip.rs` 末尾 `#[cfg(test)] mod aoip_fake_tests { include!("../tests/T43/aoip_fake.rs"); }`。
  **不是** integrator 的 `tests/public_registration.rs` 注册（同 T16/T17/T18/T19/T22/T45 范式），
  integrator 不要再注册（会重复）。
- 运行：`cargo test -p khaen-rt-host aoip_fake`（WBS T43 DoD；全部用例名 `t43_` 前缀）。

## 覆盖

| 组 | 内容 |
|---|---|
| 常量/profile | AES67 48k / Inferno 96k、S32_LE 设备容器、H16 12/16、PTX 唯一来源、冻结错误码 |
| H16 容器 | 官方 16ch 顺序（1 L … 16 Rtr）、逻辑 12 与容器 16 分开、非逻辑 8/9/12/13、显式 flow 配置 |
| 角色选择 | 显式 ignore/拒收、目标角色未订阅拒收、非逻辑通道 policy、禁止静默下混 |
| AES67 SDP | 只读解析 L24/L16/48k/动态 PT/ptime/组播/ts-refclk PTPv2；速率/编码/通道/连接/时间基准拒收 |
| 流计划 | SDP ↔ 显式 flow 配置交叉校验（flow 数/每 flow 通道/总通道/域/重复媒体） |
| 端点规格 | RAVENNA 稳定 card 解析（`/proc/asound/cards`）、`hw:<card>,0`、inferno 命名 PCM、pipe 帧长/对齐 |
| Inferno | device id / usrvclock 路径 / 绑定网卡 / 96k / rx 通道数环境校验 |
| 失流 fake | T_block 门限、首包超时、停滞→整块静音、同 epoch 恢复、持续失流→新 epoch、PTP 失锁、EOF |
| 集成 | T19 `EpochClock` 新 epoch 与 `reopen(epoch)`；T17 `ResolvedDevice` 复用 |
| 纪律 | 类型 needs_drop==false、源码无 unsafe/FFI/文件/进程/网络调用 |

## 为什么 include!

Rust 不会把 `tests/<子目录>/*.rs` 自动当 test target 编译；leaf 又不得改
`tests/public_registration.rs`（integrator 独占）。故沿用 T16/T17/T18/T19/T22/T45 的
`src/*.rs` 内 `#[cfg(test)] include!` 自注册：测试进 lib 单测构建，可按 `aoip_fake` 过滤，
且不改公共注册文件。

## 离线约束

测试全部用脚本化 nanos/帧数/字符串驱动：不打开 ALSA/pipe、不启动 aes67-daemon/statime、
不改 CueMix、不连板端；真实 AES67/Dante 端到端仍为 NOT_VERIFIED（H16 未到货 + 板端
daemon 缺库 / statime clock identity panic，见 evidence/T43/MANIFEST.md §7）。
