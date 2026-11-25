# T37 私有测试（corrected / lowdelay）

本目录只放 T37 的私有测试；**不**改 integrator 独占的
crates/designer/tests/public_registration.rs，也不改 src/lib.rs。

## 编译/执行方式（与 T10/T33/T34 等 leaf 一致）

crates/designer/src/corrected.rs 末尾有：

    #[cfg(test)]
    mod corrected_tests {
        use super::*;
        include!("../tests/T37/corrected_cases.rs");
    }

include! 相对 src/corrected.rs 解析到本目录的 corrected_cases.rs，于是这些测试是
khaen-designer lib 的单元测试（模块路径 corrected::corrected_tests::*），
可以按名字过滤执行：

    cd rt-stack
    cargo test -p khaen-designer corrected
    cargo test -p khaen-designer --lib corrected -- --list
    cargo test -p khaen-designer --lib corrected -- --nocapture

**integrator 不要再把本文件注册进 public_registration.rs**（会与 include! 重复编译）。

## 覆盖（对应 spec/fir-toolbox.md v1.2-draft §3.6 判据 1–8）

| 测试 | 覆盖 |
|---|---|
| t37_rejects_magnitude_only_target_by_default | §3.6 判据 1 + §6.10：corrected/lowdelay 目标必须带显式相位（E_TGT_PHASE_UNSUPPORTED） |
| t37_magnitude_only_allowed_reports_not_equivalent | §3.6 判据 6：显式降级 magnitude_only=true + not equivalent 声明 |
| t37_absolute_phase_pure_delay_design_meets_limits | §3.6 判据 2：absolute 总相位复误差门 + 参考/延迟报告 |
| t37_delay_removed_requires_exact_d | §3.6 判据 1：D_used 必须等于 target_delay_samples（E_TGT_DELAY） |
| t37_non_causal_target_rejected | §3.6 判据 3：目标 GD 为负 -> E_FIR_NONCAUSAL |
| t37_equivalence_claims_rejected | §3.6 判据 6/§3.8：声称 linear/min 等价 -> E_FIR_EQUIV_CLAIM |
| t37_min_phase_substitution_fails_phase_where_corrected_passes | 反例：最小相位替代不满足相位门；corrected 通过且不声明等价 |
| t37_group_delay_reported_frequency_varying | §3.6 判据 4：GD min/max/mean/@1k 与频变类别 |
| t37_ringing_support_and_out_of_band | §1.3/§1.4：有效支撑/onset/振铃；§3.6 判据 7 带外报告 |
| t37_lowdelay_scan_reports_tradeoff | §3.6 判据 5：>=2 个 D 的误差-延迟折中 + 应用 D 达标 |
| t37_lowdelay_applied_d_over_max_rejected | §3.6 判据 5：D>delay_max / 缺 delay_max -> E_FIR_DELAY |
| t37_lowdelay_infeasible_reports_best_achieved | §3.6 判据 5：INFEASIBLE 报告 best achieved，design_strict 给 E_FIR_BUDGET |
| t37_corrected_infeasible_reports_best_achieved | §3.6 判据 2：不达标 accepted=false + E_FIR_BUDGET/INFEASIBLE |
| t37_determinism_and_no_hidden_gain | §1.5/§1.6：两次设计逐位一致；reference 声明=实测，无隐藏增益 |
| t37_reference_and_endpoints | §1.5/§2.4：参考归一化与 20 Hz/20 kHz Endpoint 联合类型 |
| t37_param_rejections_codes | §3.6 参数/§2.5：taps/band/fs/D/weights/等价声明拒绝码 |
| t37_target_rate_mismatch_rejected | §6.3：target runtime_rate != 设计 fs -> E_TGT_RATE |
| t37_96k_delay_scaling | fs 改变时 D/fs 延迟换算（96k 减半） |
| t37_phase_wrap_handling | §1.2/§6.6：连续相位拟合、wrap 后比较（不是未展开直接相减） |
| t37_fft_and_cg_report | LS 计划可复算：n_fft/bins/ridge/CG 迭代/相对残差/收敛 |
| t37_normative_grid_verification | §1.2 normative 网格 N>=max(8192,8L) 下的完整验证 |
| t37_budget_facts_profile_caps | §7.2：regular/extended 上限与 f32 内存事实（准入归 T56） |
| t37_narrow_band_grid_infeasible | §3.6 判据 8/§2.5：网格可判性不足 -> E_FIR_BUDGET/INFEASIBLE |
| t37_summary_lines_contain_required_facts | 报告摘要字段（证据日志用） |

## oracle 说明

- 目标用 T11 import_json_bytes 真实导入（tspi-target-draft/v1），不是 mock 目标结构。
- 响应验证用 corrected::verify（直接 DTFT + 解析 GD，O(L)/点），不复用 LS 中间量。
- 最小相位替代用 min_phase_from_magnitude_table（同态法，诊断/oracle；不是本家族输出）。
- 数值期望来自 evidence/T37/MANIFEST.md 的 Python 原型 + Rust 实测交叉记录。
