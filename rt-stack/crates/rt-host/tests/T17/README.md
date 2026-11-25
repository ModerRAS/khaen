# tests/T17 —— T17（alsa）离线模型测试

- 归属：WBS T17 / team task-83，owner rt-alsa；写域仅
  `crates/rt-host/src/alsa.rs` + `crates/rt-host/tests/T17/` + `rt-stack/evidence/T17/`。
- 测试文件：`alsa_fake.rs`（**88 个 #[test]**，函数名统一 `t17_` 前缀；模块名 `alsa_fake_tests`；
  另含 1 个 `#[cfg(target_os="linux")]` 测试，Windows 不编译/不执行）。
- 运行（在 `rt-stack/` 下）：

`@
cargo test -p tspi-rt-host alsa_fake
`@

- 期望：`running 88 tests ... 88 passed; 0 failed`（executed test_count = 88 > 0，WBS 非空测试门）。
- 离线：只用 `FakeBackend`（T01 E1 MOTU 实测参数），不打开 `/dev/snd`、不需要声卡；
  硬件路径被 `HwOpenGate`（默认 Denied）+ `NoHwBackend`（未绑定）双重关闭。

## 为什么用 include! 而不是 public_registration.rs

`rt-stack/README.md` §3.1/§5 规定：`tests/public_registration.rs` 与公共注册**只由 integrator 写**；
cargo 只把 `tests/*.rs`（顶层文件）当集成测试目标，**不会自动发现** `tests/T17/` 子目录。

因此 T17 沿用 T16 的合法方式：`crates/rt-host/src/alsa.rs` 末尾

`@rust
#[cfg(test)]
mod alsa_fake_tests {
    include!("../tests/T17/alsa_fake.rs");
}
`@

本文件因此参与 lib 的单测构建，真实被 `cargo test -p tspi-rt-host alsa_fake` 执行
（executed = 82；测试路径 `alsa::alsa_fake_tests::t17_*` 含 alsa_fake）。
**integrator 不要**再把它加到 `public_registration.rs`（会重复注册/编译失败）。

## 测试覆盖（对照 core-v1 §5.4/§6.1/§6.4/§9.2/§11.9 与 WBS T17 DoD）

