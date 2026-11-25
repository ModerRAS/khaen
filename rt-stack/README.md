# TSPI RT-first DSP stack（rt-stack）

> 状态：**T06 scaffold 已落地 + leaf 逐步合入中**（2026-11-17，T90 批次更新）。唯一 Cargo workspace、10 个 crate、
> `rt-check` CLI 与离线 CI 入口由 T06 建立；leaf（T07/T08/T09/T10/T12/T13/T16…）由各自任务实现并在
> `evidence/<Txx>/` 留证。T90 批次：rt-host 增加 **Linux target-specific** real ALSA 依赖（Windows 不编译）、
> core events 补齐 v1.1 kind 36–40、T10/T60 私有集成测试注册。无板端/ALSA/RT 实测、无服务/模式/退役动作、
> 无 git commit。所有“已验证”仅指本地 `cargo test`/`cargo fmt`/`rt-check` 真实执行。

权威契约（只读输入，禁止改）：

| 文件 | 任务 | 用途 |
| --- | --- | --- |
| `spec/core-v1.md` | T02 | block/state/epoch/event/worker-ack/RT 错误/ABI/禁止行为（T06 消费 §3/§11/§12/§13） |
| `spec/fir-toolbox.md` | T04 | FIR 七家族 + 10 段模态 PEQ + target 导入（设计器 leaf 输入） |
| `spec/compat-matrix.md` | T05 | REST/WS/YAML/AoIP/保护/拒收矩阵（compat/control leaf 输入） |
| `reference/SPEC.md` | T03 | f64 参考语义、手算向量、fixture 格式（core 对拍基准） |
| `scratch/lead/planner/TSPI-RT-STACK-WBS.md` | T06 | 写域/依赖/DoD 唯一计划来源 |

T06 证据：`evidence/T06/manifest.md`（输入 hash、命令/exit code、executed test 数、未测项、契约偏差登记）。

## 1. 唯一 Cargo 根与目录结构

```text
rt-stack/
├── Cargo.toml / Cargo.lock        # 唯一 workspace 根（integrator 独占）
├── README.md                      # 本文件（integrator 独占）
├── ci/                            # 离线检查脚本 + lint fixture（integrator 独占）
├── crates/                        # 10 个 crate（见 §2）
├── evidence/<Txx>/                # 各任务私有证据（T06 为 evidence/T06/）
├── reference/                     # T03 产物（只读）
└── spec/                          # T02/T04/T05 产物（只读）
```

约束（WBS §1）：**只有** `projects/tspi-audio-dsp/rt-stack/Cargo.toml` 这一个 Cargo 根；禁止在 rt-stack 内
另建 crate/workspace/Cargo.toml（新建 crate 必须经 integrator 在 T06/T26/T32 串行合入）。

## 2. crate 表

| 目录 | package | 角色/模块 | leaf 任务（写域） |
| --- | --- | --- | --- |
| `crates/core` | `tspi-dsp-core` | `abi`（§11 ABI）、`rt`（类型/错误码/常量/§12.3 stub）、`events`（§8.2 kind）、`plan`、`direct`、`partitioned`、`workers`、`compile`、`nodes`、`schedule_plan` | T07 direct、T08 partitioned、T09 workers、T12 compile、T23 nodes、T56 schedule_plan |
| `crates/designer` | `tspi-fir-designer` | `linear`、`targets`、`min_fast`、`min_slow`、`apodizing`、`brickwall`、`corrected`、`nos`、`crossover`、`phase`、`modal_peq`、`report` | T10/T11/T33–T41/T55 |
| `crates/rt-host` | `tspi-rt-host` | `spsc`、`swap`、`admission`、`clock`、`format`、`fault`、`alsa`、`aoip`、`scheduler`、`content_map`（task-210：host 自持缺中置内容适配，见 `spec/compat-matrix.md` C09）；T26 另加 `src/main.rs`（bin） | T16–T22/T43/T45/T26（+ task-210） |
| `crates/compat` | `tspi-dsp-compat` | `ws`、`calibration`、`content`、`roles`、`channel_map` | T13/T24/T25/T57 |
| `crates/control` | `tspi-control` | `rest`、`modes`、`web` | T42/T44/T70 |
| `crates/modal-design` | `tspi-modal-design` | `ingest`、`timebase`、`target`、`solve`、`regularize`、`validate`、`artifact` | T48–T54 |
| `crates/calibration` | `tspi-calibration` | `job`、`sweep`、`capture`、`positions`、`peq_fit`、`phase_fit`、`compile_apply` | T60–T66 |
| `crates/file-runner` | `tspi-file-runner` | T14 自组织整个 `src/` | T14 |
| `crates/bench` | `tspi-bench` | `metrics`（T47 自组织整个 `src/`） | T47 |
| `crates/rt-check` | `tspi-rt-check` | lib + bin `rt-check`：`abi_layout`、`forbid`、`drop_audit`；`plan-hash`/`state-machine` 为入口占位 | T06 自有（T12/T21/T26 扩展命令） |

