# tests/T41 —— T41（modal-peq）私有测试

- 归属：WBS T41 / task-59，owner rt-modal-peq；写域仅 leaf + tests/T41 + evidence/T41。
- 测试文件：\`modal_peq_cases.rs\`（32 个 #[test]，函数名统一 \`t41_*\`）。
- 运行：

\`\`\`
cd rt-stack
cargo test -p tspi-fir-designer modal_peq
\`\`\`

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：\`tests/public_registration.rs\` 与公共注册**只由 integrator 写**；
cargo 只把 \`tests/*.rs\`（顶层文件）当集成测试目标，**不会自动发现** \`tests/T41/\` 子目录。

因此 T41 采用不碰公共注册的合法方式：\`crates/designer/src/modal_peq.rs\` 末尾

\`\`\`rust
#[cfg(test)]
mod modal_peq_tests {
    use super::*;
    include!("../tests/T41/modal_peq_cases.rs");
}
\`\`\`

本文件因此参与 \`src/lib.rs\` 的单测构建，真实被 \`cargo test -p tspi-fir-designer modal_peq\`
执行（executed = 32），测试名过滤路径含 \`modal_peq\`；**不需要 integrator 注册，也不要再把它加到
public_registration.rs**（会重复注册/编译失败，因为文件按模块内联语义编写，依赖 \`super::*\` 作用域）。

## 测试覆盖（按契约分组）

- V5/V6/V7 RBJ 不变量：peaking f0=gain、gain=0 恒等、shelf f0=gain/2+DC/Nyq、S=1 alpha/sin、半增益带宽信息性检查；
- shelf S/Q 语义：q 字段承载 S（不是 Q），slope=12*S ≤12，中心修正=半增益；
- 类型/界限：未知 type 拒绝（E_PEQ_TYPE）、freq/gain/q/fs 界限、通道名正则、契约采样率；
- 预测/精度：DC/Nyquist、极点半径 ≤0.999999、V15 f32 量化偏差与 requires_f64、V15 极点半径；
- 导出：tspi-modal-peq/v1 字段/确定性/≥17 位有效数字，导出 b/a 与字段自洽（≤0.1 dB/1°）；
- 段数/策略：模态 peaking ≤10、每 shelf ≤1、总 ≤32、模态只切、宽带 boost cap（0/+3 dB）与 allow_boost；
- IIR 不硬截 FIR：L_req 公式（r=0.999→11508、r=0.9995→23021）、极限模态段 E_PEQ_FIR_TAIL + required_len、温和段 4096 可通过；
- 原图比对：2a/2b 分报、平坦通过、检出设计改善、过切/正增益 2a FAIL、模态带外新增窄缺口 2b FAIL、1/24 oct 原始分辨率另报（6.833 vs 1/3 oct 2.873）、部分覆盖 warning、非法曲线拒绝；
- 合成夹具：8 模态+倾斜+噪声（≥2 个 Q>10）检出 f±3 Hz、Q±30%、gain≤0、Q 不夹 10、q_unresolved 正确、合并/≤10 段。
