# tests/T38 —— T38（fir-nos）私有测试

- 归属：WBS T38 / team task-71，owner rt-fir-nos；写域仅
  crates/designer/src/nos.rs + tests/T38 + evidence/T38。
- 测试文件：nos_cases.rs（35 个 #[test]，函数名统一 t38_*）。
- 运行：

      cd rt-stack
      cargo test -p khaen-designer nos
      cargo test -p khaen-designer t38               # 精确 35 passed（推荐）
      cargo test -p khaen-designer nos -- --list     # 注意：nos 过滤会多命中 1 个无关的 apodizing 诊断测试

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T38/ 子目录。

因此 T38 采用与 T36/T41 相同的合法方式：crates/designer/src/nos.rs 末尾

    #[cfg(test)]
    mod nos_tests {
        use super::*;
        include!("../tests/T38/nos_cases.rs");
    }

本文件因此参与 src/lib.rs 的单测构建，真实被 cargo test -p khaen-designer nos
执行（T38 executed = 35），测试名过滤路径含 nos；不需要 integrator 注册，也不要再把它加到
public_registration.rs（会重复注册/编译失败）。

注意：WBS T38 DoD 命令 `nos` 是按子串过滤，还会命中 apodizing 的
`t35_apodizing_diagnostics_v16_numbers`（diagnostics 含 nos），所以 DoD 日志是
running 36 = 35 T38 + 1 无关命中；要精确只看 T38 用 `cargo test -p khaen-designer t38`。

## 测试覆盖（按契约分组）

- 常量/参数契约（§3.7/§2.5/§5.1）：FAMILY=nos、taps=1、D=0、恒等容差 1e-12、
  filtering=none、两条必须声明原句、filtering 字段差异登记；delta 默认值；
- 拒收（§3.7/§2.2/§7.2）：taps∈{0,2,63,1024,16384} → E_FIR_NOS；bypass=false → E_FIR_NOS；
  任何频带参数（passband/stopband/ripple/attenuation）→ E_FIR_NOS；任何窗参数
  （kaiser/hann/blackman_harris/least_squares）→ E_FIR_NOS；任何 delay（±/分数/NaN）→ E_FIR_DELAY
  并指向 T40；fs 越界/NaN/Inf → E_FIR_BAND；合法 fs 8k/48k/96k/384k 接受；
- 设计/V18（§3.7/§9 V18/§1.6）：h=[1.0]、L=1、D=0、delay_ms=0、DC 参考 0 dB、
  Σh=1、Σ|h|=1、峰值余量 0 dB、有效支撑 (0,0,1)、f32 导出 [1.0]、两次设计逐位一致；
- 响应/端点/振铃（§1.2/§1.4/§2.4）：任意频点 |H|=1 dB=0°；normative 8192 点平坦扫描全 0；
  48k 时 f20/f20k 均 measured、fs=32k 时 f20k=not_applicable_at_fs（reason 含 0.499/不可测）、
  fs=44.1k 可测；L=1 振铃两侧 n/a（reason 支撑过短）、长支撑路径仍可算；
- 独立验证（§3.7）：正例全判据 PASS；len(h)!=1 → E_FIR_TAPS；NaN/Inf → E_FIR_NUMERIC；
  长度 1 但 h∈{0,-1,1+1e-7,1.1} → checks.identity=false、criteria_to_error=E_FIR_NOS
  （h=[0] 属 §2.5 nos 例外，不走 E_FIR_NUMERIC）；恒等容差边界 1+5e-13 通过、1+2e-12 失败；
- 可选目标只记录（§6.10）：带 target 提交被接受、target_participated_in_design=false、
  系数与无 target 逐位一致、verification 仍全过；
- 后置输出保护（§3.7/§5.1；DEC「NOS 不是绕保护」）：三项全启用才可 plan；任一 limiter/clip/ramp
  关闭 → E_FIR_NOS 并列出缺失项；链缺保护/缺目标 FIR/缺 scatter/重复级/保护顺序错误 → E_FIR_NOS；
  只旁路 OutputStage::TargetFir，保护/gain/delay/polarity/scatter 均不旁路；规范链 10 级顺序；
- manifest/报告/复刻承诺（§3.7/T38 行）：family=nos、taps=1、delay=0、filtering=none、
  dac_replication=none、protection_bypassed=false、response_flat=true；
  DacReplicationRecord claim=none + 不模拟 MOTU DAC 原句 + 不承诺复刻句；report_text 含全部必须声明；
- 错误码/映射/预算：E_FIR_TAPS/NUMERIC/DELAY/BAND/NOS 五码 code/Display/Error；
  criteria_to_error 映射；NosBudget regular/4096/4B；design_checked 与 design 等价。

## 证据

rt-stack/evidence/T38/MANIFEST.md 记录输入/命令/exit code/executed test 数/关键数值/未测项；
真实执行日志见 evidence/T38/*.log，复现脚本 evidence/T38/repro.ps1。
