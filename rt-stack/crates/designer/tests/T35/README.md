# tests/T35 —— T35（fir-apodizing）私有测试

- 归属：WBS T35（deps T06/T04 已 PASS）；team task-64；owner「rt-fir-apodizing」。
- 测试文件：`apodizing_cases.rs`（39 个 `#[test]`，函数名统一 `t35_*`）。
- 运行：

```
cd rt-stack
cargo test -p khaen-designer apodizing
```

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：`tests/public_registration.rs` 与公共注册**只由 integrator 写**；
cargo 只把 `tests/*.rs`（顶层文件）当集成测试目标，**不会自动发现** `tests/T35/` 子目录。

因此 T35 采用与 T41 相同的不碰公共注册的合法方式：`crates/designer/src/apodizing.rs` 末尾

```rust
#[cfg(test)]
mod apodizing_tests {
    use super::*;
    include!("../tests/T35/apodizing_cases.rs");
}
```

本文件因此参与 `src/lib.rs` 的单测构建，真实被 `cargo test -p khaen-designer apodizing`
执行（executed = 39），测试名过滤路径含 `apodizing`；**不需要 integrator 注册**，也不要再把它加到
`public_registration.rs`（会重复注册/编译失败，因为文件按模块内联语义编写，依赖 `super::*` 作用域）。

## 测试覆盖（按 fir-toolbox v1.2-draft §3.4 分组）

- 参考构造/V16：参数默认值、V16 指标（pre/post ring −54.6361 dB、pre_energy −55.4197 dB、
  Nyquist 带 −36.0085 dB、GD 511.5、通带极平）、FFT 合成 vs §3.4 直接求和式逐点对照；
- 家族边界：f_p=0.85*Nyq 接受、>0.85*Nyq → E_FIR_APO_FP；V16 边界 f_p=20400/21600 的 pre-ring
  （−49.896/−37.869 dB）；0.90*Nyq 激进取样按 apodizing 报 → E_FIR_FAMILY；
- 砖墙互斥（§3.8）：Kaiser-sinc 砖墙式系数按 apodizing 验证 → pre_ring −30.33 dB / pre_energy −24.61 dB
  不达 −45 dB 门且 0.45fs ≤ −60 dB → E_FIR_FAMILY；中心冲激（奇长）→ E_FIR_BUDGET；
- 窗口/目标可算性：Kaiser/Hann² 镜像对称、端点/中心值、Hann²=(Hann)²、Hann² 不接受 beta；
  magnitude-only 目标在 fs/2 取 floor（Lead fs/2 裁决允许）、过渡单调不增；
- 前/后振铃（§1.4）：W=ceil(0.000333*fs)（48k=16、96k=32）、对称设计 pre==post（dB/能量）、
  支撑过短 n/a（pre=None）与 −240 dB 下限；
- 阻带/Nyquist 带（§3.4 判据 3）：−36 dB 参考构造通过 30 dB 声明；64-tap 短支撑不达标 → E_FIR_BUDGET；
  声明 40 dB 不可达 → E_FIR_BUDGET；f_p=0.85*Nyq 时 Nyquist 带不可达 → E_FIR_BUDGET（不是静默放宽）；
- 相位量化：线性相位模型偏差、GD 常数（±0.01 samples）、1 kHz 解析相位（123.75° wrap）、
  f32 系数 vs f64 的幅度/相位/GD/relL2 量化复算（§1.6）；
- 设计/报告：确定性、f32 导出、有效支撑（§1.3）、20 Hz/20 kHz Endpoint（32k 时 N/A）、
  96k 参考预设、Delay/D=(L−1)/2、报告声明句「仅自研可算窗/目标，不复刻任何专有 DAC」；
- 参数拒绝：fs 工程范围/NaN、taps 边界、passband[0]!=20、ripple<0、nyq_att<30、pre_ring_bound
  放松（>−45 → E_FIR_BAND）与更严（−50 通过、−56 → E_FIR_FAMILY）、非有限/零系数、错误长度、非对称系数。

## 明确未覆盖（NOT_RUN）

- 不生成 WAV/manifest（tspi-fir/v1 与 20/20k Endpoint 落盘归 T55）；
- 不做 target 文件导入（T11）、不做 P/G 融合/运行准入（T12/T56）、不做 RT/板端；
- 未与 T55 报告器集成（接口已按 spec 提供）。
