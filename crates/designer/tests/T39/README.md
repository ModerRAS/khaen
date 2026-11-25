# tests/T39 —— T39（输出 FIR crossover LR/BW）私有测试

- 归属：WBS T39 / task-75，owner rt-fir-crossover；写域仅
  `crates/designer/src/crossover.rs` + `crates/designer/tests/T39/` + `rt-stack/evidence/T39/`。
- 测试文件：`crossover_cases.rs`（**18 个** `#[test]`，函数名统一 `t39_*`）。
- 运行：

```powershell
cd <khaen-repo-root>
cargo test -p khaen-designer crossover
```

实测（2026-11-17，rt-stack；Cursor v0.9 终审修复组 A 后收尾）：**running 18 tests; 18 passed / 0 failed / 0 ignored**，exit 0；
日志与 `--list` 见 `rt-stack/evidence/T39/`。

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §测试约定：`tests/public_registration.rs` 与公共注册只由 integrator 写；
cargo 只把 `tests/*.rs` 顶层文件当 integration test 目标，不会自动发现 `tests/T39/` 子目录。
因此 `crates/designer/src/crossover.rs` 末尾：

```rust
#[cfg(test)]
mod crossover_tests {
    use super::*;

    include!("../tests/T39/crossover_cases.rs");
}
```

本文件因此参与 `src/lib.rs` 的单测构建，真实被 `cargo test -p khaen-designer crossover` 执行
（executed = 17）；不需要 integrator 注册，也不要再把它加到 public_registration.rs
（include 文件按模块内联语义编写、依赖 `super::*` 作用域）。

## 测试覆盖（spec fir-toolbox.md v1.2-draft §4.1 + §1.2–1.5/§2.5 口径）

1. `t39_type_parse_rejects_unknown`：LR2/LR4/LR6/LR8、BW1..BW8 解析（大小写/空白）；未知 type
   一律 `E_FIR_XOVER_TYPE`（§4.1 显式拒绝未知 type）；order/家族判定。
2. `t39_analytic_model_hand_vectors`：解析模型手算向量——fc 幅度 LR=−6.0206 dB / BW=−3.0103 dB、
   闭合公式独立对照（fc/2、fc、2fc）、DC/Nyquist 极限、解析相对相位 90°·order、解析极性
   （order%4==2 反相）、LR 全阶与 BW 奇数阶 |S|=1（全通）与 BW 偶数阶 @fc=√2（+3.0103 dB）；
   FFT 原语 delta/往返自检。
3. `t39_lr4_design_flat_sum_same_polarity`：LR4/80 Hz/2048 taps——合并 |S| 偏差 ≤0.5 dB（实测
   ~0.03 dB）+ 独立 naive DFT 复算、同极性（§4.1）、交叉点相对相位 0°、两支同 L 同 D
   （D=(taps−1)/2，相对 0 整数）、fc 幅度 −6.0206 dB、幅度近似误差门。
4. `t39_lr_orders_polarity_and_flat_sum`：LR2/4/6/8 全阶——解析极性、合并平坦、相位 0°、
   独立复算、系数长度；LR2/LR6 反相、LR4/LR8 同相。
5. `t39_bw_odd_design_allpass_sum`：BW1/3/5/7——解析全通和 → 合并平坦、相位对解析目标
   （±90°）、极性报告字段（§4.1 "BW 奇数阶需报告使用极性"）、fc 幅度 −3.0103 dB。
6. `t39_bw_even_honest_infeasible`：BW2/4/6/8——解析和本身不平坦（带内相对 max 偏差 >1 dB、
   @fc +3.0103 dB）；`design()` 返回完整报告但 `accepted=false`、failed 含 `xover_sum_flat`、
   warnings 写明规范矛盾；**F-24：偶数阶 `xover_family_polarity`/`xover_phase_at_fc` 标
   NOT_APPLICABLE（只报告、不计 pass、limit=None，不得以相位门放行/拒收）**；
   `design_strict()` 返回 `E_FIR_XOVER_SUM` 且消息含 INFEASIBLE（不含 `E_FIR_XOVER_PHASE`）。
7. `t39_linear_phase_method_conflict`：`LinearPhaseMagnitude`（§4.1 字面构造）——LR4 可用
   （同极性/平坦和/0° 相位/Type I 对称）；BW3 无法用同 L/同 D 线性相位复现 −90° 解析相位且
   和为 +3 dB 峰 → 判据失败、strict `E_FIR_XOVER_PHASE`；LR2 该极性下合并凹陷 → 失败，而默认
   AnalyticPhase 可用；偶数 taps + LinearPhaseMagnitude 被 `E_FIR_TAPS` 拒绝（Type II H(π)=0
   无法承载 HP Nyquist 增益）。
8. `t39_no_default_role_hp`：**无默认角色 HP**——`plan_outputs` 只接受显式 `OutputAssignment`；
   空赋值全 bypass（高度 4/5 默认全频）；只赋值 0/7 时其它（含高度）保持 bypass；重复/越界
   output index → `E_FIR_XOVER_LAYOUT`（F-22：该布局码已正式登记 spec §2.5）；输出系数与设计支路逐值一致。