| 组 | 覆盖 |
|---|---|
| A 稳定设备身份 | `/proc/asound/cards` 解析/续行跳过、按 id/name 唯一定位（index 非硬编码 4→7）、NotFound/Ambiguous、`hw:4,0` 名称写缓冲、buffer 过小错误 |
| B duplex 协商 | MOTU 基线 20/22ch S24_3LE 48k period128/buffer1024 与 T01 E1 一致、format 优先级回退、请求 period 256→`ERR_BLOCK_SIZE`（不夹取）、rate 44.1k/96k 不支持、period 128→64 回退、buffer 取 period 整数倍/按上限回落、通道不足、format 交集空、planar/非 interleaved 拒绝、无公共 period |
| C actual 参数 | 冻结 `TspiHwParamsV1` 全字段/stride/flags/hash；**actual period 256/32 → `ERR_DEVICE_PARAMS`（I11 负向对照，不静默夹取）**；rate/channel 越界、方向间 period/buffer/rate/format 不一致、planar/F64 拒绝、F32 仅 file/fake、buffer<period 或非整数倍、period clamp 标记 `requires_new_epoch`、`period_bytes` 7680/8448、Toslink ch12/13 宽度校验 |
| D 硬件闸门 | gate 默认 Denied（None/0/false/garbage）、显式 1/true/yes/allow、`NoHwBackend` 恒 `BackendUnavailable`、session 在 gate Denied 时解析设备前即 `HwOpenDenied`、gate Allowed 仍不可用（未绑定）、fake 离线不受 gate 影响、**源码扫描证明无 libasound/snd_pcm_/dev/snd/dlopen/extern C/std::fs/File::open 且非 RT-MODULE、无 unsafe** |
| E §5.4 DeviceOwner | acquire/release 记录 `EV_DEVICE_OWNER_CHANGE`；Playback/Calibration 互斥（`ERR_DEVICE_BUSY`）；交接 before/after hwparams hash；交接超时→Stalled + `ERR_OWNER_TRANSITION_TIMEOUT`；session Running 期间 owner=Playback、stop 释放；播放↔校准完整交接往返（含 hash/epoch） |
| F 启动状态机 | 迁移序列 Open→CapsOk→Negotiated→Configured→StartOk、epoch=1；bad card 不 open；设备级瞬时 open 失败按 `startup_attempts` 重试后 Running；重试耗尽回滚 Closed + 释放 owner + `StartupFailed`；actual period 非 64/128 立即拒绝（不可重试只开一次）；Running 重复 start→`ERR_SWITCH_BUSY`；`start_with_width` 窄设备拒绝；stop drain/close/释放 owner；Muted 后完整重准入（epoch 2 + Reacquire） |
| G 恢复状态机 | device lost/xrun 进入 Recovering（静音、`IoDeviceLost`/`IoXrun`）；Suspend→Prepare→Resume 三阶段后 Running 且**新 epoch**；恢复中只允许 `write_silence`（8448 B 全零）普通 IO 被拒；阶段失败按 `recovery_attempts` 有界重试；耗尽→Muted（不自动恢复、静音可写、无新 epoch）；`note_rt_status` 映射（407→恢复、2xx/3xx→None、其他 4xx→Faulted）；Faulted 阻断 IO、可显式重启；两次恢复 epoch 单调 1→2→3；恢复中 stop 清理；恢复阶段顺序断言 |
| H fake backend | 调用日志/计数（read/write/silence/drain/recover/close）、apply 精确回显 plan、clear_calls 保留计数、session backend 访问器 |
| I 全链离线模型 | start→5×(read+write)→xrun→静音→3 步恢复（epoch 2）→3×(read+write)→stop；调用顺序断言 Open<Apply<Start<Recover<Drain；MOTU 基线 start_with_width 全字段与 T01 E1 证据一致（20ch/22ch S24_3LE 48k period128/buffer1024、stride 7680/8448、8 periods） |
| J 错误模型 | 17 个 `AlsaError` 全部映射 §9.2 冻结码（104/105/106/109/103/118/117/120/122/407）、名称唯一、可重试分类（仅 Xrun/DeviceLost） |
| K 平台 backend/Linux | `platform_backend_kind()` 与 target 一致（Windows=Fake）、`BackendKind::is_hardware`（LinuxAlsa=true/Fake=false）、gate 默认 Denied 对 Linux kind 同样生效、**ALSA errno 映射表**（EBUSY/ENODEV/ENXIO/EIO/EBADFD/EINVAL/ENOMEM/EPIPE/ESTRPIPE/ENOSYS/EOPNOTSUPP/未知/负值）、T01 E1 arecord 参考 buffer 16384 可协商、MOTU 基线常量（20/22ch/48k/period128/buffer1024、hw:4,0 名称） |

## Linux 真实后端（T90 target-specific `alsa` 0.12）

- `crates/rt-host/src/alsa.rs` 在 `#[cfg(target_os = "linux")]` 下实现 `LinuxAlsaBackend`：
  stable-ID → `@hw:<card>,<device>`@（板端即 hw:4,0）、capture 20ch / playback 22ch S24_3LE、
  period/buffer（128/1024 与 arecord 16384 参考）、xrun/suspended/disconnected 状态读取与
  Suspend/Prepare/Resume 恢复、close 释放；设备独占 = ALSA hw 设备 EBUSY + 进程内 DeviceOwner。
  非 Linux target 不编译该模块，继续用 fake backend。
- 编译证据（Windows 本机 cross check，仅类型检查不链接）：`cargo check --target x86_64-unknown-linux-gnu`、
  `aarch64-unknown-linux-gnu`、`aarch64-unknown-linux-musl` 均 exit 0；
  见 evidence/T17/linux-cross-check-*.log 与 PKG-CONFIG-SHIM.md（host 无 libasound2-dev，
  用临时 pkg-config shim 让 alsa-sys 构建脚本通过）。

## 限制（NOT_RUN）

- 板端真实开卡/实测、Linux link/运行：NOT_RUN（见 evidence/T17/NO-HW-OPEN.md §3）。
- 未上板、未打开 MOTU/UMIK/CueMix、未跑真实 xrun/恢复 syscall 尾延迟；S1 运行中的旧栈不受影响。
- 96k/多声道真实设备、PTP/AoIP、deadline/gate 实测：归 T19/T21/T45/T74。
- Miri/ThreadSanitizer：工具链未安装（本模块无并发共享状态；RT 并发在 T16/T20/T21）。
