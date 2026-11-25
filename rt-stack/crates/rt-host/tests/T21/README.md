# tests/T21 —— fault（有界直方图 / 另核 watchdog / fallback 预分配准入 / 安全 mute + quarantine）

- 私有测试：`fault_models.rs`（**72 个 `#[test]`**，全部 `t21_` 前缀）。
- 注册方式：**`include!` 自注册**（`crates/rt-host/src/fault.rs` 末尾
  `#[cfg(test)] mod fault_tests { include!("../tests/T21/fault_models.rs"); }`），
  因为 cargo 不自动发现 `tests/<子目录>/*.rs`，而公共注册 `tests/public_registration.rs`
  属 integrator。**integrator 不要再把本文件加进 public_registration.rs**（会重复编译/符号冲突）。
- 运行：

  ```
  cargo test -p tspi-rt-host fault              # DoD（过滤命中 76 = 72×T21 + 3×T17 名含 fault + 1×scheduler "defaults" 含 fault）
  cargo test -p tspi-rt-host fault -- --list   # executed 计数可复算（76 行，其中 t21_ 72 行）
  cargo test -p tspi-rt-host                   # 整 crate 回归（T21 签收快照：lib 568 + abi_layout 3 + public_registration 1；随并发 leaf 增长）
  ```

- 全部离线：admission 用 `AdmissionEnv/MemoryPlan` 值（不执行真实 mlockall/getrlimit）、
  不打开 ALSA/设备、不起 RT/watchdog 线程；watchdog 用栈上 `TspiRtControlV1` + 手工样本序列，
  直方图/事件/quarantine/fallback 用固定数组与 POD 值。真实独立核 watchdog 线程、CPU 亲和与
  板端 RT 实测 **NOT_RUN**（见 `evidence/T21/MANIFEST.md` §8）。

## 覆盖（core-v1 条款 → 测试组）

| 组 | 条款 | 代表测试 |
|---|---|---|
| A 布局/静态断言/源码纪律 | §3.3/§8.3/§8.4/§11.13/§13.2 | t21_constants_and_fixed_layout_match_core_v1、t21_public_types_have_no_drop、t21_module_source_has_no_heap_lock_syscall_or_panic_tokens |
| B watchdog 另核/配置 | §4.1/§6 RT-first 故障行 | t21_watchdog_rejects_observation_core_equal_to_dsp_core、t21_watchdog_accepts_separate_observation_core、t21_watchdog_rejects_invalid_dsp_core_count_and_thresholds、t21_watchdog_two_worker_profile、t21_watchdog_sample_validation_rejects_bad_expected_workers/stray_mask_bits |
| C watchdog 心跳/stale/只置位/不自动恢复 | §5.1/§5.5/§9.2(406) | t21_watchdog_stale_boundary_sets_safety_request、t21_watchdog_heartbeat_advance_resets_stale_counter、t21_watchdog_epoch_change_resets_stale_without_mute、t21_watchdog_latch_is_not_auto_cleared_by_heartbeat、t21_watchdog_reset_requires_readmission_ticket、t21_watchdog_worker_lost_requests_fault_after_threshold、t21_watchdog_lost_core_mask_counts_as_worker_lost、t21_watchdog_swap_ack_stall_requests_fault、t21_watchdog_full_swap_ack_resets_stall_counter、t21_watchdog_device_lost_requests_mute、t21_watchdog_only_escalates_mute_to_fault、t21_watchdog_two_worker_ack_mask_not_hardcoded_all |
| D 有界直方图 | §8.4（6 metric×4 worker×32 桶，溢出桶=第 32 桶） | t21_histogram_uniform_bucket_boundaries、t21_histogram_records_are_isolated_per_metric_and_worker、t21_histogram_overflow_bucket_collects_tail、t21_histogram_snapshot_totals_match_records、t21_histogram_config_validation、t21_histogram_record_rejects_invalid_worker、t21_histogram_export_contains_units_bounds_and_ns、t21_ticks_to_ns_conversion |
| E 计数 | §8.4 | t21_counters_record_and_snapshot、t21_counters_snapshot_text_contains_all_names |
| F quarantine 池 | §7.5/§7.6/INV-L5/D-08/D-09 | t21_quarantine_eight_slots_ninth_refused_and_counted、t21_quarantine_release_before_all_ack_refused、t21_quarantine_all_ack_then_release_ok_and_slot_reusable、t21_quarantine_label_mismatch_does_not_corrupt_slot、t21_quarantine_duplicate_or_unexpected_ack_refused、t21_quarantine_invalid_pending_mask_refused、t21_quarantine_worker_lost_pins_release_forever、t21_quarantine_core_lost_pins_release、t21_quarantine_snapshot_reflects_state、t21_quarantine_forbid_release_is_idempotent_first_reason_wins |
| G fallback 预分配+准入 | §5.5（G=I 保 P_post、预算 T_block−io_tail、失败拒绝） | t21_fallback_admit_ok_and_headroom、t21_fallback_admit_requires_two_preallocated_graphs、t21_fallback_admit_requires_preallocated_silence_buffer、t21_fallback_admit_requires_identity_g_with_p_post、t21_fallback_admit_rejects_matrix_routes、t21_fallback_admit_rejects_zero_hash_version_and_bad_channels、t21_fallback_admit_requires_measured_worst_deadline、t21_fallback_deadline_budget_per_rate_and_block |
| H 安全状态机 | §5.1/§5.5/§9.2(307) | t21_safety_state_starts_normal、t21_safety_worker_requester_is_rejected、t21_safety_mute_without_silence_buffer_goes_faulted、t21_safety_fallback_unavailable_goes_307_mute_or_fault、t21_safety_admitted_fallback_enters_protection_fallback、t21_safety_transition_key_is_deduplicated、t21_safety_escalates_and_ignores_downgrade、t21_safety_recovery_requires_ticket_and_writes_exit_event、t21_readmission_ticket_validation |
| I supervisor 集成 | §7.5 RG-2/RG-3、§13.6 T21 | t21_supervisor_service_watchdog_mutes_and_keeps_quarantine、t21_supervisor_worker_lost_pins_quarantine_and_faults、t21_supervisor_core_lost_faults_and_pins、t21_supervisor_ack_slot_exhausted_mutes_and_forbids_release、t21_supervisor_quarantine_full_mutes_and_counts、t21_supervisor_late_streak_enters_admitted_fallback、t21_supervisor_service_control_request_from_ctl、t21_supervisor_note_swap_and_retire_counters、t21_supervisor_record_paths_fill_hist_and_counters |
| J 事件环/非 RT 导出 | §8.1/§8.3/§8.4 导出规则 | t21_event_lane_fifo_seq_lane_and_drop、t21_event_lanes_isolate_and_count_dropped、t21_export_contains_required_metadata_units_and_safety、t21_supervisor_drain_events_returns_quarantine_and_mute_events |