依赖方向（workspace.dependencies 统一管理）：`core` 为底；`designer`/`rt-host`/`compat` → core；
`control` → compat+rt-host+core；`modal-design` → designer+core；`calibration` → modal-design+designer+compat+rt-host+core；
`file-runner`/`bench` → core+rt-host；`rt-check` → core+rt-host。

第三方依赖（T90 起）：**仅 rt-host 的 Linux target** 使用 `alsa = 0.12`（crates.io 最新稳定 0.12.1，`Cargo.lock`
锁定精确版本；见 `crates/rt-host/Cargo.toml` 的 `[target.'cfg(target_os = "linux")'.dependencies]`）。
Windows/macOS 开发与 CI **不编译/不链接** libasound，T17 fake backend 保持离线可用；Linux 构建前置
`pkg-config` + `libasound2-dev`；离线构建先 `cargo fetch`。其余 crate 无第三方依赖。

## 3. 写域纪律（WBS §1/§7；所有 agent 必读）

### 3.1 integrator 独占（T06/T26/T32 串行写）

- `rt-stack/Cargo.toml`、`Cargo.lock`、`crates/*/Cargo.toml`；
- 各 crate 公共 `src/lib.rs`（**例外**：`file-runner`/`bench` 的整个 `src/` 属 T14/T47 写域）；
- 公共模块注册/公共类型/`tests/public_registration.rs`/CLI 注册/`README.md`/`ci/`；
- 任何 workspace 级 profile、lint、依赖、crate 增删。

### 3.2 leaf 任务只写（各自 WBS 写域）

- 自己的模块文件，例如 `crates/core/src/direct.rs`（T07）、`crates/designer/src/linear.rs`（T10）；
- `crates/<crate>/tests/<Txx>/<name>.rs`（私有测试；**不要**改 `public_registration.rs`，由 integrator 合入时注册）；
- `rt-stack/evidence/<Txx>/`（证据、日志、manifest）；
- 不得改 `spec/`、`reference/` 已有产物；不得碰板端/服务/模式（除 WBS 授权的 e2e-runner 专窗任务）。

### 3.3 禁止事项

- 禁止另建 Cargo 根/第二 workspace/复制 crate；禁止在 leaf 里 `cargo add` 依赖。
- 禁止并行跑 `cargo fmt --all`（会覆盖他人文件）；leaf 只 `rustfmt <自己的文件>`，integrator 统一 `cargo fmt --all`。
- 禁止修改他人写域文件；需要公共 API/模块/测试注册/依赖变更时，先回 lead/integrator。
- 禁止未经授权 `git commit`（T06 本轮未 commit）。

### 3.4 如何请求 manifest/公共 API 变更（T07–T74）

1. 在任务卡/消息中写清：task id、要改的文件、原因、期望 diff（新增依赖/模块/函数签名/测试注册）、影响 crate；
2. integrator 串行改 `Cargo.toml`/`lib.rs`/注册文件并跑 `cargo fmt --all` + `cargo test --workspace`；
3. 契约语义变更（字段/容量/错误码/ABI 布局）不得由 leaf 私自改：先按 core-v1 §15.3 升版并回 lead 裁决。

## 4. 构建与检查命令（离线）

在 `rt-stack/` 下：

```text
cargo test --workspace                 # 必须真实执行（T06 自证测试 > 0；leaf 验收时 executed test_count > 0）
cargo fmt --all --check                # 必须干净
cargo run -p tspi-rt-check --bin rt-check -- abi-layout
cargo run -p tspi-rt-check --bin rt-check -- forbid --root crates
cargo run -p tspi-rt-check --bin rt-check -- drop-audit --root crates
# 或一键：
pwsh -File ci/check.ps1 [-Offline]     # Windows
sh ci/check.sh                          # POSIX（CARGO_OFFLINE=1 强制 --offline）
```

