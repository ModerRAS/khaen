# tests/T13 —— khaen-compat calibration 私有测试（WBS T13）

- 任务：WBS T13（calibration-adapter；team task-78）；写域 `crates/compat/tests/T13/`。
- 文件：`main.rs`（cargo 自动发现为集成测试 target，名 `T13`；类似 T12 的 `tests/T12/main.rs`）。
  不需要改 `tests/public_registration.rs`（integrator 独占）。
- 运行（在 `rt-stack/` 下；构建输出可用仓库外 `CARGO_TARGET_DIR`）：

  cargo test -p khaen-compat calibration           # WBS T13 DoD（测试名统一 calibration_ 前缀）
  cargo test -p khaen-compat calibration -- --list
  cargo test -p khaen-compat                        # 整个 crate（含 public_registration 0 tests）

- 只读输入：`calibration-spec/examples/calibration-4.1.2.example.yml`、
  `...-5.3.2.example.yml`、`...-v11-highshelf.example.yml` 经 `include_str!` 绑定；
  测试不改任何校准文件/配置文件，不写临时文件（本机 %TEMP% 在内存盘 R:，写盘可能 PermissionDenied）。
- 覆盖：v1/v1.1 解析与 §5 校验（28 类错误/17 类警告路径）、合并（delay 相加/polarity XOR/gain 相加/
  PEQ 追加且 type 不降级、二次应用/跨布局拒收）、IR 草案 version/hash/role/fs/taps/placement/P/phase/D/
  stale、curve 不双叠（TargetFir 单点、G 无 curve）、T11 目标曲线数据合同、T12 compile 接口
  （design_delay_to_plan / StagePlan / CompileOptions / CompileInput+compile）。
- 证据：`rt-stack/evidence/T13/MANIFEST.md`（命令/exit/executed test_count/输入与交付哈希/NOT_RUN）。
