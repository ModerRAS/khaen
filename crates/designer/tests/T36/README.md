# tests/T36 —— T36（fir-brickwall）私有测试

- 归属：WBS T36 / team task-65，owner rt-fir-brickwall；写域仅 leaf（crates/designer/src/brickwall.rs）
  + tests/T36 + evidence/T36。
- 测试文件：brickwall_cases.rs（31 个 #[test]，函数名统一 t36_*）。
- 运行：

    cd rt-stack
    cargo test -p khaen-designer brickwall
    cargo test -p khaen-designer brickwall -- --nocapture   # 诊断数值（V13/V16/规范网格）

## 为什么用 include! 而不是 public_registration.rs

rt-stack/README.md §3.1/§5 规定：tests/public_registration.rs 与公共注册只由 integrator 写；
cargo 只把 tests/*.rs（顶层文件）当集成测试目标，不会自动发现 tests/T36/ 子目录。

因此 T36 采用与 T41 相同的合法方式：crates/designer/src/brickwall.rs 末尾

    #[cfg(test)]
    mod brickwall_tests {
        use super::*;
        include!("../tests/T36/brickwall_cases.rs");
    }

本文件因此参与 src/lib.rs 的单测构建，真实被 cargo test -p khaen-designer brickwall
执行（executed = 31），测试名过滤路径含 brickwall；不需要 integrator 注册，也不要再把它加到
public_registration.rs（会重复注册/编译失败）。

## 测试覆盖（按契约分组）

- 参数/边界/预算拒绝（§3.5 判据 5/7、§7.2）：默认值、过渡带为 0/负/<20 Hz、A_s 越界 [60,120]、
  taps<8、taps>16384（offline only → E_FIR_BUDGET）、f_s>0.499fs、stopband_hz[1]!=0.499fs、
  声明 D!=(L−1)/2、未实现 method（least_squares/remez → E_FIR_FAMILY，不冒充）、非有限参数、
  L_req 不足（A=100/Δf=100 → 3079 > 1024 → E_FIR_BUDGET）、taps=L_req 可设计；
- V13 Kaiser 可行性式 7 组表值（176/242/309/376/483/483/176）；
- profile/内存（§7.2）：regular<=4096、extended<=16384、>16384 offline_only、ram=4·taps；
- 构造/相位（§3.5 参考构造 + §3.1 判据 1/2）：V16（L=1024、fc=19750、beta=8.96）精确镜像对称、
  D=511.5/10.65625 ms、常数 GD=511.5（解析式）、DC 归一 1±1e-12、确定性（两次逐位一致）；
- 独立验证正例：V16 紧凑网格全判据通过；taps=256 的 §1.2 normative 网格（N=8192）全判据通过；
  GD stats=127.5±0.01；参考 0 dB；20 Hz/20 kHz 端点 measured；fs=32000 时 20 kHz
  not_applicable_at_fs；fs=96k 设计/端点/阻带；0.45fs<=−60 dB（§3.8 与 apodizing 区分）；
- 振铃（§1.4/§3.5 判据 6）：V16 pre_ring=−30.33 dB、pre_energy=−24.61 dB、post 对称、
  decay(−80 dB)=373 samples、[21k,0.499fs) max=−108.0 dB（T04 V16 参考值）；支撑过短（taps=8、
  W=16）时两侧 n/a 不硬算；
- 负例（crafted coeffs，验证真能拒绝坏设计）：1.5-sample 平滑（阻带 −12 dB）→ 判据 3/4 FAIL；
  系数 ×1.01 → 判据参考 FAIL（E_FIR_REF）；破坏对称 → E_FIR_PHASE_CLASS；矩形窗 sinc
  （Gibbs 纹波 ~0.8 dB）→ 判据 2 FAIL；taps 不符/NaN/Inf/全零 → E_FIR_TAPS/E_FIR_NUMERIC；
- design_checked 闭环：可行 → Ok；L<L_req → E_FIR_BUDGET；未实现 method → E_FIR_FAMILY；
- E_FIR_* 八类 code/Display 稳定性。

## 证据

rt-stack/evidence/T36/MANIFEST.md 记录输入/命令/exit code/executed test 数/关键数值/未测项；
真实执行日志见 evidence/T36/*.log，复现脚本 evidence/T36/repro.ps1。
