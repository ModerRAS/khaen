# tests/T18 —— T18（admission）离线模型测试

- 归属：WBS T18 / team task-92，owner rt-admission；写域仅
  crates/rt-host/src/admission.rs + crates/rt-host/tests/T18/ + rt-stack/evidence/T18/。
- 测试文件：admission_models.rs（66 个 #[test]，函数名统一 t18_ 前缀；模块名 admission_tests）。
  Windows 开发机执行 65 个（Linux-only 的 platform 页大小探测测试按 cfg 不编译）；Linux 上为 66 个。
- 运行（在 rt-stack/ 下，DoD 原文）：

~~~text
cargo test -p tspi-rt-host admission
~~~

- 期望：running 65 tests ... 65 passed; 0 failed（executed test_count = 65 > 0，WBS 非空测试门；Linux 上 66）。
- 离线：只用 MockMemory 注入 mlockall/RLIMIT/预触成功与失败；**不调用真实 mlockall/getrlimit、
  不打开 ALSA/设备、不需要声卡或 root**。真实 Linux 平台后端（extern C mlockall/getrlimit/sysconf）
  仅编译、本机未执行（NOT_RUN，见 evidence/T18）。

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T18/ 子目录。

因此 T18 沿用 T16/T17 的合法方式：crates/rt-host/src/admission.rs 末尾

~~~rust
#[cfg(test)]
mod admission_tests {
    include!("../tests/T18/admission_models.rs");
}
~~~

本文件因此参与 lib 的单测构建，真实被 cargo test -p tspi-rt-host admission 执行
（executed = 65；测试路径 admission::admission_tests::t18_* 含 admission）。
integrator 不要再把它加到 public_registration.rs（会重复注册/编译失败）。

## 测试覆盖（对照 core-v1 §5.1/§3/§4.1/§4.2/§9.2/§9.3/§11.13 与 WBS T18 DoD）

| 组 | 覆盖 |
|---|---|
| A 常量 | MCL_CURRENT=0x1 / MCL_FUTURE=0x2 / 组合=0x3 与 complete 判定；RLIMIT_MEMLOCK=8、RLIM_INFINITY；预创建线程数 8/6 与栈总预算 4 MiB/3 MiB；控制面结构 70,528 B + 预留 64 KiB；图 scratch/历史公式向量 |
| B 算术/预触原语 | align_up（0/边界/溢出/零对齐）、pages_for 边界、pre_touch_bytes 逐页写触（每页首字节 0xA5、页内非首字节保持不变、空/零页安全） |
| C 容量模型 | canonical 4.1.2 图逐项公式（capture/playback/partial/quarantine/silence/desc/arena/graph/双图/stacks/control/total=8,981,760）；B=64 半 payload 但历史不变；2-worker 独立 profile；96k 同内存；表/ctl_meta 计入；coeff 声明值 fail-safe；稀疏图；5 区域对齐与合计 |
| D 纯校验 | fs/B/workers 非法与 plan-env 不一致（ERR_RATE/105/115）；rt_privilege/clock/cpu_set 闸门（114/116/115）；precision（108）；n_in/n_out（106）；path/diag/role 表超限（113/107）；max_taps>16384（110）；canonical 通过 |
| E 成功路径 | 报告 plan_version/arena/mlocked/stack/workers/graph_id 精确值；mlockall 恰一次且 flags==CURRENT|FUTURE；5 区域固定顺序、字节=各区域页对齐值、合计=mlocked_bytes；报告 Copy/无 Drop；失败后重试成功 |
| F 预算/RLIMIT | 声明预算 < 需求 → 113（且无后端副作用）；== 需求通过；声明超 limit → 114；RLIMIT=0 → 114；后端读 limit 失败 → 114；env limit 太短 → 114；env limit 恰好通过；无限 limit 通过；页大小 0/非 2 幂 → 114（mlock 前拒绝） |
| G mock 失败拒启 | 校验失败零后端调用；mlockall 失败 → 114 且不预触；后端任意错误码归一 114；5 个预触位置逐一失败 → 114 且失败后不再继续；5 个区域逐一失败；多种失败都不产生 AdmissionReport |
| H 构建门/平台 | production_build_gate 真值表（生产 debug → 122）；非 Linux64 admit() fail closed（122）；Linux64 平台后端页大小探测（仅 Linux 编译） |
| I 细节 | RegionId 名称/顺序（双图相邻 current→pending）；Mock 预触记录边界；64 KiB 页对齐改变预算不改语义 |

## 限制（NOT_RUN）

- 真实 Linux mlockall/getrlimit/sysconf：代码已实现（Linux64 cfg），**本机 Windows 未执行**；
  真实 VmLck/RLIMIT 生效、root/CAP_IPC_LOCK 权限、systemd limits.d 生效核验归 T26/T29 板端窗口。
- 真实 RT 线程创建/栈绑定/FIFO 亲和：归 T26/T45；本模块只做栈预算模型与超限拒启。
- 真实 arena mmap/预触/跨 attempt 复用的实际地址稳定性：归 T26（本模块提供 pre_touch_bytes 原语）。
- 板端/ALSA/服务/模式/退役：未触碰（S1 不受影响）。
- Miri/ThreadSanitizer：未安装；本模块无并发共享状态。
