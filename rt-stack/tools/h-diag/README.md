
# h-diag — 复数 H 矩阵诊断工具（4.1.2 低频 MIMO）

> 只读诊断工具：**不修改** `rt-stack/crates/modal-design/src/{solve,regularize,validate,artifact,timebase,target}.rs`，
> 不连板、不出声、不 apply、不替代 T6-cal / T6-M 验收。目标是在改动目标函数/求解器之前，用**真实多点测量的复数 H(f)** 回答：
> 哪些频带可控、哪些秩亏、对测量误差多敏感、design/holdout 是否空间一致。

与 `rt-stack/spec/mimo-objective-v1.md` v1.1 共用同一口径：1/3 oct 中心
`40/50/63/80/100/125/160/200 Hz`，`w_f=1.0`，`α_min=0.5`，`α_max=2.0`，
`ε=0.05`，`δ=1.0 dB`，`C≥0.50`，holdout 严重恶化 `> +3.0 dB`。

## 1. 输入：预注册点位清单 + 逐文件 analysis.json

h-diag **只从清单读取 design/holdout 划分与“文件 ↔ 测点/执行器”映射，绝不从文件名猜 role**。
输入目录示例（真实数据必须放在仓库 `rt-stack/evidence/` 下，provenance 才可能判 MEASURED）：

`@
my-run/
  positions.json                      # 预注册清单（本工具 schema）
  session.json                        # 可选，但 MEASURED 判定必须有（mic/sweep/session 身份）
  positions/p01-mlp/position.json     # 可选，与清单交叉核对
  positions/p01-mlp/FL-ch00.analysis.json
  positions/p01-mlp/FR-ch01.analysis.json
  ...                                 # 每个点位 5 个执行器各一份
`@

`positions.json`（schema `tspi-hdiag-positions/v1`，示例见 `examples/positions.example.json`）：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `schema` | 是 | 固定 `tspi-hdiag-positions/v1` |
| `label` | 否 | 本次测量批次名 |
| `layout` | 否 | 期望 `4.1.2` |
| `created_utc` / `frozen_at_unix` | 是* | 预注册冻结时间；holdout 采集必须晚于它（*MEASURED 结论需要，缺失 → INCOMPLETE） |
| `actuators` | 是 | 必须恰为 `["FL","FR","SL","SR","SUB1"]` |
| `excluded_roles` | 否 | 默认 `["HFL","HFR"]`（不参与低带矩阵） |
| `mic` | 否 | `{model, serial, cal_sha256}`；也可由 session.json 提供 |
| `session_json` | 否* | 相对路径；Rust ingest 的 `tspi-mimo-measurement/v1`（*MEASURED 结论需要） |
| `common_d0_samples` | 否 | 公共整数 D0，应用于 aligned_segment 相位（所有报告指标对该值不变，见 §5） |
| `freq_field` | 否 | `auto`（默认）`/fr/fr_modal`；auto 优先 `fr_modal`（低频分辨率高） |
| `positions[].label` | 是 | 唯一测点名 |
| `positions[].role` | 是 | `design` 或 `holdout`（`validation` 视为 holdout） |
| `positions[].weight` | 否 | 默认 1.0；**只有 design 权重进入 SVD** |
| `positions[].files` | 是 | `{执行器: analysis.json 相对路径}` 或 `{执行器: {path, sha256}}`，5 个执行器必须齐全；声明 sha256 时逐个核对（不匹配 → INCONSISTENT `hdiag_analysis_sha_mismatch`） |
| `positions[].metadata_sha256` | 否 | 声明 `position.json` 的 sha256；不匹配 → INCONSISTENT |
| `positions[].position_json` | 否 | 与 Rust `tspi-mimo-position/v1` 交叉核对（name/role/weight） |
| `expected.design_points` / `holdout_points` | 否 | 默认 6 / 3（spec） |
| `synthetic` / `claim` | 否 | 声明 synthetic/dry-run → SYNTHETIC_REFUSED |
| `demo_of_existing_data` | 否 | true = 非预注册的旧数据演示 → 不得给结论 |

