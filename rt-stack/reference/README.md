# rt-stack/reference — KHAEN f64 核心卷积参考（WBS T03，P0）

2026-11-16 · 角色 **rt-reference**（team task-23）。写域：`projects/khaen-audio-dsp/rt-stack/reference/`。
状态：**P0 交付完成 + T03-review（2×P2+5×P3）已修（2026-11-17），待 Lead 人工 review**；非生产代码、非 Cargo 包、不碰板端。

本目录是 WBS T03 的交付：**独立的 f64 参考规范 + 手算 impulse/卷积向量 + 逐样本夹具**，
供后续 direct（T07）/FFT（T08）/fusion（T12）/file runner（T14）/oracle 与 dual-graph（T15/T58/T27）对拍。
在 T06（唯一 Cargo workspace）与未来 `rt-check` 尚不存在时，本包用 Python 标准库即可独立生成、验证、审阅。

- 规范（normative）：[SPEC.md](SPEC.md)
- 手算推导（可机器解析）：[HAND-CALC.md](HAND-CALC.md)
- 人工 review 检查表：[REVIEW-CHECKLIST.md](REVIEW-CHECKLIST.md)
- 证据（输入 hash/命令/exit code/未测项）：[evidence/T03/manifest.json](evidence/T03/manifest.json)

## 1. 文件地图

```
reference/
├── SPEC.md                     # khaen-reference/v1：模型/长度/流式/精度/夹具格式/消费者契约
├── HAND-CALC.md                # H1–H13 手算推导 + json handcalc/handcheck/handtrap 机器块
├── README.md
├── REVIEW-CHECKLIST.md         # 人工 review 清单 + 签收栏
├── fixtures/
│   ├── index.json              # 14 cases + sha256/bytes（输入 hash 绑定）
│   └── cases/C01..C14.json     # 逐样本输入/expected/streaming/traps
├── tools/
│   ├── gen_fixtures.py         # 确定性生成器（--check 默认 / --write）
│   ├── verify_reference.py     # 独立验证器（exact 复算 + f64 + streaming + 手算块 + hash；--negative-validation 负向/公式/trap）
│   ├── ref_f64.py              # 独立 binary64 参考实现（offline/streaming CLI）
│   └── make_evidence.py        # 运行命令并写 evidence/T03/manifest.json
└── evidence/T03/
    ├── manifest.json           # 输入/输出 sha256、命令、exit code、DoD、未测项/假设
    ├── verify-report.json      # 108 checks 明细（默认）
    ├── verify-negative-report.json  # --negative-validation 12 项（非法夹具拒绝/公式/trap）
    ├── VERIFY.txt / GEN-CHECK.txt / F64-OFFLINE.txt / F64-STREAM.txt / NEG-VALID.txt / env.txt
```

注：`real_holdout/` 子目录属 **T68**（独立 checker），本任务未创建、未占用。

## 2. 一键复现

在 `projects/khaen-audio-dsp/rt-stack/reference/` 下（Python 3.12+，无需 numpy/pip）：

```powershell
python tools/gen_fixtures.py --check
python tools/verify_reference.py --report evidence/T03/verify-report.json
python tools/verify_reference.py --negative-validation --report evidence/T03/verify-negative-report.json
python tools/make_evidence.py
```

实测（2026-11-17，T03 复审修复后）：

- `gen --check`：exit 0，14 fixtures + index 与生成内容逐字节一致；
- `verify`：exit 0，**14 fixtures / 108 checks / 0 failures**；手算块 15 个（handcalc 11、handcheck 1、handtrap 3），覆盖 11 个用例；
- 每例执行 exact 有理复算、f64 参考比较、声明 block_size（1/2/3/64/128）流式一致性、**语义强制 tag**（17 个；`baseline`/`random_exact` 为描述性索引，不单独断言）与 trap 反断言；
- `verify --negative-validation`：新增 **12 项**（8 类非法夹具必须被 validate_fixture 拒绝 + 3 个 trap 错误公式独立重建 + 1 个 handcheck 对 ref_f64 独立复查），exit 0 / 0 failures；
- **context 输入快照说明（P3-4）**：`evidence/T03/manifest.json` 的 `inputs_external` 是生成时点（2026-11-16 23:54）快照；authority（WBS/DECISION/CPU-BUDGET）hash 稳定，context 文档（CONTRACT/API/E2E/MIMO-*/upmix profile 等）随后续工作流更新属预期，权威语义以当时版本为准；重跑 `make_evidence.py` 会刷新输入/输出 hash。
- 证据原始文件见 `evidence/T03/`。

