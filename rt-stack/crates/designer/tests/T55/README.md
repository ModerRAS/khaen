# tests/T55 —— T55（fir-reporter）私有独立报告测试

- 归属：WBS T55 / task-96，owner rt-fir-reporter2；写域仅
  crates/designer/src/report.rs + crates/designer/tests/T55/ + rt-stack/evidence/T55/。
- 测试文件：report_cases.rs（19 个 #[test]，函数名统一 t55_*）。
- 运行（在 rt-stack/ 下）：

    cargo test -p tspi-fir-designer report

  点时刻实测（2026-11-17）：38 passed / 0 failed / 291 filtered out，exit 0（其中 T55 自有 19 项）；
  见 rt-stack/evidence/T55/run-tests-wbs-command.log 与 test-list.txt。

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T55/ 子目录。

因此 T55 采用与 T33/T36/T37/T41 相同的合法方式：crates/designer/src/report.rs 末尾

    #[cfg(test)]
    mod report_tests {
        use super::*;

        include!("../tests/T55/report_cases.rs");
    }

本文件因此参与 src/lib.rs 的单测构建，真实被 cargo test -p tspi-fir-designer report 执行
（executed > 0），测试名过滤路径含 report；不需要 integrator 注册，也不要再把它加到
public_registration.rs（会重复注册/编译失败，因为文件按模块内联语义编写，依赖 super::* 的私有
作用域，例如 ringing_metrics / latency_tier / band_stat / build_budget 等内部函数）。

## 测试覆盖（对应 spec fir-toolbox.md v1.2-draft §1.2–§1.5、§2.2–§2.4、§3.1–§3.8、§7.2、§9）

1. t55_oracle_hand_vectors_v1_v3_v4_v11 —— §9 V1/V3/V4/V11 手算向量 + V2 延迟表。
2. t55_oracle_fft_roundtrip_and_min_phase_consistency —— 自带 FFT 的 Parseval/往返；M2 判别力
   （min_fast <=2 deg、线性相位 >2 deg）与模块实现互证。
3. t55_report_linear_family_end_to_end —— linear 完整报告 + 与 T10 标量测量交叉对照 + 20/20k 端点。
4. t55_report_linear_highpass_lead_ruling_a —— HP 阻带含 DC、峰值 < -A_s、镜像读法穿越点。
5. t55_report_min_fast_m2_and_frequency_varying_gd —— min_fast 报告、M2、频变 GD、边界。
6. t55_report_min_slow_family_distinction —— Nyquist 带、单调、M(0.45fs)>-60 dB。
7. t55_report_apodizing_pre_ring_and_statement —— 前振铃门/能量门、对称振铃、非专有 DAC 陈述、
   f_p 越界 -> E_FIR_APO_FP。
8. t55_report_brickwall_feasibility_and_ringing_duration —— 阻带/过渡/振铃时长、Kaiser 可行性。
9. t55_report_nos_identity_and_endpoints —— nos 恒等 + 48k/32k/96k Endpoint 联合类型。
10. t55_report_corrected_complex_target_equivalence_and_delay —— 复目标、复误差、delay_removed、
    等价性/magnitude_only、缺目标/非因果拒绝。
11. t55_unsupported_family_and_spec_rejections —— 未知 family / family↔params / taps / NaN /
    全零 / fs / nos 非恒等全部 E_* 拒绝。
12. t55_family_mutual_exclusion_checks —— 砖墙冒充 apodizing、min_slow 冒充 min_fast -> E_FIR_FAMILY。
13. t55_report_hp_nyquist_even_length_rejected —— 偶长 HP 声明到 0.5fs -> E_FIR_HP_NYQ。
14. t55_report_keeps_full_evidence_on_failed_criteria —— 失败判据保留完整报告（不静默清零）。
15. t55_budget_kaiser_delay_ram_cost —— V12/V13/V14、15/25/40 ms、RAM、taps profile、§2.3 成本。
16. t55_report_consistency_with_declared_response —— §2.2 规则 5 复算一致/篡改必 FAIL。
17. t55_grid_endpoint_and_manifest_face_fields —— 网格定义/点数 + §2.2 字段面齐全。
18. t55_determinism_and_no_hidden_gain —— 确定性 + 参考归一化（禁止隐藏增益）。
19. t55_emit_sample_report_summary —— 生成 evidence/T55/report-sample.txt 的样例来源。

## 独立性说明

被测实现是 report.rs 自己的 direct DTFT + 解析 GD + 自带 radix-2 FFT（M2 cepstrum）；本测试只把
linear/min_fast/min_slow/apodizing/brickwall/nos 模块的标量函数（magnitude_db_at /
group_delay_samples_exact / min_phase_consistency_deg）作为【交叉对照】互证两个独立实现，
不进入报告测量路径。V16/V17 的作者参考值不是 golden，本测试用自己的实现复算并只作量级/判别性断言。

## 未测项

WAV/manifest 落盘、板端/ALSA/RT/soak、真实测量、f32 量化与跨架构差异、与 T14/T26/T56 的集成
均 NOT_RUN；详见 rt-stack/evidence/T55/MANIFEST.md §9。
