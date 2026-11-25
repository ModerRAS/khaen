# tests/T44 —— 模式状态机私有契约测试

- 任务：WBS T44（deps T43/T42 已完成）；team task-123；owner rt-modes。
- 写域：crates/control/src/modes.rs + crates/control/tests/T44 + rt-stack/evidence/T44。
- 权威输入：rt-stack/spec/compat-matrix.md A01-A22/K39-K44/R10-R12、rt-stack/spec/core-v1.md
  §5.1/§5.2/§5.4/§9.2、DSP-HOST-DECISION §5/§6/§7、WBS T44 行与 §1/§7、rt-stack/README.md §3/§5。
- 测试文件：modes_contract.rs（52 tests）；运行：cargo test -p tspi-control modes。

## 注册方式（不改 public_registration.rs）

cargo 不自动发现 tests/<Txx>/*.rs，公共注册属 integrator 独占。因此本文件由
crates/control/src/modes.rs 文件末的 #[cfg(test)] 子模块 include! 编译执行：

    #[cfg(test)]
    mod modes_tests {
        use super::*;
        include!("../tests/T44/modes_contract.rs");
    }

本任务未改 Cargo.toml / Cargo.lock / lib.rs / public_registration.rs / CLI / README / ci。

## 覆盖分组

| 组 | 覆盖 |
| --- | --- |
| A01/K39 模式集合/互斥 | 5 模式大小写解析、未知值拒绝、4 个播放 profile 唯一、off 无 servo/unit、B1 不启 ptp4l/phc2sys |
| A15/K44 唯一 servo/epoch | 每模式唯一 ServoOwner（toslink/target=MotuFrame，aes67/dante=Ptp，off=None）；每次切换新 epoch（失败也新 epoch）；旧 grant 失效、第二 owner claim 必失败 |
| schema/hash 原子切换 | schema 常量绑定、config_hash 复用 T42 RuntimeConfig::fingerprint、target_hash=T57 TargetSpec::fingerprint、stale expected hash 零副作用拒绝（ERR_PLAN_STALE/exit 2）、失败只发布一个一致 off 状态 |
| A16/A03 失败 off 回退 | dante PTP 未锁 exit 5、aes67 缺库/daemon 启失败 exit 8、aes67 服务未 ready exit 6、ravenna 缺失 exit 3、停止失败 exit 4；回退后 mode=off/state=error/last_error/last_exit_code=9；LastGood 策略重启自研 last-good，restore 失败再回 off 并留 warning |
| A22/P11 CueMix | 切换/失败前后 CueMix diff=0；用户 Toslink 直通不动 |
| G-no-C 禁调用 C | ServiceUnit 枚举无 CamillaDSP；所有 profile/unit 名 0 命中禁用标记；legacy status camilladsp 恒 null |
| 模拟服务状态 A07/A11 | SimulatedServices 状态机（inactive/active/failed/not-present）、启动顺序 PTP->等锁->data->自检->新 epoch->claim servo、切换时 data->PTP 逆序停止、report steps/started_units 可见 |
| A06 串行/独占 | 在途 409（exit 10 / RtStatus 120）、calibration 独占（exit 4 / RtStatus 117）、同模式幂等 vs force、dry-run/preview 零副作用 |
| T42/T43/T19 接口 | ModeAck 字段域、RuntimeConfig::validate 全模式、EngineSnapshot 字段映射、AES67 静态 plan 过 T43 validate_aes67_capture+service、Dante InfernoEnv 过 T43 validate、PtpSource 与 AoipProtocol 期望值一致、EpochClock/ClockServo 唯一 owner 语义 |
| 状态 JSON | 内部 schema tspi-mode-state/v1、T42 MODE_STATUS_SCHEMA 兼容体、A04 legacy tspi-audio/v1 顶层字段（含 outputs 7 角色、camilladsp null）、JsonValue 往返 |

## 与生产的边界（NOT_RUN）

- 不打开 ALSA/MOTU/CueMix，不启 systemd unit，不建 RT 线程，不跑 PTP/AoIP 实流；
- 服务/时钟均为模型替身；真实 wiring 归 T26，真实板端播放/校准归 T71/T74；
- 本目录证据只证明本地 cargo test 真实执行与模型契约，不证明板端可用。