## 3. T03 DoD 对照

| WBS T03 DoD | 交付/证据 |
|---|---|
| P0 独立 f64 参考规范 | `SPEC.md` §1–§9（模型/长度/流式/精度/夹具/消费者契约）；不依赖 T06/rt-check |
| 手算 impulse 向量与夹具数据 | `HAND-CALC.md` H1–H13；`fixtures/cases/` C01–C14 |
| 含 offdiag / 尾 / D | C03/C06/C07/C09/C10/C11/C12/C14（offdiag）、C02/C04/C05/C07/C09/C10/C12（tail）、C02/C05/C07/C09/C10/C11/C12/C13（D）；验证器强制 tag 语义并检查手算覆盖 |
| 文档解析卷积手算向量 + 输入 hash 人工 review | `HAND-CALC.md` 的 fenced json 块由 `verify_reference.py` 解析比对；`fixtures/index.json` 与 `evidence/T03/manifest.json` 记录 sha256；review 清单见 `REVIEW-CHECKLIST.md` |

## 4. 覆盖与边界

- 图模型：`A_map`（只选择/零填，无上混/下混）→ 输入侧 FIR `A_i` → `G=δ_D I + g`（5×5 低带嵌入）→ 输出对角 FIR `P` → 容器 scatter。
  公式 `F_ji = p_j*G_ji*A_i`，`L_F = L_A+L_G+L_P−2`（D 计入 `L_G`），`L_out = L_in+L_F−1`（含尾）。
- 布局：完整覆盖 4.1.2（FL0 FR1 SL2 SR3 HFL4 HFR5 SUB1=7，8 容器 ch6 空，participants 5）与 stereo_strict 零填；
  其它布局由 layout-presets 解析（T57），图模型与之无关。
- 精度：exact 夹具要求 binary64 逐位一致（值均为整数/二进制分式，任意求和顺序安全）；C11 为非二进制可表示系数的 tolerance 夹具（rtol=1e-12）。
- 负向：C04 圆周混叠、C06 `G·P` 列缩放、C14 输入 FIR 放输出侧——均以 trap 固化错误值供测试反断言，且 `--negative-validation` 用公式独立重建这三个错误值并校验非法夹具被拒。
- 口径交叉引用：本参考 `L_F` 含输入侧 `A_i`；T02 `FUSED_LEN_MAX`/T04 `L_F` 仅 G/P 融合（差 `L_A−1`）。block 256 仅参考/测试口径，runtime 生产见 T02 Q-11 `{64,128}`；整数 `D` 之外的半整数/分数设计 D 由 T02 `delay_q16`/系数承载，不在本参考 `graph.D`。
- 不在范围：测量/拟合/solver/holdout、H_raw、IIR/limiter/volume/保护图、多速率/ASRC/96k、f32 硬门、板端 RT 实测。

## 5. 变更纪律

- fixture expected 是 oracle，不手改；改动必须经 `gen_fixtures.py --write` 重建并由 `verify` 复核；
- schema 语义/容差判据变更须升版本并保留旧 fixtures；发现规范错误先记录差异再升版；
- 本目录不是 Cargo workspace：不要在此创建 `Cargo.toml`/`Cargo.lock`（T06 独占根文件）；
- 未来 Rust 测试加载 `fixtures/cases/*.json` 时，必须遵守 SPEC §4 比较规则并在各自 `evidence/<Txx>/` 记录实际执行的 test_count>0。
