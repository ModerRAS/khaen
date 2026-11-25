# tests/T45 —— scheduler_fake（WBS T45 私有测试）

- 文件：`scheduler_fake.rs`（63 个 `#[test]`，全部 `scheduler_fake_` 前缀）。
- 注册方式：`crates/rt-host/src/scheduler.rs` 末尾
  `#[cfg(test)] mod scheduler_fake_tests { include!("../tests/T45/scheduler_fake.rs"); }`。
  cargo 不自动发现 `tests/<子目录>/`；`tests/public_registration.rs` 属 integrator，
  **不要**再写入（会重复注册/编译失败）。
- 运行（DoD 原文）：

  ```text
  cargo test -p khaen-rt-host scheduler_fake
  ```

  过滤词 `scheduler_fake` 命中模块路径 `scheduler::scheduler_fake_tests::*`，效果等于全部 63 个用例。

## 覆盖（详见 evidence/T45/MANIFEST.md §4）

| 组 | 内容 |
|---|---|
| A 布局/常量 | §11.13 KhaenWorkerAffinityV1 32B/align8 偏移；needs_drop==false；FIFO 70/80/90；IRQ 位义；tick 预算 |
| B 实测解析 | /sys CPU 列表（0-3,6,7）；/proc/cmdline isolcpus/nohz_full；lscpu -e=CPU,CORE,SOCKET,ONLINE,...；/proc/irq/N/smp_affinity_list |
| C 拓扑快照 | mask/core 查询、IRQ 注入、实测集合一致性负向 |
| D 准入请求 | profile 4/2、优先级排序（dsp<io<=irq）、tick/spin/cutoff 未标定拒收、CPU 槽/IRQ 位负向 |
| E 正向计划 | 4 worker 逐 worker CPU/FIFO/IRQ 依赖/isolated flag；2 worker 独立 profile（ACK_MASK(2)=0x3、affinity 表 64B、hash 不同）；§12.3 sched 报告字段；affinity 表打包 |
| F 负向计划 | 拓扑缺失/离线/不允许/重复/SMT 同核/IRQ 缺证据/IRQ 在 DSP CPU/未标定参数 → 115 |
| G 失核 | `gate_block` 整块/整图拒收（LostWorkerCpu/LostWakeupIrq/IrqOnDspCpu/StaleTopology）；`two_worker_re_admission` 独立 2 worker 重新准入（不足 2 健康 CPU → Err，禁止半图） |
| H 不写内核 | `KERNEL_WRITES_PERFORMED==false`、`apply_plan_to_kernel` → ERR_UNSUPPORTED(122)、源码扫描无内核设置/FFI token |
| I 接口对齐 | T09 `ack_mask`/`RT_WORKERS_FALLBACK`/WORKER_DEFAULT_*；T18 `AdmissionEnv.cpu_set_ok` → `admission::validate` 115；实测快照→计划端到端；affinity_hash 确定性与 golden |

## 夹具说明

测试用 `SYNTHETIC(合成)` 8 CPU 快照（DSP_CPUS 0..3 + 控制/IRQ 4..7），格式与
WBS §7 / S0-02/S0-03/S0-05 被动命令输出同构；**真实板端采集 NOT_RUN**（见
`rt-stack/evidence/T45/MANIFEST.md` §8）。测试不打开设备、不写内核、不依赖 Linux。