9. `t39_relative_delay_and_parameters`：相对 D——两支同 L/同 D，D=(taps−1)/2（可为半整数，
   §1.3），相对 D=0 整数并报告；`declared_delay_samples` 两种方法均为 (taps−1)/2；
   参数边界 fc∈[20,0.45fs]、taps∈[8,16384]、attenuation∈[40,120]、fs 范围。
10. `t39_insufficient_taps_infeasible`：支撑不足（LR4/80/64、BW3/100/64、LR2/20/256）——
    幅度/合并/相位判据失败 → accepted=false、warnings 含 INFEASIBLE、strict Err（SUM 或 PHASE），
    不静默 PASS。
11. `t39_group_delay_ringing_support_reported`：GD（实测/解析/物理 GD/1 kHz）、振铃 W=ceil(0.000333fs)、
    有效支撑、max|h|、Σ|h|、LP DC≈1 / HP DC≈0（无隐藏整体增益）、naive GD 复算一致。
12. `t39_determinism_summary_and_finite`：同一输入两次设计逐值一致；summary_lines 含
    kind/method/polarity source/PASS/declared D；判据 id 唯一；notes 含无默认角色 HP/听感/NOT_RUN。
13. `t39_96k_and_window_variants`：96 kHz 设计通过判据；同 fc 物理 GD（剔除声明 D 后）在 96 kHz
    的 samples ≈2×48 kHz；Blackman-Harris/attenuation=60 变体可设计且系数有限（window/
    attenuation 只登记，见模块 docs）；fc=10 kHz 仍可接受。
14. `t39_phase_approximation_analytic_vs_linear_phase`：AnalyticPhase 相位近似误差 ≤5°（报告门）；
    LinearPhaseMagnitude 不追解析相位 → 误差显著更大，判据为 report-only（limit=None）。
15. `t39_fir_approximation_and_endpoints`：LR4/BW3 幅度近似误差 ≤0.5 dB、20 Hz/20 kHz 端点可测、
    全带误差有限（解析幅度 ≥−40 dB 才计入，深阻带相对 dB 无意义）；fc 进入 Nyquist fade 区
    （>0.225fs）时诚实 INFEASIBLE。
16. `t39_fc_production_experimental_infeasible_tiers`：RULED fc 三档——生产 ≤0.2fs accepted、
    0.2–0.225fs 仅 experimental flag 且不得生产接受、>0.225fs `E_FIR_XOVER_FC_INFEASIBLE`；
    strict 对实验设计报 `E_FIR_XOVER_FC_LIMIT`。
17. `t39_family_polarity_lr_in_phase_bw_odd_inverted`：LR2 HP 反相但 fc 同相 0°、LR4 同相；
    BW1/3/5/7 一支反相（180° 极性）且 fc 按解析目标 ±90°、合并平坦；LinearPhaseMagnitude 非规范
    对照不得验收。
18. `t39_xover_error_codes_align_spec`（F-22）：参数域四类触发（fc<20、fc>0.45fs、fs<8k、
    attenuation）全部只输出 `E_FIR_BAND` 且不再输出撤销名 `E_FIR_XOVER_BAND`；
    `E_FIR_XOVER_LAYOUT` 由布局错误触发；读 spec §2.5 断言 `E_FIR_XOVER_BAND` 无独立码行
    （仅 alias→`E_FIR_BAND` 兼容说明）、`E_FIR_XOVER_LAYOUT` 登记行与触发域存在（代码↔规格一致）。

## 独立性说明

`t39_dtft` / `t39_mag_db` / `t39_gd` 用最朴素的逐点 sin/cos 直接 DFT 与解析群延迟，不调用
crossover 模块的 FFT/谱/报告路径；解析目标手算用闭合公式独立复算（`t39_analytic_mag`）；
合并 |S| 偏差用独立 naive DFT 复算（`t39_independent_sum_dev`）。

## 已知规范矛盾/限制（详见 `rt-stack/evidence/T39/MANIFEST.md`）

- spec §4.1 旧稿"按解析幅度目标做线性相位设计"与"BW 相位不是线性、和仍应为全通"的矛盾已由
  Lead 裁决（RULED 2026-11-17，errata B）+ 终审 F-24 关闭：规范构造 = AnalyticPhase、
  LinearPhaseMagnitude 仅对照；BW 偶数阶解析和本身不平坦如实 INFEASIBLE，且偶数阶相位项
  NOT_APPLICABLE（只报告、不计 pass，拒收只走 SUM）。
- AnalyticPhase 的 Nyquist fade 从 0.45fs 起：fc > 0.225fs 时 crossover 带上界进入 fade 区 →
  近似误差按判据失败（INFEASIBLE）；参数域仍以 0.45fs 表达 `E_FIR_BAND` 消息，但 >0.225fs 先由
  `E_FIR_XOVER_FC_INFEASIBLE` 拒绝（RULED 档位）。
- 未测：板端/ALSA/CueMix/RT/soak；khaen-fir/v1 WAV/manifest 落盘（T55）；T12/T23/T56 集成；
  f32 运行期量化误差复算（T55/T22）。