Windows 开发/测试（本仓主要验证路径）不需要 libasound：`alsa` 依赖按 target 排除，`cargo test --workspace`
不会编译/链接它。Linux 真实 backend 构建前置：`pkg-config` + `libasound2-dev`（apt）；离线环境需先
`cargo fetch` 再 `--offline`（T90 起 lockfile 含 alsa 及其传递依赖）。

构建输出默认 `rt-stack/target/`（不提交）。可用 `CARGO_TARGET_DIR=<可重建外部目录>` 避免污染仓库；
T06/T90 的实测即使用外部 target 目录，未向仓库写 target/。

## 5. 测试约定

- `cargo test --workspace` 真实执行（T06 基线）：T06 自证 21 个（core 3 + rt-host 3 + rt-check 15）+ T90 cfg 门控 1 个；
  未注册/0 tests 占位**不能**用于 leaf 签收（WBS §6 要求 executed test_count > 0）。
- **2026-11-17 09:36（09:39 复跑一致）integrator T90 收口全量实测**：`cargo test --workspace --offline --no-fail-fast` = **43 test target / 811 passed / 0 failed**（exit 0）；
  `cargo fmt --all --check` exit 0；rt-check `forbid`/`drop-audit`/`abi-layout` 全 PASS（scanned 108/130/109）；Windows 构建未编译/链接 alsa
  （`cargo tree` 对 host 与 x86_64-pc-windows-msvc 均 0 条 alsa，对 x86_64/aarch64-unknown-linux-gnu 各 2 条）。后续 leaf 合入后数字会增长，以 `evidence/<Txx>/` 为准。
- 注册状态（T90）：`T10`（designer，3 文件 34 tests）与 `T60`（calibration，5 tests）已在
  `public_registration.rs` 注册；`T07/T08/T09/T12`（core）、`T13`（compat）、`T48`（modal-design）用
  `tests/<Txx>/main.rs` 自动发现；`T16/T17`（rt-host）、`T11/T33–T39/T41`（designer）等用
  `src/*.rs` 内 `#[cfg(test)] include!` 自注册，**不要**再写入 `public_registration.rs`（会重复注册/编译失败）。
- leaf 测试放 `crates/<crate>/tests/<Txx>/<name>.rs`，测试函数名/模块名带任务关键字
  （例如 `cargo test -p tspi-dsp-core direct`）；合入时由 integrator 在 `public_registration.rs` 加
  `#[path = "<Txx>/<name>.rs"] mod ...;`。
- T06 自有测试：`crates/core/tests/const_contract.rs`、`crates/rt-host/tests/abi_layout.rs`、
  `crates/rt-check/tests/{forbid_fixtures,drop_audit_fixtures,cli_smoke}.rs`。

## 6. rt-check 子命令（core-v1 §12.2）

| 子命令 | 状态 | 内容 |
| --- | --- | --- |
| `abi-layout` | T06 已实现 | §11 全部结构 size/align/offset + §3 时限/容量向量 |
| `forbid` | T06 入口 + fixture | 扫描 RT-MODULE 文件 F-01..F-44 子集（正 fixture PASS，4 类负 fixture FAIL） |
| `drop-audit` | T06 入口 + fixture | §13.2 类型清单 needs_drop==false + RT 模块 Drop/owned 类型扫描 |
| `plan-hash` | 占位（exit 3） | owner T12/T13：plan 打包与 graph_id 规范化 |
| `state-machine` | 占位（exit 3） | owner T21/T26：状态机/事件完整性模型测试 |

RT-MODULE 标记：DSP 纯 RT 模块文件头写 `//! RT-MODULE: ...`（同时按 R-DROP-14 声明禁止析构/分配/锁/syscall）。
混合模块（同文件含 [RT] 与 [NONRT]，如 `core/src/rt.rs` 的常量/静态断言区、`rt-host/src/clock.rs`）
由代码评审 + `drop-audit` 类型清单覆盖，不做文件级禁止符号扫描（常量 `const _: () = {...}` 静态断言不属运行期）。

## 7. T06 scaffold 契约偏差/澄清登记（需 T02 编辑者确认，非语义变更）

