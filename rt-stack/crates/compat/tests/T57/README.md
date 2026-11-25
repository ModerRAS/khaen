# tests/T57 —— tspi-dsp-compat roles 私有测试（WBS T57）

- 任务：WBS T57（role-correction；team task-113）；写域 crates/compat/tests/T57/。
- 文件：main.rs（cargo 自动发现为集成测试 target，名 T57；与 T13/T23 同模式）。
  不需要改 tests/public_registration.rs（integrator 独占）。
- 运行（在 rt-stack/ 下；构建输出可用仓库外 CARGO_TARGET_DIR）：

    cargo test -p tspi-dsp-compat fullrange_height     # WBS T57 DoD（测试名含 fullrange_height）
    cargo test -p tspi-dsp-compat --test T57           # T57 target 全量（20 tests）
    cargo test -p tspi-dsp-compat                       # 整个 crate（T13 50 + T57 20；无回归）

- 只读输入：overlay/web layout-presets.yml 与 calibration-spec 示例经 include_str! 绑定；
  不改任何预设/校准文件，不写临时文件（本机 %TEMP% 在内存盘 R:，写盘可能 PermissionDenied）。
- 覆盖：overlay 4.1.2 唯一映射+hash 绑定、ch6 空/scatter/设备宽度、Web SUB4 drift 检出、
  corrected 高度全频 identity（无 HP/LP/18 ms）、未知滤波拒收/不按 role 名注入、
  MIMO participants 显式 5/dense→DAC、layout hash 语义稳定/映射敏感、其它布局通用解析、
  input/remap 解析、YAML 子集与负向矩阵、T13 LayoutRoleTable/RenderRoleBase 互通。
- 证据：rt-stack/evidence/T57/MANIFEST.md（命令/exit/executed test_count/输入与交付哈希/NOT_RUN）。
