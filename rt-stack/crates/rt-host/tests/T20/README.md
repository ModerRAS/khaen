# tests/T20 —— swap（block-version+4 worker ack / quarantine / nonRT 回收 / crossfade 预算）

- 私有测试：`swap_models.rs`（91 个 `#[test]`，全部 `t20_` 前缀）。
- 注册方式：**`include!` 自注册**（`crates/rt-host/src/swap.rs` 末尾
  `#[cfg(test)] mod swap_tests { include!("../tests/T20/swap_models.rs"); }`），
  因为 cargo 不自动发现 `tests/<子目录>/*.rs`，而公共注册 `tests/public_registration.rs`
  属 integrator。**integrator 不要再把本文件加进 public_registration.rs**（会重复编译/符号冲突）。
- 运行：
  ```
  cargo test -p khaen-rt-host swap            # DoD（91 passed / 0 failed）
  cargo test -p khaen-rt-host --offline swap -- --list   # t20_ 计数可复算（91 tests；swap 过滤共 94 = 91 T20 + 3 T21 命名含 swap）
  ```
- 全部离线：admission 用 `MockMemory`（不执行真实 mlockall/getrlimit）、不打开 ALSA/设备、
  不起 RT 线程；crossfade/retire 用栈上 `TspiRtControlV1`/`TspiSwapAckV1`/夹具 plan 字节。

## 覆盖（core-v1 条款 → 测试组）

| 组 | 条款 | 代表测试 |
|---|---|---|
| A 布局/类型 | §11.13/§13.2/§9.2 | t20_swap_ack_layout_matches_spec、t20_crossfade_budget_layout_matches_spec、t20_rt_visible_types_have_no_drop、t20_rt_entry_points_take_borrowed_rt_structs_only |
| B swap ack | §7.3/§11.13/RG-2 | t20_swap_ack_worker_acks_accumulate_and_set_retire_ready、t20_swap_ack_two_worker_profile_uses_parameterized_mask、t20_swap_ack_block_all_ack_sets_all_workers、t20_swap_ack_block_cutoff_rejected、t20_swap_ack_block_version_label_rejected |
| C graph quarantine | §7.6/INV-L5/R-DROP-06 | t20_graph_quarantine_not_released_before_all_ack、t20_graph_quarantine_late_ack_label_mismatch、t20_graph_quarantine_exhaustion_rejects_new_enter、t20_graph_quarantine_release_all_acked |
| D plan 解析/准入 | §11.10/§5.3（E4） | t20_admit_assigns_monotonic_versions、t20_admit_rejects_*（magic/abi/rate/block/channels/stride/paths/taps/coeff/graph_id/table/role）、t20_admit_runs_admission_mlock_and_pretouch、**t20_admit_plan_flags_e4_rejects_bits5_to_7**（bit0..4 接受 / bit5..7 与混置拒收 / bit2 仍走 crossfade 准入；`PLAN_FLAG_MASK=0x1F` 钉死） |
| E stage/arm/commit/abort | §4.4/§5.3/§7.5 RG-1 | t20_stage_*、t20_arm_writes_pending_and_arms、t20_commit_swaps_pointer_and_version、t20_commit_rejects_illegal_pending_labels、t20_complete_swap_returns_idle_and_promotes、t20_abort_* |
| F 热切/retire 集成 | §7.5 RG-1..RG-4/§13.6 | t20_swap_complete_quarantines_old_graph、t20_retire_requires_swap_all_ack、t20_retire_requires_quarantine_all_ack、t20_retire_requires_old_blocks_terminal、t20_retire_full_path_releases_old_plan、t20_rt_path_does_not_touch_arc_count、t20_lost_worker_keeps_old_plan_forever |
| G crossfade | §5.3 I06/§7.4/§11.13 | t20_crossfade_accepts_aligned_budget、t20_crossfade_rejects_d_mismatch、t20_crossfade_rejects_peak_cpu_over_available、t20_crossfade_rejects_peak_ram_below_lower_bound、t20_crossfade_rejects_rate_change、t20_crossfade_then_commit_and_retire_keeps_d_until_ack |

## §13.6 T20 负向要求逐条对照

- (a) 迟到写入后 cutoff 的块永不发布：`t20_swap_ack_block_cutoff_rejected`、`t20_swap_ack_block_partial_phase_rejected`
  （cutoff/未三 phase all-ack 的块不得推进 swap ack；T09 `set_cutoff`/`ack_all` 语义在本文件组装场景中断言）。
- (b) quarantine 槽在所有 worker ack 前不复用：`t20_graph_quarantine_not_released_before_all_ack`。
- (c) 旧 plan 在 all-ack 前不 unmap（mock 计数器）：`t20_retire_requires_swap_all_ack`、
  `t20_retire_full_path_releases_old_plan`（`SwapStats::unmapped` + `arena_strong_count`）。
- (d) 失联 worker 场景内存不回收：`t20_lost_worker_keeps_old_plan_forever`。
- (e) crossfade 双图峰值超限 → 拒绝：`t20_crossfade_rejects_peak_cpu_over_available`、
  `t20_crossfade_rejects_peak_ram_over_available`、`t20_crossfade_rejects_peak_ram_below_lower_bound`。
- (f) old/new total_delay_q16 不等 → 拒绝：`t20_crossfade_rejects_d_mismatch`、`t20_crossfade_rejects_declared_d_total_mismatch`。