`*.analysis.json`：旧工具 schema `tspi-channel-measurement/v1`，读取
`fr`（或自动选择 `fr_modal`）的 `freq_hz/db/phase_deg`、`delay_samples/delay_ms/delay_coarse_samples`、
`snr_db`、`peak_db`、`polarity`、`phase_ref`、`meta{``khaen-calibration-meta/v1``}`。

## 2. 用法（离线，只需 Python 3 + numpy）

`@bash
cd rt-stack/tools/h-diag

# 真实/合成数据诊断（写 hdiag.json + REPORT.md + CSV；有 matplotlib 时另出 PNG）
python hdiag.py run --manifest <run>/positions.json --out-dir <out>

# 可选：提供 applied-after 复测集（同 labels/roles）后，ENERGY_MIGRATION 与
# HOLDOUT_SEVERE_REGRESSION 由 measured after/before 计算，而不是 NOT_APPLICABLE
python hdiag.py run --manifest <before>/positions.json \
    --after-manifest <after>/positions.json --out-dir <out>

# 预检：清单/文件/元数据/门
python hdiag.py check-manifest --manifest <run>/positions.json

# 合成已知秩自测（43 项，全部 synthetic）
python hdiag.py selftest --out-dir <evidence>/selftest

python hdiag.py version
`@

退出码（与仓库 provenance guard 风格一致）：`0` = 诊断产出且结论可用；
`3` = 结论被扣（UNPROVEN / SYNTHETIC_REFUSED / INVALID_MEASUREMENT / INCONSISTENT /
INCOMPLETE，但 hdiag.json/REPORT.md 仍写）；`2` = 输入/用法错误；`1` = 意外失败。

输出：
`hdiag.json`（机器可读，schema `tspi-h-diag/v1`）、`REPORT.md`（人读，表 + 定义 + NOT_RUN）、
`per_frequency.csv`、`bands.csv`、`coherence_band.csv`、`lambda_tradeoff.csv`、
`holdout_prediction.csv`，以及（仅当 matplotlib 可用）`plots/*.png`。

## 3. 六个必答问题与实现口径

1. **奇异值谱 + 有效秩**：对 `Hw = diag(sqrt(w_p)) H` 做加权 SVD；
   有效秩 = `#{σ_i > max(1e-3·σ_1, σ_1·10^(-SNR/20))}`。
   `1e-3` 与 `solve.rs RANK_ZERO_REL=1e-6`（Gram 上）对应；噪声项保证“低于测量误差的方向不算可控”。
2. **条件数 / 最小奇异值**：`κ = σ_1/σ_rank_eff`；`κ_full = σ_1/σ_min` 仅诊断；
   秩亏时 `κ` 为 null（不拿 0 当分母造数）。
3. **执行器两两复相干性**：每频点 `c_ij(f)=|Σ_p w_p conj(H[p,i,f])H[p,j,f]| / (||h_i||·||h_j||)`，
   相位 `arg(Σ…)`；带平均在 `p` 与 `f` 上求和（`w_f=1.0`）。
   1 = 空间签名相同（冗余、不可分辨），0 = 正交（独立）。
4. **执行器组合可控性**：`SUB1` / `SUB1+FL+FR` / `ALL5` 各自 rank、κ、两把“可达深度”尺子：
   `D_null=10log10(P/(P-r))`（随机空间目标的平均抵消上限），
   `D_meas=20log10(1/(ε·κ))`（测量相对误差 `ε=10^(-SNR/20)` 经 κ 放大后的可靠反演深度），
   取最小为 bottleneck。两者都是**上界/失效机制暴露**，不是承诺。