| ID | 现象 | T06 处理 | 影响 |
| --- | --- | --- | --- |
| O-1 | core-v1 §3.2 `TBLOCK_64_48K_NS=1333333` 是 floor，而 `TBLOCK_128_48K_NS=2666667` 是 ceil；两行取整口径不一致 | 保留字面值；`gate_ns` 用 floor（更严）；`t_block_ns_ceil(48k,64)=1333334` 由公式给出并在测试中单独断言 | 仅常量注释；不放宽任何门 |
| O-2 | core-v1 §12.3 `core::rt::master_block` 签名需要 `&RtControl`，§13.2 又把 `RtControl` 位置写在 `rt-host::swap`（core→rt-host 会成环） | `RtControl` 规范定义在 `core::rt`，`rt-host::swap` 重导出；两条路径均存在 | 类型语义/needs_drop 不变；建议 T02 升 1.0.x 文字澄清 |
| O-3 | §13.2 表把 `BlockIo<'a>`/`WorkerRtCtx<'a>` 标 Copy，但其字段含 `&mut [f32]`/`&mut TspiWorkerStatsV1`，Rust 不可 Copy | 按 §12.3 冻结签名实现，不 derive Copy；`needs_drop == false` 由测试保证 | 无 RT ABI 影响；Copy 列澄清 |
| O-4 | §12.3 把 `kernel_direct/kernel_partitioned` 列在 core::rt，WBS 又让 T07/T08 实现 `direct.rs/partitioned.rs` | T06 在 `core::rt` 放签名 stub；leaf 在自己的模块实现后由 integrator 在 T26 调整 re-export/委托 | 公共注册只 integrator 改 |
| O-5 | `plan-hash`/`state-machine` 的完整实现依赖 T12/T13/T21/T26 | T06 只提供可运行入口，明确 exit 3 与 owner；不冒充已实现 | CI 不调用这两个命令 |
| O-6 | core-v1 v1.1 §8.2 追加 event kind 36–40（EV_STARTING/PLAN_ABORTED/EPOCH_ABORTED/ACK_SLOT_EXHAUSTED/CROSSFADE_BUDGET_REJECT），T06 `events.rs` 原仅 35 | T90 按 spec v1.1.1 全量补齐 36–40 常量 + `event_name` 导出 + 编译期断言（任务卡写 36–38，按 spec 同步到 40，不静默漏项） | 仅新增常量/映射，不改 1–35 编号；leaf 实现可直接引用 |
| O-7 | T06 原口径“无第三方 crate（离线构建）” | T90 起 Lead 批准 **target-specific** Linux `alsa` 依赖（0.12.1，Cargo.lock 锁定）；Windows 开发/CI 不编译；真实 backend 接线归 T17-follow-up/T26；Linux 需 `pkg-config`+`libasound2-dev` | 打破“完全离线无第三方”初始口径，已在 README §2/§4、ci/README 与 evidence/T90 登记 |

## 8. 未测项（NOT_RUN，禁止当已验证）

- 未实现任何 leaf（direct/partitioned/workers/designer/compat/control/calibration/modal/file-runner/bench）；
- 未上板、未打开 ALSA/CueMix、未跑 RT/benchmark/cyclictest/osnoise/soak；
- 未做 96k/多声道真实设备、PTP/AoIP/Dante、H16/UMID 实测；
- 未执行退役/服务切换（G-no-C 属未来 T71/T72）；
- `plan-hash`/`state-machine` 未实现（exit 3）；leaf crate 的 0 tests 占位不算验收；
- T06 未 git commit（按任务纪律）。


## 10. T26 host 入口（fake/file 端到端；未上板）

- crates/rt-host/src/main.rs（bin tspi-rt-host）：唯一 I/O owner 的 host 入口集成。命令：
  - tspi-rt-host selftest：4 worker DAG 与 T07 直算 bit-exact（stereo identity + 2x2 offdiag 两图）+ admission/format 前置校验 + fake 闭环；
  - tspi-rt-host fake-io --blocks N：fake 设备闭环（合成 2ch → B1 串行 → B2 四线程并行 → B3 串行 → T22 f32 设备字节；T16 有界 SPSC desc 环、T19 每块 tick 预算计数）；
  - tspi-rt-host admission-check：T18 plan_memory 容量/预算 + T22 format 契约自校验。
