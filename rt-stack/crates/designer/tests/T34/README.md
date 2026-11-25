# tests/T34 —— T34（min_slow）私有独立响应测试

- 归属：WBS T34 / task-72，owner rt-fir-minslow；写域仅 leaf 文件 + tests/T34 + evidence/T34。
- 测试文件：min_slow_cases.rs（18 个 #[test]，函数名统一 t34_*）。
- 运行：

  cd rt-stack
  cargo test -p tspi-fir-designer min_slow

  点时刻实测（2026-11-17）：18 passed / 0 failed / 227 filtered out，exit 0；见
  rt-stack/evidence/T34/run-tests.log 与 test-list.txt（18 tests, 0 benchmarks）。

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T34/ 子目录。

因此 T34 采用与 T33/T41 相同的合法方式：crates/designer/src/min_slow.rs 末尾

    #[cfg(test)]
    mod min_slow_tests {
        use super::*;

        include!("../tests/T34/min_slow_cases.rs");
    }

本文件因此参与 src/lib.rs 的单测构建，真实被 cargo test -p tspi-fir-designer min_slow 执行
（executed = 18），测试名过滤路径含 min_slow（模块路径 min_slow::min_slow_tests::t34_*）；
不需要 integrator 注册，也不要再把它加到 public_registration.rs（会重复注册/编译失败，因为
文件按模块内联语义编写、依赖 super::* 的私有作用域，例如 Fft/bessel_i0/kaiser_window 等
实现细节）。

## 测试覆盖（对应 spec fir-toolbox.md v1.2 §3.3 判据 1–6、§3.8、§3.2 判据 2/3、§1.2–§1.6、§2.4）

1. t34_fft_bessel_kaiser_primitives：FFT delta/往返/Parseval、Bessel I0 已知值、Kaiser 窗
   对称与 β=0 全 1（原语自检，独立于设计路径）。
2. t34_v17_min_slow_reference_reproduction：V17 参考复算——M2 hilbert_consistency 0.003647°
   （发布值 0.0036067°，Δ=4.0e−5°）、GD 表 50 Hz–22 kHz（与发布值 Δ≤8.6e−4 samples）、
   Nyquist 带 max −40.98425 dB（发布值 −40.98521，Δ=9.6e−4 dB）、M(20k)=−6.0206、
   M(22k)=−16.6833、M(23.5k)=−40.2829 dB、原型↔最小相位幅度守恒 8.99e−9 dB。
3. t34_normative_criteria_all_pass_default：默认参数 11 条规范判据全 PASS + 各字段范围。
4. t34_independent_response_passband_nyquist_monotonic：独立直接 DFT 复算通带 p-p
   （报告 3.15393e−4 vs 直接 3.15373e−4 dB）、Nyquist 带 max（−40.984250 vs −40.984246 dB）、
   慢滚降单调性（上翘 0 dB）；并验证 f_p 处 ≈0 dB、20 kHz ≈−6.02 dB（slow 而非 fast）。
5. t34_min_phase_energy_frontloaded_and_magnitude_preserved：能量前置（首 25% taps =
   0.999999999999944）、原型对称/DC 归一、原型↔最小相位幅度守恒 ≤1e−6 dB、M2 两尺寸
   （65536/32768）通过、线性相位原型的 M2=179.9998°（证明 M2 判据有区分力、不是恒真）。
6. t34_delay_frequency_varying_not_zero_delay：特征频点 GD 与独立解析 GD 一致（≤0.5 sample/±5%）、
   频变/非零延迟（GD(1k)=0.234606、GD(22k)=2.202806 samples）、onset、gd_max、
   zero_delay_claim=false、latency_ms=GD(1 kHz)/fs，延迟声明文案含 NOT zero-delay/onset/gd_max/
   M(0.45fs)。
7. t34_slow_vs_fast_family_distinction：slow/fast 数值互斥（spec §3.3 判据 6 / §3.8）——
   slow M(0.45fs)=−13.716 dB、−60 dB 穿越点 23844.95 Hz > 0.45fs；fast 风格（raised_cosine
   16k→19k 同构造）M(0.45fs)=−120 dB、穿越点 18956.48 Hz ≤ 0.45fs、slow_like=false；
   两者 M(20k) −6.02 vs −119.99 dB、Nyquist 带 −40.98 vs −120.0 dB。穿越点另有独立直接 DFT 复算。