## §13.6 T21 负向要求逐条对照

- watchdog 超时（stale 达阈值）→ 只置位 `safety_request`=1（mute），事件/迁移由 control 执行：
  `t21_watchdog_stale_boundary_sets_safety_request`、`t21_supervisor_service_watchdog_mutes_and_keeps_quarantine`。
- worker lost / 失核 → Faulted，quarantine 永久保留、release 永久拒绝（未 ack 不复用）：
  `t21_quarantine_worker_lost_pins_release_forever`、`t21_quarantine_core_lost_pins_release`、
  `t21_supervisor_worker_lost_pins_quarantine_and_faults`、`t21_supervisor_core_lost_faults_and_pins`。
- ack 槽耗尽 → 停止启动新 block（`block_start_allowed()==false`）→ SilenceMute，禁止释放：
  `t21_quarantine_release_before_all_ack_refused`、`t21_supervisor_ack_slot_exhausted_mutes_and_forbids_release`。
- quarantine 池耗尽 → `RT_E_QUARANTINE_FULL` + SafetyMute，retained 不减：
  `t21_quarantine_eight_slots_ninth_refused_and_counted`、`t21_supervisor_quarantine_full_mutes_and_counts`。
- 恢复只经完整 admission（`ReAdmissionTicket` 必须由成功 `AdmissionReport` 生成），无自动恢复：
  `t21_safety_recovery_requires_ticket_and_writes_exit_event`、`t21_watchdog_reset_requires_readmission_ticket`、`t21_readmission_ticket_validation`。
- 事件 transition_key 不重复执行：`t21_safety_transition_key_is_deduplicated`、`t21_safety_escalates_and_ignores_downgrade`。
- 安全输出不消费半图：fallback 未准入/静音 buffer 缺失时不允许进入对应安全态（升级或 Faulted）：
  `t21_safety_fallback_unavailable_goes_307_mute_or_fault`、`t21_safety_mute_without_silence_buffer_goes_faulted`。