- rt-check host-file --work DIR --frames N（或 --session FILE --output FILE）：file 端到端 WAV → T14 session/plan（BlockSource 同源切分）→ T09 4-worker DAG → 真实 WAV。rt-host 不能反向依赖 file-runner（会成环），故 file 命令入口在 rt-check。
- 本 T26 批次未接：T20 swap/crossfade、T21 fault 事件导出、T42/T44 REST/WS+mode、T45 内核亲和/IRQ 实写、T25/T23/T56 的整链入口；真实 ALSA/MOTU 打开与板端 RT/soak 亦未做（NOT_RUN）。
- **T155 最终接线（部署前冻结）**：`tspi-rt-host run --config rt-host.toml`（schema tspi-rt-host/v1）：建图 / ALSA owner 契约(fake) / 4 worker DAG + T45 调度计划逐块 gate / T20 swap all-ack / T21 fault 事件+watchdog / Web 端口（external 或 spawn 探测 /healthz 200）/ 优雅退出（SIGINT/SIGTERM、--blocks、--duration-ms、TSPI_HOST_SAFE_MODE=mute 保护输出）；`tspi-rt-daemon serve --config rt-daemon.toml`（schema tspi-rt-daemon/v1）：T42 REST + T44 modes + T70 Web/WS 同端口，`/healthz` 200、`/` 8656 B 页面。配置模板 deploy/config/rt-host.toml.example、rt-daemon.toml.example；aarch64 gnu 双 bin release `t155-final-20261117`（tspi-rt-host 768,384 B sha256 e011745c…；tspi-rt-daemon 1,210,296 B sha256 009a110e…；manifest 11 files 校验 ok）。
- **T155 仍未接/未测**：真实 ALSA PCM open/duplex（backend=fake only）、内核亲和/FIFO/IRQ 实写（T45 只计划不写内核）、T25/T23/T56 整链入口、板端安装/启动/真实设备/浏览器/真实 systemd（NOT_RUN）；T20 crossfade 双图仅 swap ack 层、T21 事件未接 UI 消费、daemon 用 MemoryEngine 替身（真实 host engine 注入属 follow-up）。

## 9. 变更记录

| 日期 | 变更 |
| --- | --- |
| 2026-11-17 | T06：建立唯一 workspace + 10 crate scaffold + rt-check（abi-layout/forbid/drop-audit）+ ci/ + 本 README；cargo test --workspace / cargo fmt --all --check 真实执行（见 evidence/T06/manifest.md）。 |
| 2026-11-17 | T26（task-132）：crates/rt-host/src/main.rs host 入口（bin tspi-rt-host：selftest/fake-io/admission-check，4 worker B2 四线程并行、T16 SPSC/T19 tick/T22 format/T18 admission 串行接线）+ rt-check host-file（T14 file WAV→4-worker DAG→WAV 端到端）+ 集成测试；验证见 evidence/T26/manifest.md。T20/T21/T42/T44/T45/T25/T23/T56 入口接线与真实 ALSA/板端仍 NOT_RUN。 |
| 2026-11-17 | **T155（task-155）最终接线**：tspi-rt-host run --config（建图/ALSA owner 契约/T45 调度计划/T20 swap all-ack/T21 fault 事件/T70 Web 接线/优雅退出/safe-mode mute）+ tspi-rt-daemon serve --config（T42 REST+T44 modes+T70 Web/WS）；deploy 新增 rt-host.toml/rt-daemon.toml 模板并冻结 env 契约；aarch64 gnu 双 bin 重建（tspi-rt-host 768,384 B sha256 e011745c…、tspi-rt-daemon 1,210,296 B sha256 009a110e…，release t155-final-20261117，manifest 11 files ok）；全 workspace 57 target/1990 passed/0 failed，fmt/rt-check 全绿。见 evidence/final-wiring/ |
| 2026-11-17 | T90 批次：rt-host 增加 Linux target-specific `alsa=0.12` 依赖（cfg 门控 + Windows 不编译）+ core events v1.1 kind 36–40 + T10/T60 私有集成测试注册 + README/ci 更新；验证见 evidence/T90/manifest.md。 
| 2026-11-17 | **INTEGRATOR-BATCH2（task-105）**：core::rt `TICK_READS_PER_BLOCK_MAX` 2→3（I04，加断言测试）；`TspiWorkerAffinityV1`（32B/align8）+ `TspiPlanHeaderV1` v1.1 affinity/ctl 字段提升到 `core::abi`（repr(C) 合法对齐版 **errata E1** 待 T02 确认，abi-layout scanned 109→120）；`AdmissionEnv/Report` 合并 sched 字段（cpu_set_mask/allowed_cpu/actual_cpu/expected_workers/sched_priority/irq_dependency_mask/ack_mask），validate 加 expected_workers 一致性闸门；T45 include! 自注册确认（不入 public_registration）。验证见 `evidence/INTEGRATOR-BATCH2/manifest.md`：全量 43 target/964 passed/0 failed（T55 收口后终态）、fmt exit 0、rt-check 3 PASS ||