8. t34_slow_rolloff_independent_params_semantics：独立参数 nyquist_attenuation_db（20..60，
   无 fast 的 stopband_hz/attenuation_db）；目标幅度 profile 严格单调、f_p 前=1、Nyquist=floor、
   20 kHz=−6.02059 dB、0.45fs=−13.71810 dB、0.49fs=−41.0539 dB（慢滚降一路延续到 Nyquist）。
9. t34_family_boundary_accepts_edge_rejects_beyond：f_p=0.42fs 参数合法但 Nyquist 衰减不足 →
   诚实 FAIL（E_FIR_BUDGET/INFEASIBLE）；0.42fs+1 Hz → E_FIR_FAMILY；fp=19k 设计 PASS 且带
   接近家族上界 warning。
10. t34_param_rejections_codes：21 类非法参数逐一断言稳定错误码（E_FIR_TAPS/BAND/SCHEMA/
    BUDGET/FAMILY），含 family/transition 非规范值、roots 未实现拒绝、网格非 2 的幂、
    prototype.taps/cepstrum_fft < 2*taps 等；design_strict 对家族非法透传 E_FIR_FAMILY。
11. t34_determinism_and_no_hidden_gain：两次设计逐值一致（系数/原型/响应/延迟/判据）；DC 声明
    0 dB 实测 ±0.01 dB；sum(h)≈1（无隐藏整体增益）。
12. t34_endpoints_20hz_20khz_and_not_applicable：48 kHz 的 20 Hz（≈0 dB）/20 kHz（≈−6.0206 dB，
    慢滚降显著高于阻带 floor）均 measured；32 kHz 的 20 kHz 写 not_applicable_at_fs
    （Endpoint 联合类型，非裸字符串）。
13. t34_48k_96k_design_and_delay_scaling：96 kHz 设计通过判据、20 kHz 可测且 ≈−0.3372 dB、
    GD(1k) 48k=0.234606 → 96k=0.505225 samples（约 fs 比）、Nyquist 带 −53.0 dB；两者都频变/
    非零延迟。
14. t34_nyquist_declaration_honesty_not_silent_pass：nyq=60 dB 参数合法但构造不可达 →
    design() 完整报告 accepted=false、nyquist_attenuation 判据 FAIL（measured −40.984 vs limit −59）、
    design_strict=E_FIR_BUDGET 且消息含 INFEASIBLE；nyq=20 dB 可达 → PASS。
15. t34_low_taps_infeasible_reports_honestly：8 taps → failed=[passband_ripple,
    nyquist_attenuation]、design_strict=E_FIR_BUDGET+INFEASIBLE；32 taps → 仅 nyquist_attenuation
    FAIL（覆盖“部分判据失败”路径），不静默截断/放行。
16. t34_finite_support_and_ringing：有效支撑 first/last/effective_taps、主峰 peak=0、振铃
    W=ceil(0.000333fs)=16、pre-ring 按 spec §1.4 记 n/a、post-ring 与能量独立复算一致。
17. t34_stability_boundary_fir_poles_at_origin：FIR 分母度 0、极点全在 z=0、无 IIR 极点、
    无 NaN/Inf、逆因果诊断仅 MAY 诊断且有限、零点半径检查明确 not_run。
18. t34_design_strict_and_summary_lines：design_strict 成功；summary_lines 含 family=min_slow/
    11 条判据 PASS/NOT zero-delay/slow (raised-cosine f_p->Nyquist)/stability 行，默认参数摘要
    无 FAIL；T55 可消费字段自检。

## 独立性说明

“独立复算”工具（t34_direct_resp / t34_direct_mag_db / t34_direct_gd / t34_direct_crossing /
t34_band_max_db）用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟 Re(G/H)，不调用被测模块的
FFT/谱插值/报告路径。只有 M2 一致性调用公开函数 min_phase_consistency_deg（M2 本身就是规范
判据定义），并以“原型↔最小相位幅度守恒 + 能量前置 + 线性相位反例被 M2 判非最小相位”作独立
交叉证据。slow/fast 对照的 fast 侧用同一 symmetric_prototype_with/min_phase_coefficients 构造，
只换幅度 law（raised_cosine 16k→19k），确保差异来自家族语义而不是实现路径。