5. **正则化 trade-off**：`σ_1` 归一化后 `g=(Hn^H Hn+λI)^{-1}Hn^H t`，
   `λ∈[0,1e-6,…,1]`；target `t` 为单位范数的声明探针（每频点固定种子复高斯方向 / 最弱保留模式），
   报告 residual=`||Hn g-t||` 与 effort=`Σ|g|²` 曲线、L-curve 拐点 λ、以及 effort 上限
   (0.01/0.1/1.0) 内可达的最小 residual。
6. **design vs holdout 空间一致性**：比较**右奇异向量 V（执行器空间 C^N）**的主子空间夹角
   （左奇异向量在位置空间，design/holdout 点位不同不可比）：`C=V_d^H V_h` 的奇异值
   =cos θ，报告 max/mean angle、σ_min overlap、distance；最大角 > 30° 或秩不一致 → overfit risk。
   另输出每 holdout 点/带 `R_k`（spec §2.2 公式；当前 identity G + design 复数均值场预测），
   标 AGREEMENT / INCONSISTENT。

## 4. 门（fail-closed）

- **provenance**（复用仓库 `tools/mimo-prep/mimo12k_guard_lite.py` 与
  `mimo_kit/provenance.py`，常量引用 `provenance.py:56-63`）：
  证据根外 → UNPROVEN；synthetic/dry-run/fixture 路径/标记/注册 sha → SYNTHETIC_REFUSED；
  MA1/ARC S24 字节映射伪影、死麦/死执行器行、clock/onset 非法 → INVALID_MEASUREMENT；
  无 session/mic 身份 → UNPROVEN。**synthetic 数据永不产生真实结论。**
- **测量一致性**：逐文件核对 `sample_rate / sweep_start/end/seconds / level_db / pre/post /
  playback_device+channels / capture_device+channels` 与公共频率网格；任何漂移 → INCONSISTENT
  （声源/音量/处理链/时间基准不一致会制造假秩和假相位）。另核对 role/点数/冻结顺序/
  polarity/文件名与 channel 一致性/SNR 下限。
- **完整性**：design < 6 或 holdout < 3 → INCOMPLETE；缺冻结时间/session → INCOMPLETE。
- 只有 `verdict = DIAGNOSTIC_ONLY` 才给“可控频带/秩亏频带”结论，且明确不是 T6-M 验收。

## 5. 相位与不变性（为什么可以不依赖 T49 CommonD0）

`H[p,n,f] = 10^(db/20)·exp(j·(phase_deg − 360·f·D0/fs))·polarity`。
`D0` 是**每测点公共**的整数延迟旋转，对所有执行器相同；因此奇异值、条件数、
相干幅值/相位差、子空间夹角、可达深度与 effort 曲线**全部与 D0 无关**（行/列的公共
单位相位不改变这些量）。raw H 仍记录 phase_ref 与是否应用 D0，便于审计。

## 6. 自测（synthetic，仅证明算法/流程）

`python hdiag.py selftest --out-dir <dir>` 运行 43 项：已知谱 SVD、尺度不变、条件数、
秩亏列、有效秩噪声阈值、4 类相干性、主子空间夹角（相同/旋转 30°/秩不匹配）、
1/3 oct 带边界、R_k 公式、ridge 闭式解/零空间/effort cap 单调、可控深度闭式、
provenance 分类（measured-like/synthetic marker/path/external/byte-artifact/缺 mic）、
以及 6 design + 3 holdout 的合成端到端（known rank 3、FL/FR 全相干）：必须被判
SYNTHETIC_REFUSED/UNPROVEN、结论扣留，但指标/JSON/REPORT 全部产出。
失败返回 rc=1。

## 7. NOT_RUN

真实多点（design ≥6 + holdout ≥3）尚未采集；无 applied G、无复测、无 EDC/RT60、
无 T6-cal/T6-M 验收。详见 `rt-stack/evidence/h-diag-20261121/NOT-RUN.md`。
