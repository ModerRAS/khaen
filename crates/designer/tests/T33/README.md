# tests/T33 —— T33（min_fast）私有独立响应测试

- 归属：WBS T33 / task-62，owner rt-fir-minfast；写域仅 leaf 文件 + tests/T33 + evidence/T33。
- 测试文件：min_fast_cases.rs（16 个 #[test]，函数名统一 t33_*）。
- 运行：

  cd rt-stack
  cargo test -p khaen-designer min_fast

  点时刻实测（2026-11-17）：16 passed / 0 failed / 80 filtered out，exit 0；见
  rt-stack/evidence/T33/run-tests.log 与 test-list.txt（16 tests, 0 benchmarks）。

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T33/ 子目录。

因此 T33 采用与 T41 相同的合法方式：crates/designer/src/min_fast.rs 末尾

    #[cfg(test)]
    mod min_fast_tests {
        use super::*;

        include!("../tests/T33/min_fast_cases.rs");
    }

本文件因此参与 src/lib.rs 的单测构建，真实被 cargo test -p khaen-designer min_fast 执行
（executed = 16），测试名过滤路径含 min_fast；不需要 integrator 注册，也不要再把它加到
public_registration.rs（会重复注册/编译失败，因为文件按模块内联语义编写、依赖 super::* 的私有
作用域，例如 Fft/ResponseSpectrum 等实现细节）。

## 测试覆盖（对应 spec fir-toolbox.md v1.2 §3.2 判据 1–5 与 §3.1 判据 3–6 等）

1. t33_fft_bessel_kaiser_primitives：FFT delta/往返/Parseval、Bessel I0 已知值、Kaiser 窗
   对称与 β=0 全 1（原语自检，独立于设计路径）。
2. t33_v17_min_fast_reference_reproduction：V17 参考复算——M2 hilbert_consistency、幅度守恒、
   GD 表（50 Hz–18 kHz 七个频点）、DC 增益；期望值来自 evidence/T04/vector-results.json。
3. t33_normative_criteria_all_pass_default：默认参数 10 条规范判据全 PASS + 各字段范围。
4. t33_independent_response_passband_stopband_transition：独立直接 DFT 复算通带 p-p、阻带 max、
   −A_s 穿越点（不依赖被测谱插值/报告路径）。
5. t33_min_phase_energy_frontloaded_and_magnitude_preserved：能量前置（首 25% taps）、
   原型↔最小相位幅度守恒、原型对称线性相位而输出非对称、M2 两尺寸（65536/32768）。
6. t33_delay_frequency_varying_not_zero_delay：特征频点 GD 与独立解析 GD 一致（容差
   §3.2 判据 3 的 ±0.5 sample/±5%）、频变/非零延迟、onset、gd_max、zero_delay_claim=false、
   latency_ms=GD(1 kHz)/fs。
7. t33_finite_support_and_ringing：有效支撑 first/last/effective_taps、振铃 W=ceil(0.000333fs)、
   pre/post ring 与能量（主峰贴近 0 时按 spec §1.4 记 n/a 而非隐藏数值）。
8. t33_stability_boundary_fir_poles_at_origin：FIR 分母度 0、极点全在 z=0、无 IIR 极点、
   无 NaN/Inf、逆因果诊断仅 MAY 诊断且有限、零点半径检查明确 not_run。
9. t33_family_boundary_accepts_edge_rejects_beyond：f_s/f_p = 1.5 边界内通过；>1.5 与
   f_s>0.45fs 分别 E_FIR_FAMILY；0.45fs 边界精确值接受参数校验。
10. t33_param_rejections_codes：24 类非法参数逐一断言稳定错误码（E_FIR_TAPS/BAND/SCHEMA/
    BUDGET/FAMILY），含 roots 未实现拒绝、网格非 2 的幂、cepstrum_fft < 2*taps 等。
11. t33_determinism_and_no_hidden_gain：两次设计逐值一致；DC 声明 0 dB 实测 ±0.01 dB；
    原型 DC 归一 sum=1；无隐藏整体增益。
12. t33_endpoints_20hz_20khz_and_not_applicable：48 kHz 的 20 Hz/20 kHz 均 measured；
    32 kHz 的 20 kHz 写 not_applicable_at_fs（Endpoint 联合类型，非裸字符串）。
13. t33_linear_prototype_variant_designs_and_reports：prototype.magnitude=linear 变体设计成功、
    判据实测通过、给出线性过渡的工程 warning。
14. t33_infeasible_reports_honestly_not_silent_pass：64 taps/80 dB 与 attenuation=120 dB
    不可达时 design() 仍返回完整报告但 accepted()=false、failed 列表含
    stopband_attenuation/transition_edge，design_strict() 以 E_FIR_BUDGET 拒绝（INFEASIBLE）。
15. t33_48k_96k_design_and_delay_scaling：96 kHz 设计通过判据、20 kHz 端点可测、
    GD 样本标度随采样率变化、两个采样率下都频变/非零延迟。
16. t33_design_strict_and_summary_lines：design_strict 成功；summary_lines 含 family/判据 PASS/
    NOT zero-delay/stability 行，默认参数摘要无 FAIL；T55 可消费的字段自检。

## 独立性说明

“独立复算”工具（t33_direct_resp / t33_direct_mag_db / t33_direct_gd / t33_direct_crossing）
用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟 Re(G/H)，不调用被测模块的 FFT/谱插值/报告路径。
只有 M2 一致性调用公开函数 min_phase_consistency_deg（M2 本身就是规范判据定义），并用
“原型↔最小相位幅度守恒 + 能量前置”作为独立交叉证据。V17 期望值来自 T04 可复现脚本
evidence/T04/recompute-vectors.mjs（本测试不复用其代码）。
