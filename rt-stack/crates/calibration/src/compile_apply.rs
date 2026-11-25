//! [NONRT] P/G 每 path FIR 编译 → prepare 资源准入 → version apply ack → 拒坏图 →
//! 热更自研 last-good 回滚 → phase 方向（WBS T66）—— 本文件是 WBS T66 写域。
//!
//! 本模块是新 daemon 校准链的「编译/热更/回滚」单 owner（WBS T66）：
//!
//! 1. **P/G 每 path FIR 编译**：把 T54 每 path G 修正系数（f32 WAV + manifest，不含 delta_D
//!    baseline）与 T64 目标 FIR（P_post 基线形状）/ T65 整数 delay+polarity 编译成
//!    TspiPlanHeaderV1/TspiPathV1/TspiDiagFirV1/TspiRoleV1 + 系数 blob 的可准入 plan 字节；
//!    factorized（G path + P diag）与 fused（F_ji = p_j * G_ji，zero-pad 线性卷积）两种形态
//!    都由 T12 tspi_dsp_core::compile 生成，本模块按 T12 成本报告（或显式 force）选择并打包，
//!    禁止静默截尾。plan 字节布局与 T20 rt-host::swap::parse_plan_arena 完全对齐
//!    （header 128B + path/diag/role 表 + 尾部署部 64B 对齐系数 blob）。
//! 2. **phase/mapping hash 完整**：编译产物的 graph_id（core-v1 §11.10 规范化 SHA-256）之外，
//!    报告必须绑定 T57 overlay/mapping hash、T65 phase ops/design hash、T54 artifact hash 链
//!    （artifact/manifest/solution/design plant/target/baseline/wav aggregate）；任一缺失/非法 =
//!    拒收（E_CA_MAPPING_HASH / E_CA_PHASE_HASH / E_CA_ARTIFACT_HASH）。
//! 3. **phase 方向正确（T65 §4 契约）**：测量复响应 X=|X|e^{+jφ}（正斜率=更晚到达），
//!    物理延迟 D 的因果 FIR 是 H=e^{-jωD}（T40 design_integer = delta(D)·polarity）。
//!    CoeffEmbedded 编码下纯 delay/polarity 通路必须逐样本等于 T40 delta（因果，索引 0..D 为 0）；
//!    T65 evidence 的 direction_ok/残余 ≤0.5 sample/min(D)=0/参考通道=最晚到达 任一不成立 = 拒收。
//!    **唯一延迟定义（F-16）**：线性相位 P FIR（T64 目标 FIR）的固有群延迟
//!    GD_p=(taps-1)/2 已含在系数里；`role.delay_samples`（T65 D_i）是额外整数延迟，
//!    总延迟 = GD_p + D_i。RoleMetadata（role 表延迟）与 CoeffEmbedded（convolve_delta
//!    再叠加 D_i）都不能与 GD_p 并存：**GD_p≠0 且 D_i≠0 一律拒收 E_CA_DOUBLE_COUNT**；
//!    定义落地前，带 target FIR 的角色只允许 D_i=0。
//! 4. **prepare 资源准入**：rt-host::admission 的 validate/plan_memory/admit_with（T18 冻结口径，
//!    MockMemory 注入）先行；容量/预算/RLIMIT/mlock/预触任一失败 → 拒启（不得 warn 后自称 RT），
//!    last-good 或 SafetyMute 由宿主保留。plan 由 T20 PlanRegistry::admit_with 登记并分配版本。
//! 5. **version apply ack（§5.3）**：stage → arm（写 pending + ARMED）→ [RT] master block 边界
//!    commit（T20 commit_swap_at_block_boundary）→ complete_swap（RG-1）→ 新版本 all-ack block
//!    （TspiSwapAckV1::note_block_all_ack，RG-2）。**只有** retire_ready=1 且 ack_mask 达到
//!    ACK_MASK(expected) 才允许把 actual_version 视为 applied（T24/T42/T70 消费本条）；未 ack
//!    的 commit 不构成 applied。
//! 6. **拒坏图**：打包后自校验（magic/abi/header/表排序/系数区间/taps/role 范围/CRC-32C/graph_id
//!    重算）+ T20 解析二次校验；任何命中 → 拒绝，绝不部分生效。
//! 7. **热更自研 last-good rollback（WBS §8：失败只自研 last-good 或 mute，不回旧 Camilla）**：
//!    未 commit 前失败 → abort_and_disarm（旧图仍生效）；已 commit 但 ack 未达 / T3 应用后复测
//!    失败 → SafetyMute → 完整重新 admission（§5.2：fault 后恢复只经完整 re-admission）→ 重新
//!    提交 last-good plan 并重新 all-ack；无 last-good → 保持 SafetyMute（不输出半图）。回滚
//!    **绝不**启动旧 C 进程/服务，也不从 C 读系数。
//! 8. **T3 应用后复测（ACCEPTANCE §5 / K4）+ D7 headroom**：applied 只是 ack 完成；最终
//!    confirmed 必须有 measured T3 证据（残余延迟 ≤0.5 sample、极性全一致、FR 偏差 ≤1 dB、
//!    目标偏差 ≤±3 dB、PEQ 实现差 ≤0.75 dB、峰值 ≤−1 dBFS、holdout 已报）。缺实测 = INCOMPLETE
//!    （禁止 PASS_OPEN_LOOP）；D7（CONTRACT §3.3/§11）：单段 boost ≤+3 dB、每通道正向总和
//!    ≤6 warn/>12 拒、预测累计峰值 ≤−6 dBFS（headroom ≥6 dB）在编译期硬拒。F-35：D7 证据必须
//!    绑定 T64 报告**原始字节**（sha256），并用**已编码 P 行**重算峰值/headroom（不再只信申报数字）。
//!
//! # 依赖（只读消费，不改上游文件）
//!
//! - T54 tspi_modal_design::artifact：每 path WAV/manifest/version/hash/5x5 participants/
//!   stereo mask/overlay DAC/P/fs/D 与 ArtifactCurrent/stale 判定；
//! - T64 crate::peq_fit：目标 FIR / D7 boost-headroom 证据 / P 基线形状（本模块只吃类型化输入）；
//! - T65 crate::phase_fit：整数 delay/polarity ops + FIR 执行方向/残余检查（PhaseChannelOp/
//!   PhaseFitChecks）；
//! - T12 tspi_dsp_core::compile：factorized/fused 图、长度/成本/选择、零填充线性卷积；
//! - T18 tspi_rt_host::admission：validate/plan_memory/admit_with（MockMemory 离线注入）；
//! - T20 tspi_rt_host::swap：PlanRegistry/admit/stage/arm/complete_swap/swap-ack/quarantine；
//! - T57 tspi_dsp_compat::roles（经 T54 artifact scope 的 overlay 绑定哈希）；T13/T42/T70 消费
//!   本模块的 apply ack/actual version/rollback 结果。
//!
//! # 契约缺口登记（不改他人写域；报 integrator/Lead）
//!
//! - GAP-T66-1：core::plan::pack_plan/validate_plan/recompute_graph_id 仍是 T06 scaffold
//!   （ERR_UNSUPPORTED）；本 leaf 按 core-v1 §11.10 自建打包/校验（布局与 T20 parse 对齐），
//!   待 integrator/T26 统一后本模块改为调用 core::plan。
//! - GAP-T66-2：ABI v1.0 的 TspiPathV1/TspiDiagFirV1 无 delay_q16 字段（§11.10 v1.1 才有，
//!   现为 reserved1 必须为 0）；本模块要求输入 delay_q16==0，非零 = E_CA_DELAY_Q16_ABI 拒收
//!   （T65 op 是整数 sample，不丢精度；T12 分数 D 需先折入系数）。
//! - GAP-T66-3：plan ABI v1.0 无 role IIR/节点系数区（core-v1 §15.2 Q-06 未决，归 T23/T41）；
//!   本模块 peq_count>0 一律拒收（E_CA_PEQ_ABI），禁止静默丢弃 PEQ；**残差 FIR 不能单独代表 P
//!   （H_eff≠Hraw·P）**，IIR/节点区落地前可部署部分只能由 T64 目标 FIR 承载（F-37）；入口
//!   compile_plan 与自校验 CompiledPlan::verify 使用同一 peq_count>0 拒收规则（F-38）。
//! - GAP-T66-4：TspiRoleV1.gain_q15 只能表示 ≤1.0 线性增益（0 dB 最大）；正 trim 需增益节点
//!   （T23）或折入 P FIR；本模块拒绝 role gain_db > 0（E_CA_GAIN_RANGE）。
//! - GAP-T66-5：graph_id 规范化在 §11.10 规则 4（header 含 plan_version）与注 5（plan_version
//!   不参与）之间字面冲突；本模块按「不含 plan_version」实现并在 verify 中固定该口径。
//! - GAP-T66-6：PlanRegistry::ensure_new_graph 拒绝同 graph_id 再准入（§11.10 注 5 = stale）；
//!   因此回滚 = 宿主 SafetyMute + 完整 re-admission 的**新 runtime session**（fresh registry，
//!   version 计数重置），不冒充 registry 内版本倒退（INV-L7 禁止倒退）。宿主接线归 T26。
//! - GAP-T66-7：T12 matrix_stage_plans 对所有角色（含非 participant 的 HFL/HFR）都生成 delta_D
//!   对角 baseline；本模块按此打包，语义解释为「全输出公共 bulk delay」，若 T02/T12 认为
//!   非 participant 不应加 D，需 T12 修正后本模块复测（与 evidence/T66/manifest.md §4 同一登记）。
//!
//! # 未测（NOT_RUN，禁止当已验证）
//!
//! - 无板端/ALSA/CueMix/真实扫频/UMIK 数据：全部为离线模型与合成夹具；未做真实 xrun/RT/soak；
//! - 未打真实 T3/T4/T6 实测，record_t3_retest 只校验证据字段与门，不替代 T67/T68/T69；
//! - 未改 Cargo.toml/lib.rs/public_registration.rs/CLI；不新增依赖（std + 既有 crate）；
//! - 不 git commit；不碰 S1/服务/模式。
//!
//! 私有测试：tests/T66/compile_apply_cases.rs 由本文件末尾
//! #[cfg(test)] mod t66_tests { include!(...) } 编进 lib 单测（cargo 不自动发现 tests/<Txx>/；
//! 公共注册属 integrator，不改 public_registration.rs）。运行：cargo test -p tspi-calibration compile_apply

use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tspi_dsp_compat::ws::{json_parse, JsonValue};
use tspi_dsp_core::abi::{
    plan_abi_supported, TspiBlockAckV1, TspiDiagFirV1, TspiPathV1, TspiPlanHeaderV1,
    TspiPlanIirHeaderV2, TspiRoleIirV2, TspiRoleV1, PLAN_IIR_ENTRY_BYTES,
    PLAN_IIR_HEADER_BYTES, PLAN_IIR_MAX_SECTIONS, PLAN_IIR_SECTION_COEFFS, TSPI_PLAN_ABI_V1,
    TSPI_PLAN_ABI_V2, TSPI_PLAN_IIR_MAGIC, TSPI_PLAN_IIR_VERSION, TSPI_PLAN_MAGIC,
};
use tspi_dsp_core::compile::{
    compile, CompileInput, CompileOptions, CompiledPlans, FusionForm, SelectionReason,
    COMPILE_NO_SOURCE,
};
use tspi_dsp_core::events::{
    event_name, EV_EPOCH_START, EV_PLAN_ABORTED, EV_PLAN_ADMITTED, EV_PLAN_PREPARED,
    EV_PLAN_STAGED, EV_PLAN_SWAPPED, EV_ROLLBACK, EV_SAFETY_MUTE_ENTER, EV_SAFETY_MUTE_EXIT,
    EV_SWAP_ACK,
};
use tspi_dsp_core::nodes::crc32c;
use tspi_dsp_core::plan::{PlanBuf, PlanToken, StageToken};
use tspi_dsp_core::rt::{
    PlanVersion, RtStatus, BLOCK_SIZES, MAX_DIAG_FIRS, MAX_INPUT_CHANNELS, MAX_MATRIX_PATHS,
    MAX_OUTPUT_CHANNELS, MAX_OUTPUT_ROLES, RT_WORKERS, SAMPLE_RATES_HZ, TAPS_EXTENDED_MAX,
    TAPS_NORMAL_MAX,
};
use tspi_dsp_core::workers::{ack_mask, TspiRtControlV1};
use tspi_fir_designer::modal_peq::{freq_upper_hz, log_frequency_grid};
use tspi_fir_designer::phase::{design_integer, IntegerDelaySpec, Polarity as PhasePolarity};
use tspi_modal_design::artifact::{
    decode_wav_f32_mono, ArtifactBundle, ArtifactCurrent, ArtifactPath, ArtifactScope,
    ArtifactStatus,
};
use tspi_rt_host::admission::{
    plan_memory, validate as admission_validate, AdmissionEnv, MemoryBackend,
};
use tspi_rt_host::swap::{commit_swap_at_block_boundary, PlanRegistry, TspiSwapAckV1};

use crate::job::{sha256_hex, ArtifactKind, ArtifactRecord, StageCtx, StageFailure};
use crate::peq_fit::{FIT_FREQ_MIN_HZ, REFERENCE_FREQ_HZ, REPORT_GRID_OCTAVE};
use crate::peq_plan::PeqBiquad;
use crate::phase_fit::{PhaseChannelOp, PhaseFitChecks};

// ---------------------------------------------------------------------------
// schema / 常量 / 冻结门限
// ---------------------------------------------------------------------------

/// 编译报告 schema。
pub const COMPILE_APPLY_SCHEMA: &str = "tspi-cal-compile-apply/v1";
/// apply ack 报告 schema。
pub const APPLY_ACK_SCHEMA: &str = "tspi-cal-apply-ack/v1";
/// T3 应用后复测证据 schema（本模块只校验，不产生实测）。
pub const T3_RETEST_SCHEMA: &str = "tspi-cal-t3-retest/v1";
/// 工具名。
pub const COMPILE_APPLY_TOOL_NAME: &str = "tspi-cal-compile-apply";
/// 工具版本。
pub const COMPILE_APPLY_TOOL_VERSION: &str = "t66-1";
/// 编译报告 artifact 相对路径（job artifacts_dir 下）。
pub const COMPILE_REPORT_ARTIFACT_PATH: &str = "compile/compile-report.json";
/// 打包 plan 字节 artifact 相对路径。
pub const PLAN_ARTIFACT_PATH: &str = "compile/plan.bin";
/// apply ack artifact 相对路径。
pub const APPLY_ACK_ARTIFACT_PATH: &str = "apply/apply-ack.json";
/// 编译报告 canonical hash 域。
pub const COMPILE_REPORT_HASH_DOMAIN: &str = "tspi-cal-compile-apply-canonical-v1";
/// plan graph_id 规范化域（core-v1 §11.2 GRAPH_HASH_DOMAIN 字符串 + 0x00）。
pub const GRAPH_HASH_DOMAIN: &[u8] = b"tspi-plan/v1\0";
/// 编译期 plan_version（未准入；由 T20 registry 分配）。
pub const PLAN_VERSION_UNASSIGNED: u32 = 0;

/// T54 artifact schema（G 系数产物）。
pub const ARTIFACT_SCHEMA: &str = "tspi-modal-artifact/v1";
/// T65 phase fit schema。
pub const PHASE_FIT_SCHEMA: &str = "tspi-phase-fit/v1";
/// crossover 接线证据 schema（T39 设计 + 合并平坦 + 每 role 卷积后 P 行预测）。
pub const CROSSOVER_EVIDENCE_SCHEMA: &str = "tspi-cal-crossover-evidence/v1";
/// T64 peq fit schema（D7 headroom 证据来源）。
pub const PEQ_FIT_SCHEMA: &str = "tspi-peq-fit/v1";
/// T65 FIR 执行残余容差（ACCEPTANCE §5 T3 残余延迟 ≤0.5 sample）。
pub const PHASE_EXECUTION_TOLERANCE_SAMPLES: f64 = 0.5;
/// 契约延迟上限（CONTRACT §3.1：floor(0.2·fs) ms 对应 200 ms）。
pub const ROLE_DELAY_MAX_MS: f64 = 200.0;
/// D7 单段 boost 硬帽（CONTRACT §3.3/§11；producer 硬帽）。
pub const D7_SINGLE_BOOST_CAP_DB: f64 = 3.0;
/// D7 每通道正向总和 warning 门。
pub const D7_POSITIVE_SUM_WARN_DB: f64 = 6.0;
/// D7 每通道正向总和 reject 门。
pub const D7_POSITIVE_SUM_REJECT_DB: f64 = 12.0;
/// D7 预测累计峰值上限。
pub const D7_PREDICTED_PEAK_LIMIT_DBFS: f64 = -6.0;
/// D7 最小 headroom。
pub const D7_MIN_HEADROOM_DB: f64 = 6.0;
/// F-35：编码后 P 行幅度链峰值增益与 T64 报告隐含 chain gain 的绑定容差（dB）。
///
/// T64（peq_fit）用 1/24 oct 网格的幅度链算 predicted peak；本模块用同一口径对**真正下发的
/// 编码 P 行**重算。若系数被替换/更热，链增益与报告值的差会超过该容差（f32 量化的正常差 <1e-5 dB）。
const D7_P_ROW_BIND_TOL_DB: f64 = 0.01;
/// plan IIR 段系数与 T64 报告 b/a 的绑定容差（f32 量化；报告 f64 经最短往返 JSON）。
const PEQ_PLAN_COEFF_BIND_TOL: f64 = 1e-6;
/// 单段 PEQ 绝对正峰帽容差（dB；f32 量化后复核 D7 单段 +3 dB）。
const PEQ_SECTION_PEAK_TOL_DB: f64 = 0.01;
/// T3 残余延迟通过门（ACCEPTANCE §5）。
pub const T3_RESIDUAL_DELAY_TOLERANCE_SAMPLES: f64 = 0.5;
/// T3 残余延迟 hard fail 门。
pub const T3_RESIDUAL_DELAY_HARD_FAIL_SAMPLES: f64 = 1.0;
/// T3 FR 偏差容差。
pub const T3_FR_DEVIATION_TOLERANCE_DB: f64 = 1.0;
/// T3 目标偏差容差（±3 dB，D7 允许 ≤+3 dB boost 填谷）。
pub const T3_TARGET_DEVIATION_DB: f64 = 3.0;
/// T3 PEQ 实现差容差。
pub const T3_PEQ_TOLERANCE_DB: f64 = 0.75;
/// T3 实测峰值上限。
pub const T3_MEASURED_PEAK_LIMIT_DBFS: f64 = -1.0;
/// role gain 下限（channelmap 域）。
pub const ROLE_GAIN_MIN_DB: f64 = -60.0;
/// role gain 上限（channelmap 域；plan 只能编码 ≤0 dB，见 GAP-T66-4）。
pub const ROLE_GAIN_MAX_DB: f64 = 12.0;
/// 系数 blob 对齐（core-v1 A-ABI-8）。
pub const COEFF_ALIGN_BYTES: usize = 64;
/// plan header 字节数（core-v1 §11.10）。
pub const PLAN_HEADER_BYTES: usize = 128;
/// path 表项字节数（§11.10）。
pub const PATH_ENTRY_BYTES: usize = 32;
/// diag 表项字节数（§11.10）。
pub const DIAG_ENTRY_BYTES: usize = 32;
/// role 表项字节数（§11.10）。
pub const ROLE_ENTRY_BYTES: usize = 32;
/// affinity 表项字节数（§11.13）。
pub const AFFINITY_ENTRY_BYTES: usize = 32;
/// role id 最大字节数（T54/T57 token 上界）。
pub const MAX_ROLE_ID_BYTES: usize = 64;

/// plan flags bit0：stereo strict（FL/FR 直出，其余输出零填）。
pub const PLAN_FLAG_STEREO_STRICT: u8 = 1 << 0;
/// plan flags bit1：modal_support_to_other_outputs（默认 0，不实现上/下混）。
pub const PLAN_FLAG_MODAL_SUPPORT: u8 = 1 << 1;
/// plan flags bit2：crossfade（本模块不生成）。
pub const PLAN_FLAG_CROSSFADE: u8 = 1 << 2;
/// plan flags bit3：extended taps 已准入。
pub const PLAN_FLAG_EXTENDED_TAPS_ADMITTED: u8 = 1 << 3;
/// plan flags bit4：fallback graph（本模块不生成）。
pub const PLAN_FLAG_FALLBACK_GRAPH: u8 = 1 << 4;
/// plan flags 已知位掩码。
pub const PLAN_FLAG_MASK: u8 = 0x1F;

/// path flags bit0：含 delta_D 对角 baseline（系数内，kernel 不得重复加）。
pub const PATH_FLAG_DIAG_BASELINE_INCLUDED: u8 = 1 << 0;
/// path flags bit1：fused 路径。
pub const PATH_FLAG_FUSED: u8 = 1 << 1;
/// path flags bit2：zero-pad 线性卷积融合。
pub const PATH_FLAG_ZERO_PAD_LINEAR_CONV: u8 = 1 << 2;
/// path flags bit3：输入侧延迟（本模块不生成）。
pub const PATH_FLAG_INPUT_SIDE_DELAY: u8 = 1 << 3;

/// diag flags bit2：crossover 段（core nodes <code>DIAG_FLAG_CROSSOVER</code> 同值）。
pub const DIAG_FLAG_CROSSOVER: u16 = 1 << 2;
/// diag flags bit3：target FIR。
pub const DIAG_FLAG_TARGET: u16 = 1 << 3;
/// diag flags bit4：phase/delay/polarity only。
pub const DIAG_FLAG_PHASE: u16 = 1 << 4;
/// diag flags 已知位掩码。
pub const DIAG_FLAG_MASK: u16 = 0x1F;

/// role flags bit0：enabled。
pub const ROLE_FLAG_ENABLED: u16 = 1 << 0;
/// role flags bit1：zero_fill（该角色无节目源时必须显式零填）。
pub const ROLE_FLAG_ZERO_FILL: u16 = 1 << 1;
/// role flags 已知位掩码。
pub const ROLE_FLAG_MASK: u16 = 0x03;

/// apply 状态：空闲（无 prepared plan）。
pub const APPLY_STATE_IDLE: &str = "idle";
/// apply 状态：已 prepare（admission 通过，未 stage）。
pub const APPLY_STATE_PREPARED: &str = "prepared";
/// apply 状态：已 commit，等待新版本 all-ack（未 applied）。
pub const APPLY_STATE_AWAITING_ACK: &str = "awaiting_ack";
/// apply 状态：ack 完成（actual version 生效），但 T3 应用后复测未记录。
pub const APPLY_STATE_APPLIED: &str = "applied";
/// apply 状态：T3 measured 通过（confirmed）。
pub const APPLY_STATE_CONFIRMED: &str = "confirmed";
/// apply 状态：已回滚（last-good 重新生效）。
pub const APPLY_STATE_ROLLED_BACK: &str = "rolled_back";
/// apply 状态：SafetyMute（无可用 last-good；只静音，不输出半图）。
pub const APPLY_STATE_SAFETY_MUTE: &str = "safety_mute";

/// 回滚形态：未 commit，abort 在途切换（旧图保持生效）。
pub const ROLLBACK_MODE_ABORTED: &str = "aborted_before_commit";
/// 回滚形态：SafetyMute + 完整 re-admission 重新提交 last-good。
pub const ROLLBACK_MODE_READMIT_LAST_GOOD: &str = "bounce_readmit_last_good";
/// 回滚形态：无 last-good，只保持 SafetyMute。
pub const ROLLBACK_MODE_SAFETY_MUTE: &str = "safety_mute";

/// 汇总状态：INCOMPLETE（T3 未执行/未报，禁止 PASS）。
pub const OVERALL_INCOMPLETE: &str = "INCOMPLETE";
/// 汇总状态：APPLIED_NOT_CONFIRMED（ack 完成但 T3 未做）。
pub const OVERALL_APPLIED_NOT_CONFIRMED: &str = "APPLIED_NOT_CONFIRMED";
/// 汇总状态：CONFIRMED（ack + T3 measured 全过）。
pub const OVERALL_CONFIRMED: &str = "CONFIRMED";
/// 汇总状态：ROLLED_BACK。
pub const OVERALL_ROLLED_BACK: &str = "ROLLED_BACK";
/// 汇总状态：SAFETY_MUTE。
pub const OVERALL_SAFETY_MUTE: &str = "SAFETY_MUTE";

/// D7 warning：每通道正向总和 >6 dB（≤12 dB 接受）。
pub const W_D7_POSITIVE_SUM_WARN: &str = "W_D7_POSITIVE_SUM_WARN";
/// 实验 warning：T64 报告的 IIR 段（band_count>0）不进 plan（plan ABI v1.0 无 IIR 区）。
pub const W_EXPERIMENT_IIR_EXCLUDED: &str = "W_EXPERIMENT_IIR_EXCLUDED";
/// 实验 warning：进 plan 的 P 行（identity）与 T64 报告隐含链增益的 F-35 绑定被显式跳过。
pub const W_EXPERIMENT_P_BINDING_SKIPPED: &str = "W_EXPERIMENT_P_BINDING_SKIPPED";

// ---------------------------------------------------------------------------
// 稳定错误码（REST/log/evidence 引用；未知一律拒收）
// ---------------------------------------------------------------------------

pub const E_CA_OPTIONS: &str = "E_CA_OPTIONS";
pub const E_CA_FS: &str = "E_CA_FS";
pub const E_CA_BLOCK: &str = "E_CA_BLOCK";
pub const E_CA_CHANNELS: &str = "E_CA_CHANNELS";
pub const E_CA_ROLES: &str = "E_CA_ROLES";
pub const E_CA_TAPS: &str = "E_CA_TAPS";
pub const E_CA_CAPACITY: &str = "E_CA_CAPACITY";
pub const E_CA_PATH_TABLE: &str = "E_CA_PATH_TABLE";
pub const E_CA_MAPPING: &str = "E_CA_MAPPING";
pub const E_CA_MAPPING_HASH: &str = "E_CA_MAPPING_HASH";
pub const E_CA_PHASE: &str = "E_CA_PHASE";
pub const E_CA_PHASE_HASH: &str = "E_CA_PHASE_HASH";
pub const E_CA_PHASE_DIRECTION: &str = "E_CA_PHASE_DIRECTION";
pub const E_CA_PHASE_RESIDUAL: &str = "E_CA_PHASE_RESIDUAL";
pub const E_CA_ENCODING: &str = "E_CA_ENCODING";
pub const E_CA_DOUBLE_COUNT: &str = "E_CA_DOUBLE_COUNT";
pub const E_CA_D7_BOOST: &str = "E_CA_D7_BOOST";
pub const E_CA_D7_POSITIVE_SUM: &str = "E_CA_D7_POSITIVE_SUM";
pub const E_CA_D7_PEAK: &str = "E_CA_D7_PEAK";
pub const E_CA_ARTIFACT_STATUS: &str = "E_CA_ARTIFACT_STATUS";
pub const E_CA_ARTIFACT_STALE: &str = "E_CA_ARTIFACT_STALE";
pub const E_CA_ARTIFACT_HASH: &str = "E_CA_ARTIFACT_HASH";
pub const E_CA_NUMERIC: &str = "E_CA_NUMERIC";
pub const E_CA_DELAY_Q16_ABI: &str = "E_CA_DELAY_Q16_ABI";
pub const E_CA_PEQ_ABI: &str = "E_CA_PEQ_ABI";
pub const E_CA_GAIN_RANGE: &str = "E_CA_GAIN_RANGE";
pub const E_CA_COMPILE: &str = "E_CA_COMPILE";
pub const E_CA_PLAN_PACK: &str = "E_CA_PLAN_PACK";
pub const E_CA_PLAN_VERIFY: &str = "E_CA_PLAN_VERIFY";
pub const E_CA_STALE: &str = "E_CA_STALE";
pub const E_CA_ADMISSION: &str = "E_CA_ADMISSION";
pub const E_CA_STATE: &str = "E_CA_STATE";
pub const E_CA_COMMIT: &str = "E_CA_COMMIT";
pub const E_CA_ACK_TIMEOUT: &str = "E_CA_ACK_TIMEOUT";
pub const E_CA_ROLLBACK: &str = "E_CA_ROLLBACK";
pub const E_CA_T3: &str = "E_CA_T3";
pub const E_CA_EVIDENCE: &str = "E_CA_EVIDENCE";
/// crossover 证据缺失/非法/与 P 行不一致（D7 绑定 fail-closed）。
pub const E_CA_CROSSOVER: &str = "E_CA_CROSSOVER";
pub const E_CA_INTERNAL: &str = "E_CA_INTERNAL";

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// 编译/热更/回滚稳定错误（NONRT；码 + 文本 + 可选冻结 RtStatus，不进 RT 区）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileApplyError {
    code: &'static str,
    field: String,
    detail: String,
    status: Option<RtStatus>,
}

impl CompileApplyError {
    /// 构造（不含 RtStatus）。
    pub fn new(code: &'static str, field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: field.into(),
            detail: detail.into(),
            status: None,
        }
    }

    /// 附冻结 RtStatus（T18/T20 透传）。
    pub fn with_status(mut self, status: RtStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 字段路径。
    pub fn field(&self) -> &str {
        &self.field
    }

    /// 细节。
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// 冻结 RtStatus（若来自 T18/T20）。
    pub fn status(&self) -> Option<RtStatus> {
        self.status
    }

    /// 映射到 T42 REST 错误码（leaf-local 口径；未知 -> invalid_field）。
    pub fn rest_code(&self) -> &'static str {
        match self.code {
            E_CA_STALE | E_CA_ARTIFACT_STALE => "stale",
            E_CA_STATE | E_CA_COMMIT => "switch_busy",
            E_CA_ACK_TIMEOUT => "conflict",
            E_CA_D7_BOOST | E_CA_D7_POSITIVE_SUM | E_CA_D7_PEAK | E_CA_GAIN_RANGE => {
                "invalid_field"
            }
            E_CA_TAPS | E_CA_CAPACITY | E_CA_CHANNELS | E_CA_ROLES | E_CA_PATH_TABLE => {
                "invalid_plan"
            }
            E_CA_ADMISSION => "admission_failed",
            E_CA_ARTIFACT_STATUS | E_CA_ARTIFACT_HASH => "invalid_plan",
            _ => "invalid_field",
        }
    }
}

impl fmt::Display for CompileApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(
                f,
                "{}: {}: {} (RtStatus code {})",
                self.code,
                self.field,
                self.detail,
                status.code()
            ),
            None => write!(f, "{}: {}: {}", self.code, self.field, self.detail),
        }
    }
}

impl std::error::Error for CompileApplyError {}

/// 本模块结果别名。
pub type CaResult<T> = Result<T, CompileApplyError>;

fn ca_err(
    code: &'static str,
    field: impl Into<String>,
    detail: impl Into<String>,
) -> CompileApplyError {
    CompileApplyError::new(code, field, detail)
}

fn ca_status(code: &'static str, field: impl Into<String>, status: RtStatus) -> CompileApplyError {
    CompileApplyError::new(
        code,
        field,
        format!("冻结 RtStatus code {} 拒绝", status.code()),
    )
    .with_status(status)
}

// ---------------------------------------------------------------------------
// 通用校验 / 哈希辅助
// ---------------------------------------------------------------------------

/// 64 字符小写 hex（SHA-256 规范文本）。
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 32 字节 hash -> 64 字符小写 hex。
pub fn hex32(hash: &[u8; 32]) -> String {
    let mut text = String::with_capacity(64);
    for byte in hash {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// role token 校验（T54/T57：^[A-Z][A-Z0-9]*$，≤64 字节）。
pub fn is_role_token(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_ROLE_ID_BYTES {
        return false;
    }
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_uppercase() => {}
        _ => return false,
    }
    bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// 有限 f64 校验（NaN/Inf 一律拒收；RT 系数必须有限）。
pub fn require_finite(value: f64, field: impl Into<String>) -> CaResult<()> {
    if !value.is_finite() {
        return Err(ca_err(
            E_CA_NUMERIC,
            field,
            format!("非有限浮点值 {value} 进入 plan"),
        ));
    }
    Ok(())
}

fn require_sha256(value: &str, field: &str, code: &'static str) -> CaResult<()> {
    if !is_sha256_hex(value) {
        return Err(ca_err(
            code,
            field,
            format!(
                "需要 64 字符小写 SHA-256 hex，得到 {:?}",
                truncate(value, 96)
            ),
        ));
    }
    Ok(())
}

fn truncate(value: &str, limit: usize) -> &str {
    match value.char_indices().nth(limit) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

/// f64 -> f32 系数（拒绝非有限/溢出；不改数值语义）。
fn to_f32(coeffs: &[f64], field: &str) -> CaResult<Vec<f32>> {
    let mut out = Vec::with_capacity(coeffs.len());
    for (index, value) in coeffs.iter().enumerate() {
        if !value.is_finite() {
            return Err(ca_err(
                E_CA_NUMERIC,
                format!("{field}[{index}]"),
                format!("非有限系数 {value} 不得进入 plan"),
            ));
        }
        if value.abs() > f32::MAX as f64 {
            return Err(ca_err(
                E_CA_NUMERIC,
                format!("{field}[{index}]"),
                format!("系数 {value} 超出 f32 表示范围"),
            ));
        }
        out.push(*value as f32);
    }
    Ok(out)
}

fn f64_from_f32(values: &[f32]) -> Vec<f64> {
    values.iter().map(|value| *value as f64).collect()
}

fn max_abs(values: &[f64]) -> f64 {
    values
        .iter()
        .fold(0.0_f64, |acc, value| acc.max(value.abs()))
}

fn is_identity_fir(values: &[f64]) -> bool {
    values.len() == 1 && values[0] == 1.0
}

/// delta(D)·polarity（因果延迟：index==D 处 ±1，其余 0）。
fn delta_fir(delay_samples: u32, polarity: i8) -> Vec<f64> {
    let taps = delay_samples as usize + 1;
    let mut out = vec![0.0_f64; taps];
    out[delay_samples as usize] = polarity as f64;
    out
}

/// 两 FIR 逐样本相等（长度必须一致；容差 1e-12）。
fn firs_equal(a: &[f64], b: &[f64]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() <= 1e-12)
}

fn fusion_form_str(form: FusionForm) -> &'static str {
    match form {
        FusionForm::Factorized => "factorized",
        FusionForm::FusedGp => "fused_gp",
    }
}

fn selection_reason_str(reason: SelectionReason) -> &'static str {
    match reason {
        SelectionReason::FusedCheaper => "fused_cheaper",
        SelectionReason::FusedNotCheaper => "fused_not_cheaper",
        SelectionReason::FusedOverTapLimit => "fused_over_tap_limit",
        SelectionReason::FusedOverRouteLimit => "fused_over_route_limit",
        SelectionReason::FusedOverCoeffBudget => "fused_over_coeff_budget",
    }
}

// ---------------------------------------------------------------------------
// T57 overlay/mapping 绑定（role -> 物理输出槽位 + hash 完整性）
// ---------------------------------------------------------------------------

/// 一个角色的 overlay 绑定（role 序号 / role id / DAC 首槽位 / 该 role 全部槽位）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleDacSlot {
    /// plan 内 role 序号（0..O-1，非物理通道号）。
    pub role_index: u16,
    /// 角色 id（大写 token）。
    pub role_id: String,
    /// 物理输出首槽位（= outputs[0]；CONTRACT：role.index = 首个 output）。
    pub dac: u16,
    /// 该 role 的全部物理输出槽位（严格升序、唯一）。
    pub outputs: Vec<u16>,
}

impl RoleDacSlot {
    /// 构造并校验单条 role 绑定。
    pub fn new(
        role_index: u16,
        role_id: impl Into<String>,
        dac: u16,
        outputs: Vec<u16>,
    ) -> CaResult<Self> {
        let role_id = role_id.into();
        let slot = Self {
            role_index,
            role_id,
            dac,
            outputs,
        };
        slot.validate(MAX_OUTPUT_CHANNELS as u16)?;
        Ok(slot)
    }

    /// 校验（container_channels 为设备容器宽度）。
    pub fn validate(&self, container_channels: u16) -> CaResult<()> {
        if !is_role_token(&self.role_id) {
            return Err(ca_err(
                E_CA_MAPPING,
                format!("mapping.roles[{}].role_id", self.role_index),
                format!("role id {:?} 不匹配 ^[A-Z][A-Z0-9]*$ 或超长", self.role_id),
            ));
        }
        if self.outputs.is_empty() {
            return Err(ca_err(
                E_CA_MAPPING,
                format!("mapping.roles[{}].outputs", self.role_index),
                "role 必须映射到至少一个输出槽位",
            ));
        }
        if self.outputs[0] != self.dac {
            return Err(ca_err(
                E_CA_MAPPING,
                format!("mapping.roles[{}].dac", self.role_index),
                format!(
                    "dac {} 必须等于 outputs 首个槽位 {}",
                    self.dac, self.outputs[0]
                ),
            ));
        }
        let mut previous: Option<u16> = None;
        for output in &self.outputs {
            if *output >= container_channels {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.roles[{}].outputs", self.role_index),
                    format!("输出槽位 {output} 越界（container_channels={container_channels}）"),
                ));
            }
            if let Some(previous) = previous {
                if *output <= previous {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        format!("mapping.roles[{}].outputs", self.role_index),
                        "outputs 必须严格升序且唯一",
                    ));
                }
            }
            previous = Some(*output);
        }
        Ok(())
    }
}

/// T57 overlay 解析结果的 plan 绑定（layout/overlay/role/participants hash + dense->DAC + role->DAC）。
///
/// 说明：T54 artifact scope 已带 overlay 绑定快照；T57 是权威 overlay 解析方。本结构同时接受
/// 两路来源（from_artifact_scope / new），compile 时交叉校验 dense/role/dac/unused 一致。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingBinding {
    /// layout id（4.1.2 等）。
    pub layout_id: String,
    /// overlay 源字节 sha256（T57 解析输入的原始字节 hash）。
    pub overlay_source_sha256: String,
    /// T57 overlay 绑定 hash（layout+roles+outputs）。
    pub binding_hash: String,
    /// 合并 role 绑定 hash（layout+participants+dense->DAC+stereo mask+overlay）。
    pub role_hash: String,
    /// participants 绑定 hash（T50 canonical）。
    pub participants_binding_hash: String,
    /// 容器/DAC 通道数。
    pub container_channels: u16,
    /// dense（G 行/列）-> DAC。
    pub dense_to_dac: Vec<u16>,
    /// 全部 plan 角色（按 role_index 升序）。
    pub roles: Vec<RoleDacSlot>,
    /// 未被任何 role 使用的输出槽位（4.1.2 = [6]）。
    pub unused_outputs: Vec<u16>,
    /// 是否 stereo strict 受限 profile。
    pub stereo_strict: bool,
    /// stereo mask（stereo strict 时恒 FL/FR）。
    pub stereo_mask: Vec<String>,
}

impl MappingBinding {
    /// 构造（全字段显式；调用方保证 hash 来自 T57/T54）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        layout_id: impl Into<String>,
        overlay_source_sha256: impl Into<String>,
        binding_hash: impl Into<String>,
        role_hash: impl Into<String>,
        participants_binding_hash: impl Into<String>,
        container_channels: u16,
        dense_to_dac: Vec<u16>,
        roles: Vec<RoleDacSlot>,
        unused_outputs: Vec<u16>,
        stereo_strict: bool,
        stereo_mask: Vec<String>,
    ) -> CaResult<Self> {
        let mapping = Self {
            layout_id: layout_id.into(),
            overlay_source_sha256: overlay_source_sha256.into(),
            binding_hash: binding_hash.into(),
            role_hash: role_hash.into(),
            participants_binding_hash: participants_binding_hash.into(),
            container_channels,
            dense_to_dac,
            roles,
            unused_outputs,
            stereo_strict,
            stereo_mask,
        };
        mapping.validate()?;
        Ok(mapping)
    }

    /// 从 T54 artifact scope 取 hash/dense->DAC/participants（非 participant role 由调用方 with_roles 补）。
    pub fn from_artifact_scope(scope: &ArtifactScope) -> CaResult<Self> {
        let mut roles = Vec::with_capacity(scope.participants.len());
        for participant in &scope.participants {
            let dac = match u16::try_from(participant.dac) {
                Ok(value) => value,
                Err(_) => {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        format!("artifact.scope.participants[{}].dac", participant.dense),
                        format!("DAC {} 超出 u16 槽位域", participant.dac),
                    ))
                }
            };
            let outputs = match scope
                .overlay
                .roles
                .iter()
                .find(|role| role.role_id == participant.role_id)
            {
                Some(role) => role.outputs.clone(),
                None => vec![dac],
            };
            roles.push(RoleDacSlot {
                role_index: participant.dense,
                role_id: participant.role_id.clone(),
                dac,
                outputs,
            });
        }
        let mut dense_to_dac = Vec::with_capacity(scope.dense_to_dac.len());
        for dac in &scope.dense_to_dac {
            match u16::try_from(*dac) {
                Ok(value) => dense_to_dac.push(value),
                Err(_) => {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        "artifact.scope.dense_to_dac",
                        format!("DAC {dac} 超出 u16 槽位域"),
                    ))
                }
            }
        }
        let mapping = Self {
            layout_id: scope.layout_id.clone(),
            overlay_source_sha256: scope.overlay.source_sha256.clone(),
            binding_hash: scope.overlay.binding_hash.clone(),
            role_hash: scope.role_hash.clone(),
            participants_binding_hash: scope.participants_binding_hash.clone(),
            container_channels: scope.container_channels,
            dense_to_dac,
            roles,
            unused_outputs: scope.overlay.unused_outputs.clone(),
            stereo_strict: scope.stereo_strict,
            stereo_mask: scope.stereo_mask.clone(),
        };
        mapping.validate()?;
        Ok(mapping)
    }

    /// 用调用方提供的完整 role 表替换 roles（必须含全部 plan 角色；按 role_index 升序）。
    pub fn with_roles(mut self, roles: Vec<RoleDacSlot>) -> CaResult<Self> {
        self.roles = roles;
        self.validate()?;
        Ok(self)
    }

    /// 校验 hash 完整 + dense/role/dac/unused 一致。
    pub fn validate(&self) -> CaResult<()> {
        if self.layout_id.trim().is_empty() || self.layout_id.chars().count() > 64 {
            return Err(ca_err(
                E_CA_MAPPING_HASH,
                "mapping.layout_id",
                "layout id 必须非空且 ≤64 字符",
            ));
        }
        require_sha256(
            &self.overlay_source_sha256,
            "mapping.overlay_source_sha256",
            E_CA_MAPPING_HASH,
        )?;
        require_sha256(
            &self.binding_hash,
            "mapping.binding_hash",
            E_CA_MAPPING_HASH,
        )?;
        require_sha256(&self.role_hash, "mapping.role_hash", E_CA_MAPPING_HASH)?;
        require_sha256(
            &self.participants_binding_hash,
            "mapping.participants_binding_hash",
            E_CA_MAPPING_HASH,
        )?;
        if self.container_channels == 0 || self.container_channels as usize > MAX_OUTPUT_CHANNELS {
            return Err(ca_err(
                E_CA_MAPPING,
                "mapping.container_channels",
                format!(
                    "container_channels={} 越界（1..={MAX_OUTPUT_CHANNELS}）",
                    self.container_channels
                ),
            ));
        }
        if self.dense_to_dac.is_empty() {
            return Err(ca_err(
                E_CA_MAPPING,
                "mapping.dense_to_dac",
                "participants 不能为空（M>=1）",
            ));
        }
        let mut seen_dac = vec![false; self.container_channels as usize];
        for (dense, dac) in self.dense_to_dac.iter().enumerate() {
            if *dac >= self.container_channels {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.dense_to_dac[{dense}]"),
                    format!(
                        "DAC {dac} 越界（container_channels={}）",
                        self.container_channels
                    ),
                ));
            }
            if seen_dac[*dac as usize] {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.dense_to_dac[{dense}]"),
                    format!("DAC {dac} 重复（dense->DAC 必须单射）"),
                ));
            }
            seen_dac[*dac as usize] = true;
        }
        if self.roles.is_empty() {
            return Err(ca_err(E_CA_ROLES, "mapping.roles", "plan 角色表不能为空"));
        }
        if self.roles.len() > MAX_OUTPUT_ROLES {
            return Err(ca_err(
                E_CA_ROLES,
                "mapping.roles",
                format!("role 数 {} 超过 {MAX_OUTPUT_ROLES}", self.roles.len()),
            ));
        }
        let mut previous_index: Option<u16> = None;
        let mut seen_role_ids: Vec<&str> = Vec::with_capacity(self.roles.len());
        let mut used_outputs = vec![false; self.container_channels as usize];
        for (position, role) in self.roles.iter().enumerate() {
            if role.role_index as usize != position {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("mapping.roles[{position}].role_index"),
                    format!(
                        "role_index {} 必须等于表内序号 {position}（按 role_index 升序、无空洞）",
                        role.role_index
                    ),
                ));
            }
            role.validate(self.container_channels)?;
            if seen_role_ids.iter().any(|id| *id == role.role_id) {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("mapping.roles[{position}].role_id"),
                    format!("role id {:?} 重复", role.role_id),
                ));
            }
            seen_role_ids.push(role.role_id.as_str());
            for output in &role.outputs {
                if used_outputs[*output as usize] {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        format!("mapping.roles[{position}].outputs"),
                        format!("输出槽位 {output} 被多个 role 重复占用"),
                    ));
                }
                used_outputs[*output as usize] = true;
            }
            if let Some(previous) = previous_index {
                if role.role_index <= previous {
                    return Err(ca_err(
                        E_CA_ROLES,
                        format!("mapping.roles[{position}].role_index"),
                        "role_index 必须严格升序",
                    ));
                }
            }
            previous_index = Some(role.role_index);
        }
        // participants 的 DAC 必须在 role 表中存在且一致（防止 dense/role 双份映射漂移）。
        for (dense, dac) in self.dense_to_dac.iter().enumerate() {
            let Some(role) = self.roles.iter().find(|role| role.dac == *dac) else {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.dense_to_dac[{dense}]"),
                    format!("DAC {dac} 在 role 表中不存在（overlay/participants 不一致）"),
                ));
            };
            if !role.outputs.contains(dac) {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.dense_to_dac[{dense}]"),
                    format!("DAC {dac} 不在 role {:?} 的 outputs 中", role.role_id),
                ));
            }
        }
        let mut seen_unused = vec![false; self.container_channels as usize];
        for (position, output) in self.unused_outputs.iter().enumerate() {
            if *output >= self.container_channels {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.unused_outputs[{position}]"),
                    format!("未用槽位 {output} 越界"),
                ));
            }
            if seen_unused[*output as usize] {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.unused_outputs[{position}]"),
                    format!("未用槽位 {output} 重复"),
                ));
            }
            seen_unused[*output as usize] = true;
            if used_outputs[*output as usize] {
                return Err(ca_err(
                    E_CA_MAPPING,
                    format!("mapping.unused_outputs[{position}]"),
                    format!("槽位 {output} 同时被 role 使用且标为 unused"),
                ));
            }
        }
        if self.stereo_strict {
            let expected = ["FL", "FR"];
            if self.stereo_mask.len() != expected.len()
                || self
                    .stereo_mask
                    .iter()
                    .zip(expected.iter())
                    .any(|(actual, expected)| actual != expected)
            {
                return Err(ca_err(
                    E_CA_MAPPING,
                    "mapping.stereo_mask",
                    "stereo_strict 时 stereo_mask 必须恰为 [FL, FR]",
                ));
            }
            for id in expected {
                if !self.roles.iter().any(|role| role.role_id == id) {
                    return Err(ca_err(
                        E_CA_ROLES,
                        "mapping.roles",
                        format!("stereo_strict 缺少角色 {id}"),
                    ));
                }
            }
        } else if !self.stereo_mask.is_empty() {
            return Err(ca_err(
                E_CA_MAPPING,
                "mapping.stereo_mask",
                "非 stereo_strict 时 stereo_mask 必须为空",
            ));
        }
        Ok(())
    }

    /// DAC -> role_index。
    pub fn role_index_for_dac(&self, dac: u16) -> Option<u16> {
        self.roles
            .iter()
            .find(|role| role.dac == dac)
            .map(|role| role.role_index)
    }

    /// role_index -> RoleDacSlot。
    pub fn role(&self, role_index: u16) -> Option<&RoleDacSlot> {
        self.roles.iter().find(|role| role.role_index == role_index)
    }

    /// participants 数 M。
    pub fn participants_len(&self) -> usize {
        self.dense_to_dac.len()
    }

    /// mapping canonical 文本（确定性；进编译报告 hash）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str("tspi-cal-mapping/v1\n");
        let _ = writeln!(text, "layout_id={}", self.layout_id);
        let _ = writeln!(text, "overlay_source_sha256={}", self.overlay_source_sha256);
        let _ = writeln!(text, "binding_hash={}", self.binding_hash);
        let _ = writeln!(text, "role_hash={}", self.role_hash);
        let _ = writeln!(
            text,
            "participants_binding_hash={}",
            self.participants_binding_hash
        );
        let _ = writeln!(text, "container_channels={}", self.container_channels);
        text.push_str("dense_to_dac=");
        for (index, dac) in self.dense_to_dac.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(text, "{dac}");
        }
        text.push('\n');
        for role in &self.roles {
            let _ = write!(
                text,
                "role {} {} dac={} outputs=",
                role.role_index, role.role_id, role.dac
            );
            for (index, output) in role.outputs.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                let _ = write!(text, "{output}");
            }
            text.push('\n');
        }
        text.push_str("unused_outputs=");
        for (index, output) in self.unused_outputs.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(text, "{output}");
        }
        text.push('\n');
        let _ = writeln!(text, "stereo_strict={}", self.stereo_strict as u8);
        text.push_str("stereo_mask=");
        for (index, role) in self.stereo_mask.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(role);
        }
        text.push('\n');
        text
    }

    /// mapping hash（canonical 文本 SHA-256；编译报告绑定用）。
    pub fn mapping_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ---------------------------------------------------------------------------
// T65 phase evidence（delay/polarity + FIR 执行方向/残余）
// ---------------------------------------------------------------------------

/// 单 channel 的 T65 op（角色 -> 整数 delay + 校准极性）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChannelEvidence {
    /// 角色/通道名（= plan role id）。
    pub channel: String,
    /// 设备通道号（CONTRACT index；可选）。
    pub index: Option<u32>,
    /// 整数延迟补偿 D（samples；D_i = round(max_j t_j - t_i)，min=0）。
    pub delay_samples: u32,
    /// 校准极性（+1 normal / -1 invert）。
    pub polarity: i8,
}

/// T65 phase fit 证据（计划/方向/残余/参考通道 + 逐 channel ops）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseEvidence {
    /// 报告 schema（必须 = tspi-phase-fit/v1）。
    pub schema: String,
    /// phase ops canonical SHA-256（角色->整数 delay/polarity）。
    pub ops_sha256: String,
    /// T65 design report SHA-256。
    pub design_report_sha256: String,
    /// FIR 执行方向校验通过（T65 execution.fir_direction_ok）。
    pub direction_ok: bool,
    /// 各通道残余到达差 / 执行容差的最大值（samples）。
    pub residual_max_samples: f64,
    /// 执行容差（必须 ≤0.5 sample；ACCEPTANCE §5）。
    pub tolerance_samples: f64,
    /// 全通道 min(delay_samples)==0（CONTRACT §3.1）。
    pub min_zero_ok: bool,
    /// 参考通道为到达最晚（D=0）。
    pub reference_is_latest: bool,
    /// 参考通道名。
    pub reference_channel: String,
    /// 逐 channel ops（按 plan 角色顺序）。
    pub channels: Vec<PhaseChannelEvidence>,
}

impl PhaseEvidence {
    /// 构造（逐字段显式）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schema: impl Into<String>,
        ops_sha256: impl Into<String>,
        design_report_sha256: impl Into<String>,
        direction_ok: bool,
        residual_max_samples: f64,
        tolerance_samples: f64,
        min_zero_ok: bool,
        reference_is_latest: bool,
        reference_channel: impl Into<String>,
        channels: Vec<PhaseChannelEvidence>,
    ) -> Self {
        Self {
            schema: schema.into(),
            ops_sha256: ops_sha256.into(),
            design_report_sha256: design_report_sha256.into(),
            direction_ok,
            residual_max_samples,
            tolerance_samples,
            min_zero_ok,
            reference_is_latest,
            reference_channel: reference_channel.into(),
            channels,
        }
    }

    /// 从 T65 PhaseChannelOp + PhaseFitChecks 直接构造（真实集成入口）。
    ///
    /// residual_max_samples 由调用方从 T65 execution 报告取（本模块不重算测量残差）。
    pub fn from_ops(
        ops: &[PhaseChannelOp],
        checks: &PhaseFitChecks,
        ops_sha256: impl Into<String>,
        design_report_sha256: impl Into<String>,
        reference_channel: impl Into<String>,
        residual_max_samples: f64,
    ) -> Self {
        let channels = ops
            .iter()
            .map(|op| PhaseChannelEvidence {
                channel: op.channel.clone(),
                index: op.index,
                delay_samples: op.delay_samples,
                polarity: op.polarity.sign() as i8,
            })
            .collect();
        Self {
            schema: PHASE_FIT_SCHEMA.to_string(),
            ops_sha256: ops_sha256.into(),
            design_report_sha256: design_report_sha256.into(),
            direction_ok: checks.fir_direction_ok && checks.fir_design_ok,
            residual_max_samples,
            tolerance_samples: PHASE_EXECUTION_TOLERANCE_SAMPLES,
            min_zero_ok: checks.min_zero,
            reference_is_latest: checks.reference_is_latest,
            reference_channel: reference_channel.into(),
            channels,
        }
    }

    /// 单个 channel 证据。
    pub fn channel(&self, role_id: &str) -> Option<&PhaseChannelEvidence> {
        self.channels.iter().find(|entry| entry.channel == role_id)
    }

    /// 校验（hash 完整 + 方向/残余/契约延迟/min=0/参考通道）。
    pub fn validate(&self, sample_rate_hz: u32) -> CaResult<()> {
        if self.schema != PHASE_FIT_SCHEMA {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.schema",
                format!("schema 必须为 {PHASE_FIT_SCHEMA}，得到 {:?}", self.schema),
            ));
        }
        require_sha256(&self.ops_sha256, "phase.ops_sha256", E_CA_PHASE_HASH)?;
        require_sha256(
            &self.design_report_sha256,
            "phase.design_report_sha256",
            E_CA_PHASE_HASH,
        )?;
        if !self.direction_ok {
            return Err(ca_err(
                E_CA_PHASE_DIRECTION,
                "phase.direction_ok",
                "T65 FIR 执行方向校验未通过（conj(H)=e^{+jωD} 方向错误会把相对错位放大）",
            ));
        }
        require_finite(self.residual_max_samples, "phase.residual_max_samples")?;
        require_finite(self.tolerance_samples, "phase.tolerance_samples")?;
        if !(self.tolerance_samples > 0.0
            && self.tolerance_samples <= PHASE_EXECUTION_TOLERANCE_SAMPLES)
        {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.tolerance_samples",
                format!(
                    "执行容差 {} 必须 ∈(0, {}]（ACCEPTANCE §5）",
                    self.tolerance_samples, PHASE_EXECUTION_TOLERANCE_SAMPLES
                ),
            ));
        }
        if self.residual_max_samples < 0.0 || self.residual_max_samples > self.tolerance_samples {
            return Err(ca_err(
                E_CA_PHASE_RESIDUAL,
                "phase.residual_max_samples",
                format!(
                    "残余到达差 {} sample 超过容差 {}（ACCEPTANCE §5 残余延迟门）",
                    self.residual_max_samples, self.tolerance_samples
                ),
            ));
        }
        if !self.min_zero_ok {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.min_zero_ok",
                "全通道 min(delay_samples) 必须 =0（CONTRACT §3.1：D=0 通道=到达最晚参考）",
            ));
        }
        if !self.reference_is_latest {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.reference_is_latest",
                "参考通道必须是到达最晚（D=0）的通道",
            ));
        }
        if self.reference_channel.trim().is_empty() {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.reference_channel",
                "参考通道名必须非空",
            ));
        }
        if self.channels.is_empty() {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.channels",
                "逐 channel ops 不能为空",
            ));
        }
        let max_delay = (sample_rate_hz as u64) / 5;
        let mut seen: Vec<&str> = Vec::with_capacity(self.channels.len());
        let mut min_delay = u32::MAX;
        let mut reference_seen = false;
        for (position, channel) in self.channels.iter().enumerate() {
            if !is_role_token(&channel.channel) {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("phase.channels[{position}].channel"),
                    format!("通道名 {:?} 不是 role token", channel.channel),
                ));
            }
            if seen.iter().any(|id| *id == channel.channel) {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("phase.channels[{position}].channel"),
                    format!("通道 {:?} 重复", channel.channel),
                ));
            }
            seen.push(channel.channel.as_str());
            if channel.delay_samples as u64 > max_delay {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("phase.channels[{position}].delay_samples"),
                    format!(
                        "延迟 {} 超过 floor(0.2·fs)={}（CONTRACT §3.1 200 ms 上限，不截断）",
                        channel.delay_samples, max_delay
                    ),
                ));
            }
            if channel.polarity != 1 && channel.polarity != -1 {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("phase.channels[{position}].polarity"),
                    format!("极性 {} 必须为 ±1", channel.polarity),
                ));
            }
            min_delay = min_delay.min(channel.delay_samples);
            if channel.channel == self.reference_channel {
                reference_seen = true;
                if channel.delay_samples != 0 {
                    return Err(ca_err(
                        E_CA_PHASE,
                        format!("phase.channels[{position}].delay_samples"),
                        "参考通道 delay_samples 必须 =0",
                    ));
                }
            }
        }
        if !reference_seen {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.reference_channel",
                format!("参考通道 {:?} 不在 ops 中", self.reference_channel),
            ));
        }
        if min_delay != 0 {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.channels",
                "全通道 min(delay_samples) 必须 =0",
            ));
        }
        Ok(())
    }

    /// phase canonical 文本（确定性）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str("tspi-cal-phase-evidence/v1\n");
        let _ = writeln!(text, "schema={}", self.schema);
        let _ = writeln!(text, "ops_sha256={}", self.ops_sha256);
        let _ = writeln!(text, "design_report_sha256={}", self.design_report_sha256);
        let _ = writeln!(text, "direction_ok={}", self.direction_ok as u8);
        let _ = writeln!(
            text,
            "residual_max_samples={:.12}",
            self.residual_max_samples
        );
        let _ = writeln!(text, "tolerance_samples={:.12}", self.tolerance_samples);
        let _ = writeln!(text, "min_zero_ok={}", self.min_zero_ok as u8);
        let _ = writeln!(
            text,
            "reference_is_latest={}",
            self.reference_is_latest as u8
        );
        let _ = writeln!(text, "reference_channel={}", self.reference_channel);
        for channel in &self.channels {
            let _ = writeln!(
                text,
                "channel {} delay={} polarity={}",
                channel.channel, channel.delay_samples, channel.polarity
            );
        }
        text
    }

    /// phase hash（canonical 文本 SHA-256）。
    pub fn phase_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ---------------------------------------------------------------------------
// T64 D7 boost/headroom 证据
// ---------------------------------------------------------------------------

/// 单角色 D7 boost/headroom 记录（来自 T64 fit report）。
#[derive(Clone, Debug, PartialEq)]
pub struct RoleBoostRecord {
    /// 角色 id。
    pub role_id: String,
    /// 单段最大正向增益（dB；硬帽 +3）。
    pub single_boost_db: f64,
    /// 每通道正向总和（dB；>6 warn、>12 拒）。
    pub positive_sum_db: f64,
    /// 预测最大累计响应峰值（dBFS；必须 ≤−6）。
    pub predicted_peak_dbfs: f64,
    /// D7 headroom（dB；必须 ≥6）。
    pub headroom_db: f64,
    /// T64 自己的 limits_pass 结论。
    pub limits_pass: bool,
}

impl RoleBoostRecord {
    /// 构造 + 有限性校验。
    pub fn new(
        role_id: impl Into<String>,
        single_boost_db: f64,
        positive_sum_db: f64,
        predicted_peak_dbfs: f64,
        headroom_db: f64,
        limits_pass: bool,
    ) -> CaResult<Self> {
        let record = Self {
            role_id: role_id.into(),
            single_boost_db,
            positive_sum_db,
            predicted_peak_dbfs,
            headroom_db,
            limits_pass,
        };
        record.validate_fields()?;
        Ok(record)
    }

    fn validate_fields(&self) -> CaResult<()> {
        if !is_role_token(&self.role_id) {
            return Err(ca_err(
                E_CA_D7_PEAK,
                "headroom.roles[].role_id",
                format!("role id {:?} 不是 role token", self.role_id),
            ));
        }
        require_finite(self.single_boost_db, "headroom.single_boost_db")?;
        require_finite(self.positive_sum_db, "headroom.positive_sum_db")?;
        require_finite(self.predicted_peak_dbfs, "headroom.predicted_peak_dbfs")?;
        require_finite(self.headroom_db, "headroom.headroom_db")?;
        Ok(())
    }
}

/// D7 headroom 证据（T64 fit report 摘要；编译期硬门）。
///
/// **F-35（证据必须绑定进 plan 的系数）**：除申报数字外，本结构还绑定 T64 fit report 的
/// **原始字节**（[`Self::with_t64_report_bytes`]）：validate 强制校验
/// `sha256(bytes) == report_sha256`，并从该报告取 program peak 基准与 T64 producer 结论；
/// 编译期再用**真正下发的编码 P 行**重算峰值/headroom（见
/// [`verify_d7_from_encoded_p_rows`]）。禁止只交申报数字与 64-hex hash。
#[derive(Clone, Debug, PartialEq)]
pub struct HeadroomEvidence {
    /// T64 report schema（必须 = tspi-peq-fit/v1）。
    pub schema: String,
    /// T64 fit report SHA-256。
    pub report_sha256: String,
    /// 逐角色记录。
    pub roles: Vec<RoleBoostRecord>,
    /// 实测峰值（T3/T4 measured；可选，存在即需 ≤−1 dBFS）。
    pub measured_peak_dbfs: Option<f64>,
    /// T64 fit report **原始字节**（F-35：report_sha256 必须绑定这串字节；D7 重算的
    /// program peak 基准与 producer 结论也取自该报告，不再只信调用方申报数字）。
    report_bytes: Option<Vec<u8>>,
}

impl HeadroomEvidence {
    /// 构造（不校验；compile 时 validate）。
    pub fn new(
        schema: impl Into<String>,
        report_sha256: impl Into<String>,
        roles: Vec<RoleBoostRecord>,
    ) -> Self {
        Self {
            schema: schema.into(),
            report_sha256: report_sha256.into(),
            roles,
            measured_peak_dbfs: None,
            report_bytes: None,
        }
    }

    /// 带实测峰值（T3/T4 measured 证据）。
    pub fn with_measured_peak(mut self, measured_peak_dbfs: f64) -> Self {
        self.measured_peak_dbfs = Some(measured_peak_dbfs);
        self
    }

    /// 绑定 T64 fit report **原始字节**（F-35）。
    ///
    /// validate 强制校验 `sha256(bytes) == report_sha256`，并解析该报告取 D7 program peak
    /// 基准 / producer 结论；编译期 D7 重算（[`verify_d7_from_encoded_p_rows`]）也从它取基准。
    pub fn with_t64_report_bytes(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.report_bytes = Some(bytes.into());
        self
    }

    /// 已绑定的 T64 报告原始字节（未绑定时 None）。
    pub fn t64_report_bytes(&self) -> Option<&[u8]> {
        self.report_bytes.as_deref()
    }

    /// 单角色记录。
    pub fn role(&self, role_id: &str) -> Option<&RoleBoostRecord> {
        self.roles.iter().find(|record| record.role_id == role_id)
    }

    /// D7 校验：返回 warnings（≤6 dB 正向总和无警告；6..12 dB 一条 W_D7_POSITIVE_SUM_WARN）。
    ///
    /// 硬失败：单段 >+3 dB / 正向总和 >12 dB / 预测峰值 >−6 dBFS / headroom <6 dB /
    /// limits_pass=false / 缺角色记录 / 实测峰值（若有）>−1 dBFS。
    pub fn validate(&self, expected_role_ids: &[String]) -> CaResult<Vec<&'static str>> {
        if self.schema != PEQ_FIT_SCHEMA {
            return Err(ca_err(
                E_CA_EVIDENCE,
                "headroom.schema",
                format!("schema 必须为 {PEQ_FIT_SCHEMA}，得到 {:?}", self.schema),
            ));
        }
        require_sha256(
            &self.report_sha256,
            "headroom.report_sha256",
            E_CA_ARTIFACT_HASH,
        )?;
        // F-35：report_sha256 必须绑定 T64 报告**原始字节**；申报数字/报告 hash 不能各自独立。
        let report_bytes = match self.report_bytes.as_deref() {
            Some(bytes) => bytes,
            None => {
                return Err(ca_err(
                    E_CA_EVIDENCE,
                    "headroom.report_bytes",
                    "缺少 T64 报告原始字节：report_sha256 必须绑定实际 T64 报告（F-35），禁止只申报 hash",
                ))
            }
        };
        let actual_report_sha256 = sha256_hex(report_bytes);
        if actual_report_sha256 != self.report_sha256 {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "headroom.report_sha256",
                format!(
                    "report_sha256 与传入 T64 报告字节不符（申报 {}，实际 {}；证据未绑定报告/系数，F-35）",
                    self.report_sha256, actual_report_sha256
                ),
            ));
        }
        let report = parse_t64_fit_report(report_bytes)?;
        for role_id in expected_role_ids {
            if report.channel(role_id).is_none() {
                return Err(ca_err(
                    E_CA_EVIDENCE,
                    format!("headroom.roles[{role_id}]"),
                    "T64 报告缺该通道记录（F-35：申报记录必须来自绑定的报告字节）",
                ));
            }
        }
        if let Some(peak) = self.measured_peak_dbfs {
            require_finite(peak, "headroom.measured_peak_dbfs")?;
            if peak > T3_MEASURED_PEAK_LIMIT_DBFS {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    "headroom.measured_peak_dbfs",
                    format!(
                        "实测峰值 {peak} dBFS 超过 {} dBFS（ACCEPTANCE §0.5/§5 削波门）",
                        T3_MEASURED_PEAK_LIMIT_DBFS
                    ),
                ));
            }
        }
        let mut warnings = Vec::new();
        for role_id in expected_role_ids {
            let record = match self.role(role_id) {
                Some(record) => record,
                None => {
                    return Err(ca_err(
                        E_CA_D7_PEAK,
                        format!("headroom.roles[{role_id}]"),
                        "D7 证据缺少该角色的 boost/headroom 记录（缺证据=不可判定，不得 PASS）",
                    ))
                }
            };
            record.validate_fields()?;
            if record.single_boost_db > D7_SINGLE_BOOST_CAP_DB + 1e-9 {
                return Err(ca_err(
                    E_CA_D7_BOOST,
                    format!("headroom.roles[{role_id}].single_boost_db"),
                    format!(
                        "单段 boost {} dB 超过 +{} dB 硬帽（CONTRACT §3.3 D7，producer 必须钳制）",
                        record.single_boost_db, D7_SINGLE_BOOST_CAP_DB
                    ),
                ));
            }
            if record.positive_sum_db > D7_POSITIVE_SUM_REJECT_DB + 1e-9 {
                return Err(ca_err(
                    E_CA_D7_POSITIVE_SUM,
                    format!("headroom.roles[{role_id}].positive_sum_db"),
                    format!(
                        "每通道正向总和 {} dB 超过 {} dB 拒绝门（CONTRACT §3.3 D7）",
                        record.positive_sum_db, D7_POSITIVE_SUM_REJECT_DB
                    ),
                ));
            }
            if record.positive_sum_db > D7_POSITIVE_SUM_WARN_DB + 1e-9
                && !warnings.contains(&W_D7_POSITIVE_SUM_WARN)
            {
                warnings.push(W_D7_POSITIVE_SUM_WARN);
            }
            if record.predicted_peak_dbfs > D7_PREDICTED_PEAK_LIMIT_DBFS + 1e-9 {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    format!("headroom.roles[{role_id}].predicted_peak_dbfs"),
                    format!(
                        "预测累计峰值 {} dBFS 超过 {} dBFS（headroom 必须 ≥{} dB，D7）",
                        record.predicted_peak_dbfs,
                        D7_PREDICTED_PEAK_LIMIT_DBFS,
                        D7_MIN_HEADROOM_DB
                    ),
                ));
            }
            if record.headroom_db < D7_MIN_HEADROOM_DB - 1e-9 {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    format!("headroom.roles[{role_id}].headroom_db"),
                    format!(
                        "headroom {} dB < {} dB（D7 硬门）",
                        record.headroom_db, D7_MIN_HEADROOM_DB
                    ),
                ));
            }
            if !record.limits_pass {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    format!("headroom.roles[{role_id}].limits_pass"),
                    "T64 limits_pass=false（boost/headroom 自检未过）不得编译",
                ));
            }
        }
        Ok(warnings)
    }

    /// headroom canonical 文本（确定性）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str("tspi-cal-headroom-evidence/v1\n");
        let _ = writeln!(text, "schema={}", self.schema);
        let _ = writeln!(text, "report_sha256={}", self.report_sha256);
        match self.measured_peak_dbfs {
            Some(peak) => {
                let _ = writeln!(text, "measured_peak_dbfs={peak:.12}");
            }
            None => text.push_str("measured_peak_dbfs=none\n"),
        }
        for record in &self.roles {
            let _ = writeln!(
                text,
                "role {} single={:.12} sum={:.12} peak={:.12} headroom={:.12} limits_pass={}",
                record.role_id,
                record.single_boost_db,
                record.positive_sum_db,
                record.predicted_peak_dbfs,
                record.headroom_db,
                record.limits_pass as u8
            );
        }
        text
    }

    /// headroom hash（canonical 文本 SHA-256）。
    pub fn headroom_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ---------------------------------------------------------------------------
// crossover（T39 输出 FIR 分频）D7 证据（P_post 含 crossover 段时的绑定）
// ---------------------------------------------------------------------------

/// 单 role 的 crossover P_post 证据（卷积后 P 行预测 + 系数绑定）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverRoleEvidence {
    /// role id（必须与 plan role 表一致）。
    pub role_id: String,
    /// 物理输出槽位（= plan role.out_channel）。
    pub out_channel: u16,
    /// 支路（"lowpass" | "highpass"）。
    pub branch: String,
    /// crossover 段 taps（= T39 设计 taps）。
    pub segment_taps: usize,
    /// T64 目标 FIR 段 taps（0 = 无 target 段）。
    pub target_taps: usize,
    /// 卷积后 P FIR 长度（= role.p_fir.len()，绑定长度）。
    pub composed_taps: usize,
    /// 卷积后 P 行 1 kHz 相对链增益（dB；与 T64 同口径，仅作系数绑定/信息）。
    ///
    /// 注意：分频支路在 1 kHz 可能深度衰减（低通 @1 kHz 约 -80 dB），该值可达 +几十 dB；
    /// 它**不用于** D7 预测峰值（见 p_row_max_positive_db 绝对峰值口径）。
    pub p_row_gain_db: f64,
    /// 卷积后 P 行绝对幅度峰值（dB；|H|max，D7 预测峰值的增益上界）。
    ///
    /// crossover 为 cut-only（T39 单段幅度近似 <= +0.5 dB），故该值 <= 单段帽 +3 dB；
    /// D7 预测峰值 = T64 program peak 基准 + 本值 + role gain（分频行）。
    pub p_row_max_positive_db: f64,
    /// 卷积后 P 核 canonical sha256（人读证据；编译侧用增益/峰/长度做 fail-closed 绑定）。
    pub composed_coeff_sha256: String,
}

impl CrossoverRoleEvidence {
    /// 构造并校验字段自洽（branch/taps/长度关系/有限值/hash 形态）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role_id: impl Into<String>,
        out_channel: u16,
        branch: impl Into<String>,
        segment_taps: usize,
        target_taps: usize,
        composed_taps: usize,
        p_row_gain_db: f64,
        p_row_max_positive_db: f64,
        composed_coeff_sha256: impl Into<String>,
    ) -> CaResult<Self> {
        let record = Self {
            role_id: role_id.into(),
            out_channel,
            branch: branch.into(),
            segment_taps,
            target_taps,
            composed_taps,
            p_row_gain_db,
            p_row_max_positive_db,
            composed_coeff_sha256: composed_coeff_sha256.into(),
        };
        require_finite(
            record.p_row_gain_db,
            format!("crossover.roles[{}].p_row_gain_db", record.role_id),
        )?;
        require_finite(
            record.p_row_max_positive_db,
            format!("crossover.roles[{}].p_row_max_positive_db", record.role_id),
        )?;
        require_sha256(
            &record.composed_coeff_sha256,
            &format!("crossover.roles[{}].composed_coeff_sha256", record.role_id),
            E_CA_CROSSOVER,
        )?;
        if record.segment_taps == 0 {
            return Err(ca_err(
                E_CA_CROSSOVER,
                format!("crossover.roles[{}].segment_taps", record.role_id),
                "crossover 段 taps 不能为 0",
            ));
        }
        if record.composed_taps == 0 {
            return Err(ca_err(
                E_CA_CROSSOVER,
                format!("crossover.roles[{}].composed_taps", record.role_id),
                "卷积后 P FIR 长度不能为 0",
            ));
        }
        let expected = if record.target_taps == 0 {
            record.segment_taps
        } else {
            record.segment_taps + record.target_taps - 1
        };
        if record.composed_taps != expected {
            return Err(ca_err(
                E_CA_CROSSOVER,
                format!("crossover.roles[{}].composed_taps", record.role_id),
                format!(
                    "卷积后长度 {} != segment_taps {} + target_taps {} - 1（段组合长度不自洽）",
                    record.composed_taps, record.segment_taps, record.target_taps
                ),
            ));
        }
        Ok(record)
    }
}

/// crossover 接线 + D7 证据（T39 NormativeReport + 合并平坦度 + 每 role 卷积后预测）。
///
/// **D7 绑定口径（替代 T64 报告链增益）**：P_post 含分频段后，T64 报告隐含的 P 链增益只描述
/// 目标曲线 FIR；本证据给出**卷积后** P 行的预测增益/正峰，编译期用真正下发的 f32 编码 P 行
/// 重算并与本证据交叉绑定（差 > D7_P_ROW_BIND_TOL_DB → E_CA_D7_PEAK fail-closed，
/// 不沿用 W_EXPERIMENT_P_BINDING_SKIPPED 的实验跳过）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverEvidence {
    /// schema（固定 CROSSOVER_EVIDENCE_SCHEMA）。
    pub schema: String,
    /// T39 类型（LR4 等）。
    pub kind: String,
    /// T39 设计方法。
    pub method: String,
    /// 运行采样率。
    pub fs_hz: u32,
    /// 交叉点 fc（Hz）。
    pub fc_hz: f64,
    /// 每支 taps（T39 设计支撑）。
    pub taps: usize,
    /// 容器通道数（输出赋值域）。
    pub output_count: u16,
    /// T39 设计报告 canonical sha256（判据/合并/延迟/系数摘要绑定）。
    pub design_report_sha256: String,
    /// T39 判据是否全过（INFEASIBLE 必须为 false 并拒收，不得进 plan）。
    pub normative_pass: bool,
    /// 失败判据 id（证据）。
    pub failed_criteria: Vec<String>,
    /// 合并支路 |S| 带内最大偏差（dB）。
    pub merge_flat_deviation_db: f64,
    /// 合并平坦判据上限（dB）。
    pub merge_flat_limit_db: f64,
    /// 两支相对声明延迟（samples；必须 0）。
    pub relative_delay_samples: f64,
    /// LP 支 max|h|。
    pub lowpass_max_abs_h: f64,
    /// HP 支 max|h|。
    pub highpass_max_abs_h: f64,
    /// LP 支 Σ|h|。
    pub lowpass_sum_abs_h: f64,
    /// HP 支 Σ|h|。
    pub highpass_sum_abs_h: f64,
    /// 每 role 卷积后 P 行证据。
    pub roles: Vec<CrossoverRoleEvidence>,
}

impl CrossoverEvidence {
    /// 构造（字段显式；roles 必须非空）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: impl Into<String>,
        method: impl Into<String>,
        fs_hz: u32,
        fc_hz: f64,
        taps: usize,
        output_count: u16,
        design_report_sha256: impl Into<String>,
        normative_pass: bool,
        failed_criteria: Vec<String>,
        merge_flat_deviation_db: f64,
        merge_flat_limit_db: f64,
        relative_delay_samples: f64,
        lowpass_max_abs_h: f64,
        highpass_max_abs_h: f64,
        lowpass_sum_abs_h: f64,
        highpass_sum_abs_h: f64,
        roles: Vec<CrossoverRoleEvidence>,
    ) -> CaResult<Self> {
        Ok(Self {
            schema: CROSSOVER_EVIDENCE_SCHEMA.to_string(),
            kind: kind.into(),
            method: method.into(),
            fs_hz,
            fc_hz,
            taps,
            output_count,
            design_report_sha256: design_report_sha256.into(),
            normative_pass,
            failed_criteria,
            merge_flat_deviation_db,
            merge_flat_limit_db,
            relative_delay_samples,
            lowpass_max_abs_h,
            highpass_max_abs_h,
            lowpass_sum_abs_h,
            highpass_sum_abs_h,
            roles,
        })
    }

    /// 按 role_id 取记录。
    pub fn role(&self, role_id: &str) -> Option<&CrossoverRoleEvidence> {
        self.roles.iter().find(|record| record.role_id == role_id)
    }

    /// 证据 canonical 文本（确定性；报告 hash 绑定）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str("tspi-cal-crossover-evidence/v1\n");
        let _ = writeln!(text, "schema={}", self.schema);
        let _ = writeln!(
            text,
            "kind={} method={} fs={} fc={:.17e} taps={} output_count={}",
            self.kind, self.method, self.fs_hz, self.fc_hz, self.taps, self.output_count
        );
        let _ = writeln!(
            text,
            "design_report_sha256={} normative_pass={} failed={:?}",
            self.design_report_sha256, self.normative_pass, self.failed_criteria
        );
        let _ = writeln!(
            text,
            "merge_flat_dev={:.17e} merge_flat_limit={:.17e} relative_delay={:.17e} lp_max={:.17e} hp_max={:.17e} lp_sum={:.17e} hp_sum={:.17e}",
            self.merge_flat_deviation_db,
            self.merge_flat_limit_db,
            self.relative_delay_samples,
            self.lowpass_max_abs_h,
            self.highpass_max_abs_h,
            self.lowpass_sum_abs_h,
            self.highpass_sum_abs_h
        );
        for record in &self.roles {
            let _ = writeln!(
                text,
                "role {} dac={} branch={} segment_taps={} target_taps={} composed_taps={} gain={:.17e} peak={:.17e} coeff_sha256={}",
                record.role_id,
                record.out_channel,
                record.branch,
                record.segment_taps,
                record.target_taps,
                record.composed_taps,
                record.p_row_gain_db,
                record.p_row_max_positive_db,
                record.composed_coeff_sha256
            );
        }
        text
    }

    /// 证据 canonical sha256。
    pub fn evidence_sha256(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }

    /// 请求级校验：证据必须与 role 表/段来源/长度/声明延迟一致（fail-closed）。
    pub fn validate(&self, req: &CompileApplyRequest<'_>) -> CaResult<()> {
        if self.schema != CROSSOVER_EVIDENCE_SCHEMA {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.schema",
                format!(
                    "schema 必须为 {CROSSOVER_EVIDENCE_SCHEMA}，得到 {:?}",
                    self.schema
                ),
            ));
        }
        if self.fs_hz != req.sample_rate_hz {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.fs_hz",
                format!("证据 fs={} != 请求 fs={}", self.fs_hz, req.sample_rate_hz),
            ));
        }
        if !self.fc_hz.is_finite() {
            return Err(ca_err(E_CA_CROSSOVER, "crossover.fc_hz", "fc 非有限"));
        }
        if self.taps == 0 || self.taps > TAPS_NORMAL_MAX as usize {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.taps",
                format!(
                    "taps={} 越界（1..={TAPS_NORMAL_MAX}；extended taps 不在 crossover 接线内）",
                    self.taps
                ),
            ));
        }
        if self.output_count != req.container_channels {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.output_count",
                format!(
                    "证据 output_count={} != 请求 container_channels={}",
                    self.output_count, req.container_channels
                ),
            ));
        }
        if self.kind.trim().is_empty() || self.method.trim().is_empty() {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.kind",
                "kind/method 不能为空",
            ));
        }
        require_sha256(
            &self.design_report_sha256,
            "crossover.design_report_sha256",
            E_CA_CROSSOVER,
        )?;
        if !self.normative_pass || !self.failed_criteria.is_empty() {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.normative_pass",
                format!(
                    "T39 判据未全过（failed={:?}）：INFEASIBLE 设计不得进 plan",
                    self.failed_criteria
                ),
            ));
        }
        if !self.merge_flat_deviation_db.is_finite()
            || !self.merge_flat_limit_db.is_finite()
            || self.merge_flat_limit_db <= 0.0
            || self.merge_flat_deviation_db > self.merge_flat_limit_db + 1e-9
        {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.merge_flat",
                format!(
                    "合并平坦度非法：dev={} limit={}（必须 dev<=limit>0）",
                    self.merge_flat_deviation_db, self.merge_flat_limit_db
                ),
            ));
        }
        if !self.relative_delay_samples.is_finite() {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.relative_delay_samples",
                "相对延迟非有限",
            ));
        }
        if self.relative_delay_samples.abs() > 1e-9 {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.relative_delay_samples",
                format!(
                    "两支相对声明延迟 {} samples 非 0（T39 同 taps 同 D 契约）",
                    self.relative_delay_samples
                ),
            ));
        }
        for (field, value) in [
            ("lowpass_max_abs_h", self.lowpass_max_abs_h),
            ("highpass_max_abs_h", self.highpass_max_abs_h),
            ("lowpass_sum_abs_h", self.lowpass_sum_abs_h),
            ("highpass_sum_abs_h", self.highpass_sum_abs_h),
        ] {
            require_finite(value, format!("crossover.{field}"))?;
            if value < 0.0 {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.{field}"),
                    "支路幅度统计不能为负",
                ));
            }
        }
        if self.roles.is_empty() {
            return Err(ca_err(
                E_CA_CROSSOVER,
                "crossover.roles",
                "crossover 证据 roles 不能为空（配置了分频就必须有 role 绑定）",
            ));
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.roles.len());
        for record in &self.roles {
            if seen.contains(&record.role_id.as_str()) {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}]", record.role_id),
                    "role 记录重复",
                ));
            }
            seen.push(record.role_id.as_str());
            let role = req
                .roles
                .iter()
                .find(|role| role.role_id == record.role_id)
                .ok_or_else(|| {
                    ca_err(
                        E_CA_CROSSOVER,
                        format!("crossover.roles[{}]", record.role_id),
                        "证据 role 不在请求 role 表中",
                    )
                })?;
            if !role
                .fir_segments
                .contains(&tspi_dsp_core::nodes::FirSegmentKind::Crossover)
            {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}]", record.role_id),
                    "证据含 crossover 但 role.fir_segments 无 Crossover 段（flag/证据不一致）",
                ));
            }
            if role.out_channel != record.out_channel {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].out_channel", record.role_id),
                    format!(
                        "证据 out_channel={} != role.out_channel={}",
                        record.out_channel, role.out_channel
                    ),
                ));
            }
            if record.branch != "lowpass" && record.branch != "highpass" {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].branch", record.role_id),
                    format!("branch={:?} 非法（lowpass|highpass）", record.branch),
                ));
            }
            let has_target = role
                .fir_segments
                .contains(&tspi_dsp_core::nodes::FirSegmentKind::Target);
            if role.fir_segments.first() != Some(&tspi_dsp_core::nodes::FirSegmentKind::Crossover) {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].fir_segments", record.role_id),
                    "Crossover 段必须是 P_post 段序第一段（core 规范序）",
                ));
            }
            if has_target != (record.target_taps > 0) {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].target_taps", record.role_id),
                    format!(
                        "target_taps={} 与 fir_segments 的 Target 段存在性不一致",
                        record.target_taps
                    ),
                ));
            }
            if record.segment_taps != self.taps {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].segment_taps", record.role_id),
                    format!(
                        "证据 segment_taps={} != 设计 taps={}",
                        record.segment_taps, self.taps
                    ),
                ));
            }
            if record.composed_taps != role.p_fir.len() {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}].composed_taps", record.role_id),
                    format!(
                        "证据 composed_taps={} != role.p_fir.len()={}（系数被替换/截断，fail-closed）",
                        record.composed_taps,
                        role.p_fir.len()
                    ),
                ));
            }
        }
        for role in req.roles {
            if role
                .fir_segments
                .contains(&tspi_dsp_core::nodes::FirSegmentKind::Crossover)
                && self.role(&role.role_id).is_none()
            {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    format!("crossover.roles[{}]", role.role_id),
                    "role 含 crossover 段但证据缺该 role（D7 绑定缺失，不得编译）",
                ));
            }
        }
        Ok(())
    }
}
// ---------------------------------------------------------------------------
// T64 报告字节绑定 + 编码后 P 行 D7 重算（F-35）
// ---------------------------------------------------------------------------

/// T64 报告 `bands[]` 的一条 PEQ 段（归一化 biquad 系数；用于把 plan IIR 段绑回报告字节）。
#[derive(Clone, Debug)]
struct T64Band {
    b: [f64; 3],
    a: [f64; 3],
}

/// T64 fit report 里与 D7 重算相关的逐通道子集（只读消费 `tspi-peq-fit/v1`）。
#[derive(Clone, Debug)]
struct T64ChannelHeadroom {
    name: String,
    band_count: usize,
    /// 报告 bands[]（b/a；缺字段 = 旧夹具无 PEQ；band_count>0 时 D7 会要求可绑定）。
    bands: Vec<T64Band>,
    program_peak_reference_dbfs: f64,
    predicted_peak_before_trim_dbfs: f64,
    trim_db: f64,
    predicted_peak_dbfs: f64,
    headroom_db: f64,
    headroom_pass: bool,
    boost_limits_pass: bool,
    single_filter_max_db: f64,
    filter_positive_sum_db: f64,
    target_fir_max_positive_db: f64,
}

/// T64 fit report 的 D7 视图（F-35：报告字节 sha256 绑定后解析）。
#[derive(Clone, Debug)]
struct T64FitReportView {
    channels: Vec<T64ChannelHeadroom>,
}

impl T64FitReportView {
    fn channel(&self, name: &str) -> Option<&T64ChannelHeadroom> {
        self.channels.iter().find(|channel| channel.name == name)
    }
}

/// 报告对象字段（缺字段 = 证据不可判定）。
fn report_field<'a>(value: &'a JsonValue, field: &str, path: &str) -> CaResult<&'a JsonValue> {
    value.get(field).ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}"),
            format!("T64 报告缺字段 {field}（F-35：报告字节必须可解析）"),
        )
    })
}

fn report_f64(value: &JsonValue, field: &str, path: &str) -> CaResult<f64> {
    let number = report_field(value, field, path)?.as_f64().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 不是数字"),
        )
    })?;
    if !number.is_finite() {
        return Err(ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 非有限"),
        ));
    }
    Ok(number)
}

fn report_bool(value: &JsonValue, field: &str, path: &str) -> CaResult<bool> {
    report_field(value, field, path)?.as_bool().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 不是 bool"),
        )
    })
}

fn report_u64(value: &JsonValue, field: &str, path: &str) -> CaResult<u64> {
    report_field(value, field, path)?.as_u64().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 不是非负整数"),
        )
    })
}

fn report_str<'a>(value: &'a JsonValue, field: &str, path: &str) -> CaResult<&'a str> {
    report_field(value, field, path)?.as_str().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 不是字符串"),
        )
    })
}

/// 报告 3 元 f64 数组（b/a biquad 系数；长度/有限性硬校验）。
fn report_f64_array3(value: &JsonValue, field: &str, path: &str) -> CaResult<[f64; 3]> {
    let array = report_field(value, field, path)?
        .as_array()
        .ok_or_else(|| {
            ca_err(
                E_CA_EVIDENCE,
                format!("headroom.report_bytes.{path}.{field}"),
                format!("T64 报告字段 {field} 不是数组"),
            )
        })?;
    if array.len() != 3 {
        return Err(ca_err(
            E_CA_EVIDENCE,
            format!("headroom.report_bytes.{path}.{field}"),
            format!("T64 报告字段 {field} 必须是 3 元数组（biquad 系数）"),
        ));
    }
    let mut out = [0.0_f64; 3];
    for (index, item) in array.iter().enumerate() {
        let number = item.as_f64().ok_or_else(|| {
            ca_err(
                E_CA_EVIDENCE,
                format!("headroom.report_bytes.{path}.{field}[{index}]"),
                "T64 报告 biquad 系数不是数字",
            )
        })?;
        if !number.is_finite() {
            return Err(ca_err(
                E_CA_EVIDENCE,
                format!("headroom.report_bytes.{path}.{field}[{index}]"),
                "T64 报告 biquad 系数非有限",
            ));
        }
        out[index] = number;
    }
    Ok(out)
}

/// 解析 T64 fit report 字节（`tspi-peq-fit/v1`；F-35：sha256 已绑定后的原始字节）。
fn parse_t64_fit_report(bytes: &[u8]) -> CaResult<T64FitReportView> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes",
            format!("T64 报告不是 UTF-8：{error}"),
        )
    })?;
    let document = json_parse(text).map_err(|error| {
        ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes",
            format!("T64 报告 JSON 解析失败：{error}（F-35）"),
        )
    })?;
    let schema = report_str(&document, "schema", "root")?;
    if schema != PEQ_FIT_SCHEMA {
        return Err(ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes.schema",
            format!("T64 报告 schema 必须为 {PEQ_FIT_SCHEMA}，得到 {schema:?}"),
        ));
    }
    let channels_value = report_field(&document, "channels", "root")?;
    let channels = channels_value.as_array().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes.channels",
            "T64 报告 channels 不是数组",
        )
    })?;
    if channels.is_empty() {
        return Err(ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes.channels",
            "T64 报告 channels 为空",
        ));
    }
    let mut out = Vec::with_capacity(channels.len());
    for (index, channel) in channels.iter().enumerate() {
        let path = format!("channels[{index}]");
        let name = report_str(channel, "name", &path)?.to_string();
        let band_count = report_u64(channel, "band_count", &path)? as usize;
        // bands[]：T64 报告的每段 PEQ b/a（plan ABI v2 IIR 段的字节绑定基准；缺字段 = 无）。
        let bands = match channel.get("bands") {
            Some(value) => {
                let array = value.as_array().ok_or_else(|| {
                    ca_err(
                        E_CA_EVIDENCE,
                        format!("headroom.report_bytes.{path}.bands"),
                        "T64 报告 bands 不是数组",
                    )
                })?;
                let mut bands = Vec::with_capacity(array.len());
                for (band_index, band) in array.iter().enumerate() {
                    let band_path = format!("{path}.bands[{band_index}]");
                    let b = report_f64_array3(band, "b", &band_path)?;
                    let a = report_f64_array3(band, "a", &band_path)?;
                    if (a[0] - 1.0).abs() > 1e-9 {
                        return Err(ca_err(
                            E_CA_EVIDENCE,
                            format!("headroom.report_bytes.{band_path}.a[0]"),
                            "T64 报告 PEQ 段 a0 必须归一化为 1（plan 只编码 b0,b1,b2,a1,a2）",
                        ));
                    }
                    bands.push(T64Band { b, a });
                }
                bands
            }
            None => Vec::new(),
        };
        let headroom = report_field(channel, "headroom", &path)?;
        let boost = report_field(channel, "boost", &path)?;
        let target_fir = report_field(channel, "target_fir", &path)?;
        let headroom_path = format!("{path}.headroom");
        let boost_path = format!("{path}.boost");
        let target_fir_path = format!("{path}.target_fir");
        out.push(T64ChannelHeadroom {
            name,
            band_count,
            bands,
            program_peak_reference_dbfs: report_f64(
                headroom,
                "program_peak_reference_dbfs",
                &headroom_path,
            )?,
            predicted_peak_before_trim_dbfs: report_f64(
                headroom,
                "predicted_peak_before_trim_dbfs",
                &headroom_path,
            )?,
            trim_db: report_f64(headroom, "trim_db", &headroom_path)?,
            predicted_peak_dbfs: report_f64(headroom, "predicted_peak_dbfs", &headroom_path)?,
            headroom_db: report_f64(headroom, "headroom_db", &headroom_path)?,
            headroom_pass: report_bool(headroom, "pass", &headroom_path)?,
            boost_limits_pass: report_bool(boost, "limits_pass", &boost_path)?,
            single_filter_max_db: report_f64(boost, "single_filter_max_db", &boost_path)?,
            filter_positive_sum_db: report_f64(boost, "filter_positive_sum_db", &boost_path)?,
            target_fir_max_positive_db: report_f64(
                target_fir,
                "max_positive_db",
                &target_fir_path,
            )?,
        });
    }
    Ok(T64FitReportView { channels: out })
}

/// 单点 FIR 幅度（dB；直接 DFT，与 T64 `peq_fit::fir_magnitude_db_at` 逐式同口径）。
///
/// 镜像原因：peq_fit.rs 的该函数是模块私有（`fn`），且 T64 文件归 cal-capture/rt-calibfix
/// 写域，本组只读；为保证「用 P 行重算峰值」与 T64 同一口径，这里逐式镜像，T64 算法变更必须同步。
fn fir_magnitude_db_at(coeffs: &[f64], fs: f64, freq_hz: f64) -> f64 {
    if coeffs.is_empty() {
        return 0.0;
    }
    let omega = 2.0 * std::f64::consts::PI * freq_hz / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    for (index, coeff) in coeffs.iter().enumerate() {
        let phase = omega * index as f64;
        re += coeff * phase.cos();
        im -= coeff * phase.sin();
    }
    20.0 * re.hypot(im).max(f64::MIN_POSITIVE).log10()
}

/// log-f 网格插值（端点 hold；与 T64 `peq_fit::sample_curve_db` 逐式同口径）。
fn sample_curve_db(grid: &[f64], values: &[f64], freq_hz: f64) -> f64 {
    if grid.is_empty() || values.len() != grid.len() {
        return 0.0;
    }
    if freq_hz <= grid[0] {
        return values[0];
    }
    let last = grid.len() - 1;
    if freq_hz >= grid[last] {
        return values[last];
    }
    let mut low = 0usize;
    let mut high = last;
    while high - low > 1 {
        let mid = (low + high) / 2;
        if grid[mid] <= freq_hz {
            low = mid;
        } else {
            high = mid;
        }
    }
    let f0 = grid[low].max(f64::MIN_POSITIVE);
    let f1 = grid[high].max(f64::MIN_POSITIVE);
    let ratio = (freq_hz / f0).ln() / (f1 / f0).ln();
    let t = ratio.clamp(0.0, 1.0);
    values[low] + t * (values[high] - values[low])
}

/// 与 T64 peq_fit 幅度链同口径的峰值增益（dB）：
/// `headroom_grid`（20 Hz..min(20k,fs/2−1)，1/24 oct）+ `chain_peak_gain_db` 的 P-only 形态
/// （本模块要求 PEQ 段为 0，链 = 目标 FIR；参考点 1 kHz 与 T64 一致）。
pub(crate) fn p_row_chain_peak_gain_db(coeffs: &[f64], fs: f64) -> f64 {
    let grid = log_frequency_grid(FIT_FREQ_MIN_HZ, freq_upper_hz(fs), REPORT_GRID_OCTAVE);
    if grid.is_empty() {
        return 0.0;
    }
    let response: Vec<f64> = grid
        .iter()
        .map(|freq| fir_magnitude_db_at(coeffs, fs, *freq))
        .collect();
    let reference = sample_curve_db(&grid, &response, REFERENCE_FREQ_HZ);
    response
        .iter()
        .map(|value| value - reference)
        .fold(f64::NEG_INFINITY, f64::max)
}

/// 编码后 P 行的幅度正峰（dB）。FIR-only plan 下，D7「单段 boost 帽」的等价口径是
/// 实际下发 P 行的幅度响应正峰不得超过 +3 dB（T64 target FIR producer cap 同一数值）。
pub(crate) fn p_row_max_positive_db(coeffs: &[f64], fs: f64) -> f64 {
    let grid = log_frequency_grid(FIT_FREQ_MIN_HZ, freq_upper_hz(fs), REPORT_GRID_OCTAVE);
    grid.iter()
        .map(|freq| fir_magnitude_db_at(coeffs, fs, *freq))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// 已编码 role gain（gain_q15 量化后的实际线性增益，dB）；0 = 静音。
fn encoded_role_gain_db(role: &RoleCompileInput) -> f64 {
    let quantized = role.gain_q15();
    if quantized <= 0 {
        return f64::NEG_INFINITY;
    }
    20.0 * (quantized as f64 / 32767.0).log10()
}

/// F-35：D7 硬门不再只信 `HeadroomEvidence` 申报数字，而是：
/// 1) 强制 `report_sha256 == sha256(T64 报告字节)`（在 [`HeadroomEvidence::validate`]）；
/// 2) 从绑定的 T64 报告取 program peak 基准 / producer 结论；
/// 3) 用**真正下发（编码后 + f32 量化）的 P 行（+ plan ABI v2 role IIR 级联）**按 peq_fit
///    同口径重算幅度链峰值/headroom，并与报告隐含 chain gain 交叉绑定：系数被替换/更热 = 拒收；
/// 4) plan IIR 段的段数与逐段 b/a 必须与绑定的 T64 报告 bands[] 一致（F-37/F-38）。
fn verify_d7_from_encoded_p_rows(
    req: &CompileApplyRequest<'_>,
    p_rows: &[f64],
) -> CaResult<Vec<&'static str>> {
    let role_ids: Vec<String> = req.roles.iter().map(|role| role.role_id.clone()).collect();
    let mut warnings = req.headroom.validate(&role_ids)?;
    let report_bytes = req.headroom.t64_report_bytes().ok_or_else(|| {
        ca_err(
            E_CA_EVIDENCE,
            "headroom.report_bytes",
            "缺少 T64 报告原始字节（F-35：D7 重算需要报告基准）",
        )
    })?;
    let report = parse_t64_fit_report(report_bytes)?;
    let fs = req.sample_rate_hz as f64;
    let l_p = req.l_p as usize;
    for role in req.roles {
        let channel = report.channel(&role.role_id).ok_or_else(|| {
            ca_err(
                E_CA_EVIDENCE,
                format!("headroom.roles[{}]", role.role_id),
                "T64 报告缺该 role 的 channel 记录（F-35：D7 证据必须绑定报告与系数）",
            )
        })?;
        // F-37/F-38：plan ABI v2（GAP-T66-3）下 T64 报告的 IIR 段必须作为 per-role IIR 表
        // 进 plan；否则仅部署残差 FIR 不能代表完整 P（H_eff≠Hraw·P），禁止静默丢弃。
        // 1) plan 带 IIR → 段数/逐段 b/a 必须与绑定 T64 报告字节一致（字节级绑定，F-35）；
        // 2) plan 不带 IIR 且报告有 IIR → 生产拒收；实验旁路显式记 W_EXPERIMENT_IIR_EXCLUDED。
        let planned_sections = &role.peq_sections;
        if !planned_sections.is_empty() {
            if channel.band_count != planned_sections.len() {
                return Err(ca_err(
                    E_CA_PEQ_ABI,
                    format!("roles[{}].peq_sections", role.role_index),
                    format!(
                        "plan IIR 段数 {} != T64 报告 band_count={}（role/报告绑定不一致，F-38）",
                        planned_sections.len(),
                        channel.band_count
                    ),
                ));
            }
            if channel.bands.len() != planned_sections.len() {
                return Err(ca_err(
                    E_CA_EVIDENCE,
                    format!("headroom.report_bytes.channels[{}].bands", role.role_id),
                    format!(
                        "T64 报告 bands 数组 {} 条 != band_count={}（plan IIR 系数不可绑定，F-35）",
                        channel.bands.len(),
                        channel.band_count
                    ),
                ));
            }
            let section_peak_limit_db = D7_SINGLE_BOOST_CAP_DB + PEQ_SECTION_PEAK_TOL_DB;
            let section_grid =
                log_frequency_grid(FIT_FREQ_MIN_HZ, freq_upper_hz(fs), REPORT_GRID_OCTAVE);
            for (index, (section, band)) in
                planned_sections.iter().zip(channel.bands.iter()).enumerate()
            {
                let encoded = peq_section_coeffs_f32(section);
                let expected = [band.b[0], band.b[1], band.b[2], band.a[1], band.a[2]];
                for k in 0..PLAN_IIR_SECTION_COEFFS {
                    if (encoded[k] as f64 - expected[k]).abs() > PEQ_PLAN_COEFF_BIND_TOL {
                        return Err(ca_err(
                            E_CA_PEQ_ABI,
                            format!("roles[{}].peq_sections[{index}]", role.role_index),
                            format!(
                                "IIR 段系数[{k}]={} 与 T64 报告 b/a 值 {} 不一致（f32 量化容差 {PEQ_PLAN_COEFF_BIND_TOL}；系数被替换/F-35）",
                                encoded[k], expected[k]
                            ),
                        ));
                    }
                }
                let section_peak = section_grid
                    .iter()
                    .map(|freq| encoded_section_response_db(&encoded, fs, *freq))
                    .fold(f64::NEG_INFINITY, f64::max);
                if section_peak > section_peak_limit_db {
                    return Err(ca_err(
                        E_CA_D7_BOOST,
                        format!("roles[{}].peq_sections[{index}]", role.role_index),
                        format!(
                            "进 plan 的 PEQ 段幅度正峰 {section_peak:.3} dB 超过 +{D7_SINGLE_BOOST_CAP_DB} dB 单段帽（f32 编码系数硬检，F-35）"
                        ),
                    ));
                }
            }
        } else if channel.band_count != 0 {
            if !req.experiment.bypasses_production_gates() {
                return Err(ca_err(
                    E_CA_PEQ_ABI,
                    format!("headroom.roles[{}]", role.role_id),
                    format!(
                        "T64 报告该通道 band_count={}（IIR 段）但 plan 未承载 IIR 表（缺 with_peq_sections）：残差 FIR 不能单独代表 P（H_eff≠Hraw·P，F-37），拒收",
                        channel.band_count
                    ),
                ));
            }
            if !warnings.contains(&W_EXPERIMENT_IIR_EXCLUDED) {
                warnings.push(W_EXPERIMENT_IIR_EXCLUDED);
            }
        }
        // 报告数值自洽（防止只拼少数合格字段的伪报告字节；F-35 最小绑定）。
        if (channel.predicted_peak_dbfs
            - (channel.predicted_peak_before_trim_dbfs + channel.trim_db))
            .abs()
            > 1e-6
            || (channel.headroom_db + channel.predicted_peak_dbfs).abs() > 1e-6
        {
            return Err(ca_err(
                E_CA_EVIDENCE,
                format!("headroom.report_bytes.channels[{}].headroom", role.role_id),
                "T64 报告 headroom 数值自相矛盾（predicted/trim/headroom 不一致，F-35）",
            ));
        }
        if !channel.boost_limits_pass || !channel.headroom_pass {
            return Err(ca_err(
                E_CA_D7_PEAK,
                format!("headroom.roles[{}]", role.role_id),
                "T64 报告 producer 自检未过（boost.limits_pass / headroom.pass = false），不得编译",
            ));
        }
        if channel.single_filter_max_db > D7_SINGLE_BOOST_CAP_DB + 1e-9 {
            return Err(ca_err(
                E_CA_D7_BOOST,
                format!("headroom.roles[{}]", role.role_id),
                format!(
                    "T64 报告单段 boost {} dB 超过 +{} dB 硬帽",
                    channel.single_filter_max_db, D7_SINGLE_BOOST_CAP_DB
                ),
            ));
        }
        if channel.filter_positive_sum_db > D7_POSITIVE_SUM_REJECT_DB + 1e-9 {
            return Err(ca_err(
                E_CA_D7_POSITIVE_SUM,
                format!("headroom.roles[{}]", role.role_id),
                format!(
                    "T64 报告每通道正向总和 {} dB 超过 {} dB 拒绝门",
                    channel.filter_positive_sum_db, D7_POSITIVE_SUM_REJECT_DB
                ),
            ));
        }
        if channel.filter_positive_sum_db > D7_POSITIVE_SUM_WARN_DB + 1e-9
            && !warnings.contains(&W_D7_POSITIVE_SUM_WARN)
        {
            warnings.push(W_D7_POSITIVE_SUM_WARN);
        }
        if channel.target_fir_max_positive_db > D7_SINGLE_BOOST_CAP_DB + 1e-9 {
            return Err(ca_err(
                E_CA_D7_BOOST,
                format!(
                    "headroom.report_bytes.channels[{}].target_fir",
                    role.role_id
                ),
                format!(
                    "T64 报告 target_fir.max_positive_db {} dB 超过 +{} dB（producer 帽）",
                    channel.target_fir_max_positive_db, D7_SINGLE_BOOST_CAP_DB
                ),
            ));
        }
        // 真正下发的 P 行：f32 量化后的编码系数（尾零不改变幅度响应）。
        let offset = role.role_index as usize * l_p;
        let encoded: Vec<f64> = p_rows[offset..offset + l_p]
            .iter()
            .map(|value| *value as f32 as f64)
            .collect();
        let encoded_peak_positive_db = p_row_max_positive_db(&encoded, fs);
        if encoded_peak_positive_db > D7_SINGLE_BOOST_CAP_DB + 1e-9 {
            return Err(ca_err(
                E_CA_D7_BOOST,
                format!("roles[{}].p_fir", role.role_index),
                format!(
                    "进 plan 的编码 P 行幅度正峰 {encoded_peak_positive_db:.3} dB > +{} dB（单段帽：系数本身硬拒，F-35）",
                    D7_SINGLE_BOOST_CAP_DB
                ),
            ));
        }
        let encoded_gain_db = p_row_chain_peak_gain_db(&encoded, fs);
        // plan ABI v2：有 T64 PEQ 段时，编码链路 = 编码 P 行 ⊛ 编码 IIR 级联（完整 P）。
        // 无 PEQ 时保持旧 FIR-only 计算（bit-exact，历史报告不变）。
        let (combined_gain_db, combined_peak_positive_db) = if planned_sections.is_empty() {
            (encoded_gain_db, encoded_peak_positive_db)
        } else {
            let combined_grid =
                log_frequency_grid(FIT_FREQ_MIN_HZ, freq_upper_hz(fs), REPORT_GRID_OCTAVE);
            let mut response = Vec::with_capacity(combined_grid.len());
            let mut absolute_peak = f64::NEG_INFINITY;
            for freq in &combined_grid {
                let mut value = fir_magnitude_db_at(&encoded, fs, *freq);
                for section in planned_sections {
                    value += encoded_section_response_db(&peq_section_coeffs_f32(section), fs, *freq);
                }
                absolute_peak = absolute_peak.max(value);
                response.push(value);
            }
            let reference = sample_curve_db(&combined_grid, &response, REFERENCE_FREQ_HZ);
            let gain = response
                .iter()
                .map(|value| value - reference)
                .fold(f64::NEG_INFINITY, f64::max);
            (gain, absolute_peak.max(0.0))
        };
        // crossover（T39 分频）段：P_post 已含分频卷积，T64 报告隐含链增益只描述目标曲线 FIR，
        // 不能直接用作绑定基准；改为与 CrossoverEvidence 的**卷积后预测**绑定（D7 口径）。
        // 这是 fail-closed 硬绑定：证据/系数任一侧被替换都会拒收，不沿用实验跳过 warning。
        if let Some(crossover_record) = req
            .crossover
            .and_then(|evidence| evidence.role(&role.role_id))
        {
            if (encoded_gain_db - crossover_record.p_row_gain_db).abs() > D7_P_ROW_BIND_TOL_DB {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    format!("roles[{}].p_fir", role.role_index),
                    format!(
                        "crossover 编码 P 行幅度链峰值增益 {encoded_gain_db:.4} dB 与证据卷积后预测 {:.4} dB 不一致（D7 绑定；差 > {} dB，系数被替换/更热）",
                        crossover_record.p_row_gain_db, D7_P_ROW_BIND_TOL_DB
                    ),
                ));
            }
            if (encoded_peak_positive_db - crossover_record.p_row_max_positive_db).abs()
                > D7_P_ROW_BIND_TOL_DB
            {
                return Err(ca_err(
                    E_CA_D7_PEAK,
                    format!("roles[{}].p_fir", role.role_index),
                    format!(
                        "crossover 编码 P 行幅度正峰 {encoded_peak_positive_db:.4} dB 与证据预测 {:.4} dB 不一致（D7 绑定）",
                        crossover_record.p_row_max_positive_db
                    ),
                ));
            }
        } else {
            let report_gain_db =
                channel.predicted_peak_before_trim_dbfs - channel.program_peak_reference_dbfs;
            let plan_gain_db = if planned_sections.is_empty() {
                encoded_gain_db
            } else {
                combined_gain_db
            };
            if (plan_gain_db - report_gain_db).abs() > D7_P_ROW_BIND_TOL_DB {
                if !req.experiment.bypasses_production_gates() {
                    return Err(ca_err(
                        E_CA_D7_PEAK,
                        format!("roles[{}].p_fir", role.role_index),
                        format!(
                            "进 plan 的编码 P 行幅度链峰值增益 {encoded_gain_db:.4} dB 与 T64 报告隐含 {report_gain_db:.4} dB 不一致（申报合格但系数更热/被替换，F-35）"
                        ),
                    ));
                }
                // 实验旁路：P 恒 identity（不承载 T64 修正），F-35 的 P↔报告链增益绑定显式跳过。
                if !warnings.contains(&W_EXPERIMENT_P_BINDING_SKIPPED) {
                    warnings.push(W_EXPERIMENT_P_BINDING_SKIPPED);
                }
            }
        }
        // 实际链路峰值 = 报告 program peak 基准 + 编码 P 行增益 + 编码 role gain。
        //
        // crossover 行例外：分频支路在 1 kHz 参考点可能深度衰减（低通 @1 kHz ~ -80 dB），
        // 1 kHz 相对链增益会失真成 +几十 dB；D7 预测峰值必须用 **绝对幅度峰值**（|H|max）。
        // crossover 是 cut-only（T39 判据：单段幅度近似 <= +0.5 dB），故绝对峰值 <= +3 dB 单段帽
        // 已在上面硬检；用绝对峰值 + program peak 基准才是守恒的峰值上界。
        let chain_gain_db = if req
            .crossover
            .and_then(|evidence| evidence.role(&role.role_id))
            .is_some()
        {
            if planned_sections.is_empty() {
                encoded_peak_positive_db
            } else {
                combined_peak_positive_db
            }
        } else if planned_sections.is_empty() {
            encoded_gain_db
        } else {
            combined_gain_db
        };
        let predicted_peak_dbfs =
            channel.program_peak_reference_dbfs + chain_gain_db + encoded_role_gain_db(role);
        if predicted_peak_dbfs.is_nan() {
            return Err(ca_err(
                E_CA_NUMERIC,
                format!("roles[{}].p_fir", role.role_index),
                "编码 P 行 D7 预测峰值非有限（F-35）",
            ));
        }
        if predicted_peak_dbfs > D7_PREDICTED_PEAK_LIMIT_DBFS + 1e-9 {
            return Err(ca_err(
                E_CA_D7_PEAK,
                format!("roles[{}].p_fir", role.role_index),
                format!(
                    "编码 P 行预测累计峰值 {predicted_peak_dbfs:.3} dBFS 超过 {} dBFS（headroom < {} dB，F-35）",
                    D7_PREDICTED_PEAK_LIMIT_DBFS, D7_MIN_HEADROOM_DB
                ),
            ));
        }
        let headroom_db = -predicted_peak_dbfs;
        if headroom_db < D7_MIN_HEADROOM_DB - 1e-9 {
            return Err(ca_err(
                E_CA_D7_PEAK,
                format!("roles[{}].p_fir", role.role_index),
                format!(
                    "编码 P 行 headroom {headroom_db:.3} dB < {} dB（F-35 硬门）",
                    D7_MIN_HEADROOM_DB
                ),
            ));
        }
    }
    Ok(warnings)
}

// ---------------------------------------------------------------------------
// T54 artifact（G 每 path 系数 + hash 链）
// ---------------------------------------------------------------------------

/// 一条 G path 编译输入（T54 每 path WAV + manifest 记录）。
#[derive(Clone, Debug, PartialEq)]
pub struct GPathInput {
    /// 输出 dense（G 行）。
    pub out_dense: u16,
    /// 输入 dense（G 列）。
    pub in_dense: u16,
    /// A_map 选中的源通道（TspiPathV1.in_index）；COMPILE_NO_SOURCE = 该列零填。
    pub in_source: u16,
    /// g cell 系数（f64；**不含** delta_D baseline；长度 = L_G）。
    pub coeffs: Vec<f64>,
    /// 是否对角 baseline（仅 out_dense == in_dense）。
    pub diag_baseline: bool,
    /// 是否全零 cell（T54 稀疏标记；对角仍有 baseline）。
    pub zero: bool,
    /// T54 该 path WAV sha256（artifact 绑定）。
    pub coeff_sha256: String,
}

impl GPathInput {
    /// 构造并校验（系数有限、长度>0、diag/zero 语义）。
    pub fn new(
        out_dense: u16,
        in_dense: u16,
        coeffs: Vec<f64>,
        diag_baseline: bool,
        zero: bool,
        coeff_sha256: impl Into<String>,
    ) -> CaResult<Self> {
        let path = Self {
            out_dense,
            in_dense,
            in_source: COMPILE_NO_SOURCE,
            coeffs,
            diag_baseline,
            zero,
            coeff_sha256: coeff_sha256.into(),
        };
        path.validate_lg(None)?;
        Ok(path)
    }

    /// 设置 A_map 选源。
    pub fn with_source(mut self, in_source: u16) -> Self {
        self.in_source = in_source;
        self
    }

    /// 从 T54 artifact path + WAV 字节构造（校验 WAV sha/format/fs/taps/peak/q16/zero）。
    pub fn from_artifact_path(path: &ArtifactPath, wav_bytes: &[u8]) -> CaResult<Self> {
        if path.format != "wav_f32_mono" {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].format",
                format!(
                    "path 格式 {:?} 必须为 wav_f32_mono（T54 ARTIFACT_WAV_FORMAT）",
                    path.format
                ),
            ));
        }
        if path.bytes != wav_bytes.len() as u64 {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].bytes",
                format!(
                    "manifest bytes={} 与实际 WAV 字节 {} 不一致",
                    path.bytes,
                    wav_bytes.len()
                ),
            ));
        }
        let actual_sha = sha256_hex(wav_bytes);
        if actual_sha != path.sha256 {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].sha256",
                format!(
                    "WAV 内容 sha256 {actual_sha} 与 manifest {} 不一致",
                    path.sha256
                ),
            ));
        }
        let decoded = decode_wav_f32_mono(wav_bytes).map_err(|error| {
            ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].wav",
                format!("WAV 解码失败：{error}"),
            )
        })?;
        if decoded.sample_rate_hz != path.fs {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].fs",
                format!(
                    "WAV fs={} 与 manifest fs={} 不一致",
                    decoded.sample_rate_hz, path.fs
                ),
            ));
        }
        if decoded.samples.len() as u32 != path.taps {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].taps",
                format!(
                    "WAV 长度 {} 与 manifest taps={} 不一致",
                    decoded.samples.len(),
                    path.taps
                ),
            ));
        }
        if path.delay_q16 != 0 {
            return Err(ca_err(
                E_CA_DELAY_Q16_ABI,
                "artifact.paths[].delay_q16",
                "ABI v1.0 无 delay_q16 字段（GAP-T66-2）；分数延迟必须先折入系数，q16 必须为 0",
            ));
        }
        let coeffs = f64_from_f32(&decoded.samples);
        let peak = max_abs(&coeffs);
        if (peak - path.peak_abs).abs() > 1e-5 {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].peak_abs",
                format!(
                    "manifest peak_abs={} 与 WAV 实算 {peak} 不一致",
                    path.peak_abs
                ),
            ));
        }
        let all_zero = coeffs.iter().all(|value| *value == 0.0);
        if path.diag_baseline != (path.out_dense == path.in_dense) {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].diag_baseline",
                "diag_baseline 必须当且仅当 out_dense==in_dense 为 true",
            ));
        }
        let expected_zero = all_zero && !path.diag_baseline;
        if path.zero != expected_zero && path.zero != all_zero {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.paths[].zero",
                format!(
                    "manifest zero={} 与实算 all_zero={all_zero} 不一致",
                    path.zero
                ),
            ));
        }
        Ok(Self {
            out_dense: path.out_dense,
            in_dense: path.in_dense,
            in_source: COMPILE_NO_SOURCE,
            coeffs,
            diag_baseline: path.diag_baseline,
            zero: path.zero,
            coeff_sha256: path.sha256.clone(),
        })
    }

    /// 设置/校验 L_G 长度（None = 不检查）。
    pub fn validate_lg(&self, l_g: Option<u32>) -> CaResult<()> {
        if self.coeffs.is_empty() {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                format!("g[{}][{}]", self.out_dense, self.in_dense),
                "path 系数不能为空",
            ));
        }
        if let Some(l_g) = l_g {
            if self.coeffs.len() as u32 != l_g {
                return Err(ca_err(
                    E_CA_PATH_TABLE,
                    format!("g[{}][{}]", self.out_dense, self.in_dense),
                    format!("path 系数长度 {} != 声明 L_G={l_g}", self.coeffs.len()),
                ));
            }
        }
        if self.diag_baseline != (self.out_dense == self.in_dense) {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                format!("g[{}][{}].diag_baseline", self.out_dense, self.in_dense),
                "diag_baseline 必须当且仅当 out_dense==in_dense",
            ));
        }
        let all_zero = self.coeffs.iter().all(|value| *value == 0.0);
        if self.zero && !all_zero {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                format!("g[{}][{}].zero", self.out_dense, self.in_dense),
                "zero=true 但系数非全零",
            ));
        }
        for (index, value) in self.coeffs.iter().enumerate() {
            require_finite(
                *value,
                format!("g[{}][{}][{index}]", self.out_dense, self.in_dense),
            )?;
        }
        Ok(())
    }

    /// f32 系数（已校验有限）。
    pub fn coeffs_f32(&self) -> CaResult<Vec<f32>> {
        to_f32(
            &self.coeffs,
            &format!("g[{}][{}]", self.out_dense, self.in_dense),
        )
    }

    /// f32 系数 canonical sha256（GAP：T54 f64 canonical 口径不同，这里只做报告绑定）。
    pub fn coeffs_f32_sha256(&self) -> CaResult<String> {
        let mut canonical = String::with_capacity(self.coeffs.len() * 26 + 64);
        canonical.push_str("tspi-plan-path-coeffs/v1\n");
        for value in self.coeffs_f32()? {
            let _ = writeln!(canonical, "{value:e}");
        }
        Ok(sha256_hex(canonical.as_bytes()))
    }
}

/// 一个 plan 角色的 P 输入（T64 目标 FIR + T65 delay/polarity + role 元数据）。
#[derive(Clone, Debug, PartialEq)]
pub struct RoleCompileInput {
    /// plan 内 role 序号（0..O-1）。
    pub role_index: u16,
    /// 角色 id（T54/T57 overlay token）。
    pub role_id: String,
    /// 物理输出首槽位（必须与 mapping 一致）。
    pub out_channel: u16,
    /// A_map 选中源通道（None = 零填；stereo strict 下非 FL/FR = None）。
    pub source_channel: Option<u16>,
    /// T65 整数延迟补偿 D（samples）。
    ///
    /// F-16 唯一延迟定义：P FIR 自身线性相位群延迟 GD_p≠0 时 D 必须为 0
    /// （总延迟 = GD_p + D_i，禁止双扣；否则 E_CA_DOUBLE_COUNT）。
    pub delay_samples: u32,
    /// T65 校准极性（+1/-1）。
    pub polarity: i8,
    /// 合并后 role gain（dB；plan 只能编码 [-60,0]，见 GAP-T66-4）。
    pub gain_db: f64,
    /// 输出 P FIR（T64 目标 FIR；**不含** delay/polarity，编码由 compile 做）。
    ///
    /// 线性相位 FIR（对称支撑，T64 目标 FIR 均如此）的固有群延迟
    /// GD_p=(首个非零索引+末个非零索引)/2=(taps-1)/2 已含在系数里；F-16 规定
    /// GD_p≠0 时 `delay_samples` 必须为 0（总延迟=GD_p+D_i，不允许双扣）。
    pub p_fir: Vec<f64>,
    /// T64 PEQ 段数声明（plan ABI v2 per-role IIR 区；必须 == peq_sections.len()）。
    ///
    /// plan ABI v2（GAP-T66-3）落地后，T64 报告的 IIR 段经
    /// [RoleCompileInput::with_peq_sections] 作为 plan 的 per-role IIR 表写入；本字段保留
    /// 为「无系数声明」守卫：peq_count>0 但 peq_sections 为空 = E_CA_PEQ_ABI 拒收
    /// （旧 v1 语义，禁止静默丢弃 PEQ）。
    pub peq_count: u16,
    /// T64 PEQ 级联（plan ABI v2 per-role IIR 区；空 = 无 PEQ，plan 输出 v1 字节）。
    ///
    /// 顺序即级联顺序；每段 biquad 系数 [b0,b1,b2,a1,a2]（a0=1）。系数按 f32 写入 plan
    /// 系数区（64 B 对齐），IIR 表写 role 绑定/段数/偏移并由表 CRC-32C 覆盖。F-37：
    /// T64 报告的 IIR 段与目标 FIR **必须同时进 plan**（H_eff=Hraw·P_IIR·P_FIR），
    /// 只部署其一不能代表 P。
    pub peq_sections: Vec<crate::peq_plan::PeqBiquad>,
    /// P_post 段来源（规范序 crossover → target → phase）。
    ///
    /// 空 = 传统单段行为（diag flags 由行内容启发式推断，与历史完全一致）；
    /// 非空 = 显式段来源：<code>diag_flags_for</code> 直接映射
    /// <code>FirSegmentKind::diag_flag()</code>（T39 crossover 段置 bit2）。
    pub fir_segments: Vec<tspi_dsp_core::nodes::FirSegmentKind>,
}

impl RoleCompileInput {
    /// 构造并校验（role token/极性/gain 范围/有限系数）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        role_index: u16,
        role_id: impl Into<String>,
        out_channel: u16,
        source_channel: Option<u16>,
        delay_samples: u32,
        polarity: i8,
        gain_db: f64,
        p_fir: Vec<f64>,
        peq_count: u16,
    ) -> CaResult<Self> {
        let role = Self {
            role_index,
            role_id: role_id.into(),
            out_channel,
            source_channel,
            delay_samples,
            polarity,
            gain_db,
            p_fir,
            peq_count,
            peq_sections: Vec::new(),
            fir_segments: Vec::new(),
        };
        role.validate()?;
        Ok(role)
    }

    /// identity 角色（目标 FIR=[1]、无延迟/极性、gain 0、无 PEQ）。
    pub fn identity(role_index: u16, role_id: impl Into<String>, out_channel: u16) -> Self {
        Self {
            role_index,
            role_id: role_id.into(),
            out_channel,
            source_channel: None,
            delay_samples: 0,
            polarity: 1,
            gain_db: 0.0,
            p_fir: vec![1.0],
            peq_count: 0,
            peq_sections: Vec::new(),
            fir_segments: Vec::new(),
        }
    }

    /// 显式声明 P_post 段来源（规范序；空 = 传统单段）。
    pub fn with_fir_segments(
        mut self,
        segments: Vec<tspi_dsp_core::nodes::FirSegmentKind>,
    ) -> CaResult<Self> {
        self.fir_segments = segments;
        self.validate()?;
        Ok(self)
    }

    /// 附加 T64 PEQ 级联（plan ABI v2 per-role IIR 区；GAP-T66-3）。
    ///
    /// - 空 = 保持 v1 语义（plan 字节与历史逐字节一致）；
    /// - 非空 = plan abi_version=2，写 IIR 表（role 绑定/段数/系数偏移 + 表 CRC-32C）；
    /// - 若此前经 new(.., peq_count) 声明了段数，必须与 sections 数一致（F-38 同口径）；
    /// - 每段校验有限 + 二阶实系数极点单位圆内（|a2|<1 且 |a1|<1+a2）。
    pub fn with_peq_sections(mut self, sections: Vec<PeqBiquad>) -> CaResult<Self> {
        let declared = self.peq_count as usize;
        if self.peq_count != 0 && declared != sections.len() {
            return Err(ca_err(
                E_CA_PEQ_ABI,
                format!("roles[{}].peq_count", self.role_index),
                format!(
                    "peq_count={} 与提供 IIR 段数 {} 不一致（plan ABI v2 段数必须与 role.peq_count 相等）",
                    self.peq_count,
                    sections.len()
                ),
            ));
        }
        if sections.len() > PLAN_IIR_MAX_SECTIONS {
            return Err(ca_err(
                E_CA_PEQ_ABI,
                format!("roles[{}].peq_sections", self.role_index),
                format!("{} 段超过 {PLAN_IIR_MAX_SECTIONS}", sections.len()),
            ));
        }
        self.peq_count = sections.len() as u16;
        self.peq_sections = sections;
        self.validate()?;
        Ok(self)
    }

    /// 是否含 crossover 段。
    pub fn has_crossover_segment(&self) -> bool {
        self.fir_segments
            .contains(&tspi_dsp_core::nodes::FirSegmentKind::Crossover)
    }

    /// 是否含 target 段。
    pub fn has_target_segment(&self) -> bool {
        self.fir_segments
            .contains(&tspi_dsp_core::nodes::FirSegmentKind::Target)
    }

    /// 从 T65 PhaseChannelOp 构造（delay/polarity 取自实测 op）。
    pub fn from_phase_op(
        op: &PhaseChannelOp,
        role_index: u16,
        out_channel: u16,
        gain_db: f64,
        p_fir: Vec<f64>,
        peq_count: u16,
    ) -> CaResult<Self> {
        Self::new(
            role_index,
            op.channel.clone(),
            out_channel,
            None,
            op.delay_samples,
            op.polarity.sign() as i8,
            gain_db,
            p_fir,
            peq_count,
        )
    }

    /// 校验（不含 L_P/PEQ 跨字段门）。
    pub fn validate(&self) -> CaResult<()> {
        if !is_role_token(&self.role_id) {
            return Err(ca_err(
                E_CA_ROLES,
                format!("roles[{}].role_id", self.role_index),
                format!("role id {:?} 不匹配 ^[A-Z][A-Z0-9]*$", self.role_id),
            ));
        }
        if self.polarity != 1 && self.polarity != -1 {
            return Err(ca_err(
                E_CA_ROLES,
                format!("roles[{}].polarity", self.role_index),
                format!("极性 {} 必须为 ±1", self.polarity),
            ));
        }
        require_finite(self.gain_db, format!("roles[{}].gain_db", self.role_index))?;
        if self.gain_db < ROLE_GAIN_MIN_DB || self.gain_db > ROLE_GAIN_MAX_DB {
            return Err(ca_err(
                E_CA_GAIN_RANGE,
                format!("roles[{}].gain_db", self.role_index),
                format!(
                    "role gain {} dB 越界（CONTRACT/channelmap [-60,+12]）",
                    self.gain_db
                ),
            ));
        }
        if self.gain_db > 0.0 {
            return Err(ca_err(
                E_CA_GAIN_RANGE,
                format!("roles[{}].gain_db", self.role_index),
                "ABI v1.0 role.gain_q15 只能表示 ≤0 dB（GAP-T66-4）；正 trim 需增益节点或折入 P FIR",
            ));
        }
        // plan ABI v2（GAP-T66-3）：peq_count 必须与 IIR 段数一致。无系数的 peq_count>0
        // 仍是旧 v1 守卫（入口与 verify 同一拒收规则，F-38）：不得静默丢弃 T64 PEQ。
        if self.peq_count as usize != self.peq_sections.len() {
            return Err(ca_err(
                E_CA_PEQ_ABI,
                format!("roles[{}].peq_count", self.role_index),
                format!(
                    "peq_count={} != IIR 段数 {}（无系数的 peq_count>0 是旧 plan ABI v1.0 守卫：GAP-T66-3/Q-06；plan ABI v2 需 with_peq_sections 提供系数，F-38）",
                    self.peq_count,
                    self.peq_sections.len()
                ),
            ));
        }
        if self.peq_sections.len() > PLAN_IIR_MAX_SECTIONS {
            return Err(ca_err(
                E_CA_PEQ_ABI,
                format!("roles[{}].peq_sections", self.role_index),
                format!("{} 段超过 {PLAN_IIR_MAX_SECTIONS}", self.peq_sections.len()),
            ));
        }
        for (index, section) in self.peq_sections.iter().enumerate() {
            section.validate().map_err(|detail| {
                ca_err(
                    E_CA_PEQ_ABI,
                    format!("roles[{}].peq_sections[{index}]", self.role_index),
                    detail,
                )
            })?;
        }
        if self.p_fir.is_empty() {
            return Err(ca_err(
                E_CA_ROLES,
                format!("roles[{}].p_fir", self.role_index),
                "P FIR 不能为空（identity 用 [1.0]）",
            ));
        }
        for (index, value) in self.p_fir.iter().enumerate() {
            require_finite(*value, format!("roles[{}].p_fir[{index}]", self.role_index))?;
        }
        // 段来源（显式时）：规范序非降、无重复、类型有限集合（FirSegmentKind 全覆盖）。
        let mut previous_rank: Option<u8> = None;
        let mut seen_segments: Vec<tspi_dsp_core::nodes::FirSegmentKind> = Vec::new();
        for (index, kind) in self.fir_segments.iter().enumerate() {
            if seen_segments.contains(kind) {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{}].fir_segments[{index}]", self.role_index),
                    format!("段 {} 重复（每 role 每段类型最多一个）", kind.schema_name()),
                ));
            }
            if let Some(previous) = previous_rank {
                if kind.rank() < previous {
                    return Err(ca_err(
                        E_CA_ROLES,
                        format!("roles[{}].fir_segments[{index}]", self.role_index),
                        format!(
                            "段序非法：{} 必须在规范序 crossover→target→phase 内非降",
                            kind.schema_name()
                        ),
                    ));
                }
            }
            previous_rank = Some(kind.rank());
            seen_segments.push(*kind);
        }
        if !self.fir_segments.is_empty() && self.p_fir.len() == 1 && self.p_fir[0] == 1.0 {
            return Err(ca_err(
                E_CA_ROLES,
                format!("roles[{}].fir_segments", self.role_index),
                "声明了 FIR 段但 p_fir 是 identity（证据与系数不一致）",
            ));
        }
        Ok(())
    }

    /// role gain_q15（Q15 线性增益；≤0 dB 可表示，1.0=32767）。
    pub fn gain_q15(&self) -> i16 {
        let linear = 10.0_f64.powf(self.gain_db / 20.0);
        let scaled = (linear * 32767.0).round();
        if !scaled.is_finite() || scaled <= 0.0 {
            return 0;
        }
        if scaled >= 32767.0 {
            return 32767;
        }
        scaled as i16
    }
}

/// T54 artifact 绑定（hash 链 + 状态 + stale 判定输入）。
#[derive(Clone, Debug, PartialEq)]
pub struct ArtifactBinding {
    /// artifact schema（必须 = tspi-modal-artifact/v1）。
    pub artifact_schema: String,
    /// artifact 内容 hash（canonical sha256）。
    pub artifact_hash: String,
    /// manifest 文件 sha256。
    pub manifest_sha256: String,
    /// 全部 path WAV 聚合 sha256。
    pub wav_aggregate_sha256: String,
    /// G 解 hash。
    pub solution_hash: String,
    /// design plant hash。
    pub design_plant_hash: String,
    /// target sha256。
    pub target_sha256: String,
    /// 冻结 P 基线 hash。
    pub baseline_hash: String,
    /// measurement session hash（可选）。
    pub measurement_sha256: Option<String>,
    /// calibration IR hash（可选）。
    pub calibration_sha256: Option<String>,
    /// artifact 状态字面值（READY/INCOMPLETE/FAIL）。
    pub status: String,
    /// stale 原因码（非空 = 拒收；由调用方用 ArtifactCurrent 判定）。
    pub stale_reasons: Vec<&'static str>,
    /// host graph hash 回填位（T54 identity.host_graph_sha256；编译后 = hex(graph_id)）。
    pub host_graph_sha256: Option<String>,
}

impl ArtifactBinding {
    /// 从 T54 ArtifactBundle 取 hash 链（stale 需调用方另行判定）。
    pub fn from_bundle(bundle: &ArtifactBundle) -> Self {
        Self {
            artifact_schema: ARTIFACT_SCHEMA.to_string(),
            artifact_hash: bundle.artifact_hash.clone(),
            manifest_sha256: bundle.manifest_sha256.clone(),
            wav_aggregate_sha256: bundle.quantization.wav_aggregate_sha256.clone(),
            solution_hash: bundle.identity.solution_hash.clone().unwrap_or_default(),
            design_plant_hash: bundle.identity.design_plant_hash.clone(),
            target_sha256: bundle.identity.target_sha256.clone(),
            baseline_hash: bundle.identity.baseline_hash.clone(),
            measurement_sha256: bundle.identity.measurement_sha256.clone(),
            calibration_sha256: bundle.identity.calibration_sha256.clone(),
            status: bundle.status.as_str().to_string(),
            stale_reasons: Vec::new(),
            host_graph_sha256: bundle.identity.host_graph_sha256.clone(),
        }
    }

    /// 从 bundle + 当前绑定上下文判定 stale（T54 stale_reasons 固定顺序编码）。
    pub fn from_bundle_with_current(bundle: &ArtifactBundle, current: &ArtifactCurrent) -> Self {
        let mut binding = Self::from_bundle(bundle);
        binding.stale_reasons = bundle
            .stale_reasons(current)
            .into_iter()
            .map(|reason| reason.as_str())
            .collect();
        binding
    }

    /// 从字段直接构造（测试/宿主）。
    pub fn from_parts(
        artifact_hash: impl Into<String>,
        manifest_sha256: impl Into<String>,
        wav_aggregate_sha256: impl Into<String>,
        solution_hash: impl Into<String>,
        design_plant_hash: impl Into<String>,
        target_sha256: impl Into<String>,
        baseline_hash: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            artifact_schema: ARTIFACT_SCHEMA.to_string(),
            artifact_hash: artifact_hash.into(),
            manifest_sha256: manifest_sha256.into(),
            wav_aggregate_sha256: wav_aggregate_sha256.into(),
            solution_hash: solution_hash.into(),
            design_plant_hash: design_plant_hash.into(),
            target_sha256: target_sha256.into(),
            baseline_hash: baseline_hash.into(),
            measurement_sha256: None,
            calibration_sha256: None,
            status: status.into(),
            stale_reasons: Vec::new(),
            host_graph_sha256: None,
        }
    }

    /// 是否 READY（T54 status）。
    pub fn is_ready(&self) -> bool {
        self.status == ArtifactStatus::Ready.as_str()
    }

    /// 校验：schema/状态/hash 完整/非 stale/无 host graph 冲突。
    pub fn validate(&self) -> CaResult<()> {
        self.validate_inner(false)
    }

    /// **实验旁路**：允许 T54 artifact 非 READY（仅 EXPERIMENT/NON_ACCEPTABLE 产物）。
    ///
    /// schema/hash/stale 仍然必须成立；仅跳过 `artifact.status == READY` 这一条生产门。
    /// 调用方（[crate::compile_apply::CompileApplyRequest]）只有在
    /// [ExperimentMode::allow_incomplete_artifact] 显式开启时才会走到这里。
    pub fn validate_allow_incomplete(&self) -> CaResult<()> {
        self.validate_inner(true)
    }

    fn validate_inner(&self, allow_incomplete: bool) -> CaResult<()> {
        if self.artifact_schema != ARTIFACT_SCHEMA {
            return Err(ca_err(
                E_CA_ARTIFACT_HASH,
                "artifact.schema",
                format!(
                    "artifact schema 必须为 {ARTIFACT_SCHEMA}，得到 {:?}",
                    self.artifact_schema
                ),
            ));
        }
        if !allow_incomplete && !self.is_ready() {
            return Err(ca_err(
                E_CA_ARTIFACT_STATUS,
                "artifact.status",
                format!(
                    "T54 artifact 状态 {:?} 不是 READY（INCOMPLETE/FAIL 不得编译应用）",
                    self.status
                ),
            ));
        }
        if !self.stale_reasons.is_empty() {
            return Err(ca_err(
                E_CA_ARTIFACT_STALE,
                "artifact.stale_reasons",
                format!(
                    "artifact 已 stale：{:?}（必须重设计/重编译）",
                    self.stale_reasons
                ),
            ));
        }
        require_sha256(
            &self.artifact_hash,
            "artifact.artifact_hash",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.manifest_sha256,
            "artifact.manifest_sha256",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.wav_aggregate_sha256,
            "artifact.wav_aggregate_sha256",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.solution_hash,
            "artifact.solution_hash",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.design_plant_hash,
            "artifact.design_plant_hash",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.target_sha256,
            "artifact.target_sha256",
            E_CA_ARTIFACT_HASH,
        )?;
        require_sha256(
            &self.baseline_hash,
            "artifact.baseline_hash",
            E_CA_ARTIFACT_HASH,
        )?;
        for (field, value) in [
            ("artifact.measurement_sha256", &self.measurement_sha256),
            ("artifact.calibration_sha256", &self.calibration_sha256),
            ("artifact.host_graph_sha256", &self.host_graph_sha256),
        ] {
            if let Some(value) = value {
                require_sha256(value, field, E_CA_ARTIFACT_HASH)?;
            }
        }
        Ok(())
    }

    /// canonical 文本（确定性）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str("tspi-cal-artifact-binding/v1\n");
        let _ = writeln!(text, "schema={}", self.artifact_schema);
        let _ = writeln!(text, "status={}", self.status);
        let _ = writeln!(text, "artifact_hash={}", self.artifact_hash);
        let _ = writeln!(text, "manifest_sha256={}", self.manifest_sha256);
        let _ = writeln!(text, "wav_aggregate_sha256={}", self.wav_aggregate_sha256);
        let _ = writeln!(text, "solution_hash={}", self.solution_hash);
        let _ = writeln!(text, "design_plant_hash={}", self.design_plant_hash);
        let _ = writeln!(text, "target_sha256={}", self.target_sha256);
        let _ = writeln!(text, "baseline_hash={}", self.baseline_hash);
        let _ = writeln!(
            text,
            "measurement_sha256={}",
            self.measurement_sha256.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            text,
            "calibration_sha256={}",
            self.calibration_sha256.as_deref().unwrap_or("none")
        );
        let _ = writeln!(
            text,
            "host_graph_sha256={}",
            self.host_graph_sha256.as_deref().unwrap_or("none")
        );
        text.push_str("stale_reasons=");
        for (index, reason) in self.stale_reasons.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(reason);
        }
        text.push('\n');
        text
    }

    /// artifact canonical hash（报告绑定）。
    pub fn binding_hash(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }
}

// ---------------------------------------------------------------------------
// 编译选项 / 编码方式 / 请求
// ---------------------------------------------------------------------------

/// delay/polarity 编码方式（T13 PostEncoding 同口径；两种二选一，禁止双扣）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelayPolarityEncoding {
    /// delay/polarity 留在 role 表（P FIR 只含目标形状；kernel 按 role 元数据施加）。
    RoleMetadata,
    /// delay/polarity 折入 P FIR 系数（delta(D)·polarity；role 表写 0/+1）。
    CoeffEmbedded,
}

impl DelayPolarityEncoding {
    /// 字面值。
    pub const fn as_str(self) -> &'static str {
        match self {
            DelayPolarityEncoding::RoleMetadata => "role_metadata",
            DelayPolarityEncoding::CoeffEmbedded => "coeff_embedded",
        }
    }

    /// 解析（未知 = None，拒收）。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "role_metadata" => Some(DelayPolarityEncoding::RoleMetadata),
            "coeff_embedded" => Some(DelayPolarityEncoding::CoeffEmbedded),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 实验模式（EXPERIMENT / NON_ACCEPTABLE）
// ---------------------------------------------------------------------------
//
// 背景（g-singlepoint 实测，证据 evidence/calib-chain-20261121/g-singlepoint-20261121/REPORT.md）：
// 单测点 measured 设计 P=1 < M，T54 需要 >=3 独立 holdout 测点 => artifact 恒 INCOMPLETE，
// compile 以 E_CA_ARTIFACT_STATUS 如实拒绝。为把"单点 G 能 apply 验证"推进到实验级，
// 新增**显式 flag**：默认全关；开启后 compile 允许非 READY artifact，但产物一律标
// EXPERIMENT / NON_ACCEPTABLE / applied=false / certifiable=false，**不得冒充 READY/PASS**。

/// 实验裁决标签（只出现在实验旁路产物；生产路径不得出现）。
pub const EXPERIMENT_VERDICT: &str = "EXPERIMENT";
/// 实验产物可接受性标签（NON_ACCEPTABLE = 不可作为交付/验收依据）。
pub const EXPERIMENT_ACCEPTABILITY: &str = "NON_ACCEPTABLE";
/// 单点实验作用域（不产生任何空间/多点结论）。
pub const EXPERIMENT_SCOPE_SINGLE_POINT: &str = "SINGLE_POINT_NOT_SPATIAL";
/// 多点 pilot 实验作用域（design>=2 + 独立 validation>=1；只做接线/预测评估，不产生 T6/holdout 结论）。
pub const EXPERIMENT_SCOPE_MULTIPOINT_PILOT: &str = "MULTIPOINT_PILOT_NOT_CERTIFIABLE";
/// 实验 envelope artifact 相对路径。
pub const EXPERIMENT_ENVELOPE_ARTIFACT_PATH: &str = "compile/experiment-envelope.json";
/// 实验 envelope schema。
pub const EXPERIMENT_SCHEMA: &str = "tspi-cal-experiment/v1";
/// 实验模式组合非法。
pub const E_CA_EXPERIMENT: &str = "E_CA_EXPERIMENT";

/// 实验模式开关（**默认全关**；任何一项开启都会让 compile 产物被标 EXPERIMENT/NON_ACCEPTABLE）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExperimentMode {
    /// 单点 G 实验（单测点 P<M，T54 artifact 恒 INCOMPLETE）。
    pub single_point: bool,
    /// 多点 pilot 实验（design>=2 + 独立 validation>=1；用于生产多点窗口前的接线/预测评估）。
    pub multipoint_pilot: bool,
    /// 允许 T54 artifact 非 READY 仍编译（必须与 `single_point`/`multipoint_pilot` 同时开启）。
    pub allow_incomplete_artifact: bool,
}

impl ExperimentMode {
    /// 生产默认（全关；行为与历史完全一致）。
    pub const OFF: Self = Self {
        single_point: false,
        multipoint_pilot: false,
        allow_incomplete_artifact: false,
    };
    /// 单点实验（含 artifact 状态放宽）。
    pub const SINGLE_POINT: Self = Self {
        single_point: true,
        multipoint_pilot: false,
        allow_incomplete_artifact: true,
    };
    /// 多点 pilot 实验（design>=2 + 独立 validation>=1；含 artifact 状态放宽）。
    pub const MULTIPOINT_PILOT: Self = Self {
        single_point: false,
        multipoint_pilot: true,
        allow_incomplete_artifact: true,
    };

    /// 是否处于实验模式。
    pub fn is_active(&self) -> bool {
        self.single_point || self.multipoint_pilot || self.allow_incomplete_artifact
    }

    /// 是否显式绕过生产门（单点/多点 pilot 均属实验旁路；OFF 时恒 false）。
    pub fn bypasses_production_gates(&self) -> bool {
        self.single_point || self.multipoint_pilot
    }

    /// 实验作用域标签（envelope/report 证据口径；不是验收结论）。
    pub fn scope(&self) -> &'static str {
        if self.multipoint_pilot {
            EXPERIMENT_SCOPE_MULTIPOINT_PILOT
        } else {
            EXPERIMENT_SCOPE_SINGLE_POINT
        }
    }

    /// 校验开关组合（fail-closed：不允许单独放宽 T54 状态门；两种 pilot 互斥）。
    pub fn validate(&self) -> CaResult<()> {
        if self.single_point && self.multipoint_pilot {
            return Err(ca_err(
                E_CA_EXPERIMENT,
                "experiment",
                "single_point 与 multipoint_pilot 互斥（一个实验产物只能声明的 pilot 形态）",
            ));
        }
        if self.allow_incomplete_artifact && !self.bypasses_production_gates() {
            return Err(ca_err(
                E_CA_EXPERIMENT,
                "experiment.allow_incomplete_artifact",
                "allow_incomplete_artifact 必须与 experiment.single_point / multipoint_pilot 同时开启（生产路径禁止单独放宽 T54 状态门）",
            ));
        }
        Ok(())
    }

    /// 标记字段 JSON（单行；供 report/envelope 复用；确定性顺序）。
    pub fn markers_json(&self) -> String {
        format!(
            "{{\"active\": {}, \"verdict\": \"{EXPERIMENT_VERDICT}\", \"acceptability\": \"{EXPERIMENT_ACCEPTABILITY}\", \"applied\": false, \"certifiable\": false, \"scope\": \"{}\", \"single_point\": {}, \"multipoint_pilot\": {}, \"allow_incomplete_artifact\": {}}}",
            self.is_active(),
            self.scope(),
            self.single_point,
            self.multipoint_pilot,
            self.allow_incomplete_artifact
        )
    }
}

/// 编译/打包选项（profile 级）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileApplyOptions {
    /// 本 profile 每 route 有效支撑上限（常规 4096；extended 8192/16384）。
    pub max_taps: u32,
    /// extended taps 是否已显式准入。
    pub extended_taps_admitted: bool,
    /// plan f32 系数区预算（字节）。
    pub coeff_budget_bytes: u64,
    /// expected workers（4 生产 / 2 独立降级 profile）。
    pub workers: u8,
    /// 强制形态（None = 按 T12 成本选择）。
    pub force_form: Option<FusionForm>,
}

impl Default for CompileApplyOptions {
    fn default() -> Self {
        Self {
            max_taps: TAPS_NORMAL_MAX,
            extended_taps_admitted: false,
            coeff_budget_bytes: 4 * 1024 * 1024,
            workers: RT_WORKERS,
            force_form: None,
        }
    }
}

impl CompileApplyOptions {
    /// 校验选项。
    pub fn validate(&self) -> CaResult<()> {
        if self.max_taps == 0 || self.max_taps > TAPS_EXTENDED_MAX {
            return Err(ca_err(
                E_CA_OPTIONS,
                "options.max_taps",
                format!(
                    "max_taps={} 越界（1..={TAPS_EXTENDED_MAX}；>16384 仅设计工具 offline）",
                    self.max_taps
                ),
            ));
        }
        if self.max_taps > TAPS_NORMAL_MAX && !self.extended_taps_admitted {
            return Err(ca_err(
                E_CA_OPTIONS,
                "options.extended_taps_admitted",
                "max_taps>4096 必须显式 extended 准入（core-v1 §3.3；禁止静默扩档）",
            ));
        }
        if self.coeff_budget_bytes == 0 {
            return Err(ca_err(
                E_CA_OPTIONS,
                "options.coeff_budget_bytes",
                "系数预算必须 >0",
            ));
        }
        if self.workers != RT_WORKERS && self.workers != tspi_dsp_core::rt::RT_WORKERS_FALLBACK {
            return Err(ca_err(
                E_CA_OPTIONS,
                "options.workers",
                format!(
                    "workers={} 必须为 4（生产）或 2（独立降级 profile）",
                    self.workers
                ),
            ));
        }
        Ok(())
    }
}

/// 编译请求（全部只读借用；G/P/mapping/phase/headroom/artifact 证据）。
pub struct CompileApplyRequest<'a> {
    /// 采样率（48000/96000）。
    pub sample_rate_hz: u32,
    /// block 大小（64/128）。
    pub block_size: u16,
    /// 输入通道数 N（A_map 源域；1..=20）。
    pub n_in: u16,
    /// 物理容器通道数 C（设备宽度；>= roles）。
    pub container_channels: u16,
    /// 低带 G baseline 整数延迟 D（计入 L_G 支撑；kernel 不得重复加）。
    pub low_band_d: u32,
    /// G 声明支撑 L_G（>= D+1）。
    pub l_g: u32,
    /// P 声明支撑 L_P（>=1；每 role 行零填充到该长度）。
    pub l_p: u32,
    /// T57 overlay/mapping 绑定。
    pub mapping: &'a MappingBinding,
    /// T65 phase 证据。
    pub phase: &'a PhaseEvidence,
    /// T64 D7 headroom 证据。
    pub headroom: &'a HeadroomEvidence,
    /// T54 artifact 绑定。
    pub artifact: &'a ArtifactBinding,
    /// G 每 path 系数（T54）。
    pub paths: &'a [GPathInput],
    /// P 每 role 输入（T64 目标 FIR + T65 delay/polarity）。
    pub roles: &'a [RoleCompileInput],
    /// delay/polarity 编码方式。
    pub encoding: DelayPolarityEncoding,
    /// 编译/打包选项。
    pub options: CompileApplyOptions,
    /// 实验模式（**默认 OFF**；开启后产物必须标 EXPERIMENT/NON_ACCEPTABLE）。
    pub experiment: ExperimentMode,
    /// 实验旁路记录（哪些生产门被显式绕过；写进 experiment envelope，只作证据）。
    pub experiment_notes: Vec<String>,
    /// crossover（T39 分频）D7 证据（None = P_post 不含分频段，行为与历史完全一致）。
    pub crossover: Option<&'a CrossoverEvidence>,
}

impl<'a> CompileApplyRequest<'a> {
    /// 构造（字段显式）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sample_rate_hz: u32,
        block_size: u16,
        n_in: u16,
        container_channels: u16,
        low_band_d: u32,
        l_g: u32,
        l_p: u32,
        mapping: &'a MappingBinding,
        phase: &'a PhaseEvidence,
        headroom: &'a HeadroomEvidence,
        artifact: &'a ArtifactBinding,
        paths: &'a [GPathInput],
        roles: &'a [RoleCompileInput],
        encoding: DelayPolarityEncoding,
        options: CompileApplyOptions,
    ) -> Self {
        Self {
            sample_rate_hz,
            block_size,
            n_in,
            container_channels,
            low_band_d,
            l_g,
            l_p,
            mapping,
            phase,
            headroom,
            artifact,
            paths,
            roles,
            encoding,
            options,
            experiment: ExperimentMode::OFF,
            experiment_notes: Vec::new(),
            crossover: None,
        }
    }

    /// 附加 crossover 证据（D7 绑定；None = 不含分频段）。
    pub fn with_crossover(mut self, crossover: &'a CrossoverEvidence) -> Self {
        self.crossover = Some(crossover);
        self
    }

    /// 显式开启实验模式（默认 OFF；生产路径不调用）。
    pub fn with_experiment(mut self, experiment: ExperimentMode) -> Self {
        self.experiment = experiment;
        self
    }

    /// 记录实验旁路的生产门（证据用；不参与判定）。
    pub fn with_experiment_notes(mut self, notes: Vec<String>) -> Self {
        self.experiment_notes = notes;
        self
    }

    /// role 表项。
    pub fn role(&self, role_index: u16) -> Option<&RoleCompileInput> {
        self.roles.iter().find(|role| role.role_index == role_index)
    }

    /// participants（dense 顺序 -> plan role 序号）；要求严格升序且每项在 role 表中。
    pub fn participant_role_indices(&self) -> CaResult<Vec<u16>> {
        let mut out = Vec::with_capacity(self.mapping.dense_to_dac.len());
        let mut previous: Option<u16> = None;
        for (dense, dac) in self.mapping.dense_to_dac.iter().enumerate() {
            let role_index = match self.mapping.role_index_for_dac(*dac) {
                Some(role_index) => role_index,
                None => {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        format!("mapping.dense_to_dac[{dense}]"),
                        format!("DAC {dac} 无对应 role（overlay 绑定不一致）"),
                    ))
                }
            };
            if let Some(previous) = previous {
                if role_index <= previous {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        "mapping.dense_to_dac",
                        "participants 必须映射到严格升序的 role 序号（dense 顺序 = G 行列顺序）",
                    ));
                }
            }
            previous = Some(role_index);
            out.push(role_index);
        }
        Ok(out)
    }

    /// 请求级校验（fs/容量/role/路径/mapping/phase/D7/artifact/编码一致性）。
    pub fn validate(&self) -> CaResult<()> {
        if !SAMPLE_RATES_HZ.contains(&self.sample_rate_hz) {
            return Err(ca_err(
                E_CA_FS,
                "sample_rate_hz",
                format!(
                    "fs={} 不是 P1 runtime 采样率（{:?}）",
                    self.sample_rate_hz, SAMPLE_RATES_HZ
                ),
            ));
        }
        if !BLOCK_SIZES.contains(&self.block_size) {
            return Err(ca_err(
                E_CA_BLOCK,
                "block_size",
                format!(
                    "B={} 不是 runtime 块长（{:?}）",
                    self.block_size, BLOCK_SIZES
                ),
            ));
        }
        if self.n_in == 0 || self.n_in as usize > MAX_INPUT_CHANNELS {
            return Err(ca_err(
                E_CA_CHANNELS,
                "n_in",
                format!("N={} 越界（1..={MAX_INPUT_CHANNELS}）", self.n_in),
            ));
        }
        if self.container_channels == 0 || self.container_channels as usize > MAX_OUTPUT_CHANNELS {
            return Err(ca_err(
                E_CA_CHANNELS,
                "container_channels",
                format!(
                    "C={} 越界（1..={MAX_OUTPUT_CHANNELS}）",
                    self.container_channels
                ),
            ));
        }
        self.options.validate()?;
        self.experiment.validate()?;
        self.mapping.validate()?;
        if self.mapping.container_channels != self.container_channels {
            return Err(ca_err(
                E_CA_MAPPING,
                "container_channels",
                format!(
                    "请求 container_channels={} 与 mapping {} 不一致",
                    self.container_channels, self.mapping.container_channels
                ),
            ));
        }
        if self.roles.is_empty() {
            return Err(ca_err(E_CA_ROLES, "roles", "role 表不能为空"));
        }
        if self.roles.len() > MAX_OUTPUT_ROLES {
            return Err(ca_err(
                E_CA_ROLES,
                "roles",
                format!("role 数 {} 超过 {MAX_OUTPUT_ROLES}", self.roles.len()),
            ));
        }
        if self.roles.len() > self.container_channels as usize {
            return Err(ca_err(E_CA_ROLES, "roles", "role 数不能超过容器通道数"));
        }
        if self.l_g == 0 || self.l_g > self.options.max_taps {
            return Err(ca_err(
                E_CA_TAPS,
                "l_g",
                format!(
                    "L_G={} 越界（1..={}；extended/GAP 见 core-v1 §3.3）",
                    self.l_g, self.options.max_taps
                ),
            ));
        }
        if self.l_p == 0 || self.l_p > self.options.max_taps {
            return Err(ca_err(
                E_CA_TAPS,
                "l_p",
                format!("L_P={} 越界（1..={}）", self.l_p, self.options.max_taps),
            ));
        }
        if self.low_band_d as u64 >= self.l_g as u64 {
            return Err(ca_err(
                E_CA_TAPS,
                "low_band_d",
                format!(
                    "baseline D={} 必须 < L_G={}（D 计入 G 支撑；T03 §3.1）",
                    self.low_band_d, self.l_g
                ),
            ));
        }
        let max_delay = (self.sample_rate_hz as u64) / 5;
        if self.low_band_d as u64 > max_delay {
            return Err(ca_err(
                E_CA_TAPS,
                "low_band_d",
                format!(
                    "baseline D={} 超过 floor(0.2·fs)={max_delay}（200 ms 上限）",
                    self.low_band_d
                ),
            ));
        }
        // role 表：序号连续、token 唯一、DAC 与 mapping 一致、唯一。
        let mut seen_role_ids: Vec<&str> = Vec::with_capacity(self.roles.len());
        let mut seen_dacs: Vec<u16> = Vec::with_capacity(self.roles.len());
        for (position, role) in self.roles.iter().enumerate() {
            if role.role_index as usize != position {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{position}].role_index"),
                    format!(
                        "role_index {} 必须等于表内序号 {position}（严格升序无空洞）",
                        role.role_index
                    ),
                ));
            }
            role.validate()?;
            if seen_role_ids.iter().any(|id| *id == role.role_id) {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{position}].role_id"),
                    format!("role id {:?} 重复", role.role_id),
                ));
            }
            seen_role_ids.push(role.role_id.as_str());
            if role.out_channel >= self.container_channels {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{position}].out_channel"),
                    format!(
                        "out_channel {} 越界（container_channels={}）",
                        role.out_channel, self.container_channels
                    ),
                ));
            }
            if seen_dacs.contains(&role.out_channel) {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{position}].out_channel"),
                    format!("out_channel {} 重复（scatter 必须单射）", role.out_channel),
                ));
            }
            seen_dacs.push(role.out_channel);
            match self.mapping.role(role.role_index) {
                Some(bound) => {
                    if bound.role_id != role.role_id || bound.dac != role.out_channel {
                        return Err(ca_err(
                            E_CA_MAPPING,
                            format!("roles[{position}]"),
                            format!(
                                "role 绑定与 overlay 不一致（role_id {:?}/{:?}, dac {}/{}）",
                                bound.role_id, role.role_id, bound.dac, role.out_channel
                            ),
                        ));
                    }
                }
                None => {
                    return Err(ca_err(
                        E_CA_MAPPING,
                        format!("roles[{position}]"),
                        "mapping 缺少该 role 的 overlay 绑定",
                    ))
                }
            }
            // A_map：每 role 至多一个源、源唯一（选择/零填，禁止复制分发）。
            if let Some(source) = role.source_channel {
                if source >= self.n_in {
                    return Err(ca_err(
                        E_CA_ROLES,
                        format!("roles[{position}].source_channel"),
                        format!("source {source} 越界（n_in={}）", self.n_in),
                    ));
                }
            }
            if role.delay_samples as u64 > max_delay {
                return Err(ca_err(
                    E_CA_ROLES,
                    format!("roles[{position}].delay_samples"),
                    format!(
                        "role 延迟 {} 超过 floor(0.2·fs)={max_delay}（200 ms 上限，不截断）",
                        role.delay_samples
                    ),
                ));
            }
            if role.p_fir.len() as u32 > self.l_p {
                return Err(ca_err(
                    E_CA_TAPS,
                    format!("roles[{position}].p_fir"),
                    format!("P FIR 长度 {} 超过 L_P={}", role.p_fir.len(), self.l_p),
                ));
            }
        }
        let mut used_sources: Vec<u16> = Vec::new();
        for (position, role) in self.roles.iter().enumerate() {
            if let Some(source) = role.source_channel {
                if used_sources.contains(&source) {
                    return Err(ca_err(
                        E_CA_ROLES,
                        format!("roles[{position}].source_channel"),
                        format!("源通道 {source} 被多个 role 选中（A_map 禁止复制/分发）"),
                    ));
                }
                used_sources.push(source);
            }
        }
        // G 路径表校验由 build_dense_g 做（依赖 participants）。
        let _ = self.participant_role_indices()?;
        // phase 证据与 role ops 一致（防双扣/漏扣）。
        self.validate_phase_against_roles()?;
        // D7 证据（所有 role 都必须有记录）。
        let role_ids: Vec<String> = self.roles.iter().map(|role| role.role_id.clone()).collect();
        self.headroom.validate(&role_ids)?;
        // crossover 证据（若 P_post 含分频段）：与 role 段来源/长度/声明延迟双向绑定。
        self.validate_crossover_against_roles()?;
        // artifact 状态/hash/stale（实验模式下仅放宽 status，hash/stale 仍必须成立）。
        if self.experiment.allow_incomplete_artifact {
            self.artifact.validate_allow_incomplete()?;
        } else {
            self.artifact.validate()?;
        }
        // 编码一致性：CoeffEmbedded 需要 L_P 能容纳 p_fir + D。
        if self.encoding == DelayPolarityEncoding::CoeffEmbedded {
            for (position, role) in self.roles.iter().enumerate() {
                let total = role.p_fir.len() as u64 + role.delay_samples as u64;
                if total > self.l_p as u64 {
                    return Err(ca_err(
                        E_CA_TAPS,
                        format!("roles[{position}]"),
                        format!(
                            "CoeffEmbedded 需要 p_fir({}) + D({}) = {} taps，超过 L_P={}",
                            role.p_fir.len(),
                            role.delay_samples,
                            total,
                            self.l_p
                        ),
                    ));
                }
            }
        }
        if let Some(host_graph) = &self.artifact.host_graph_sha256 {
            require_sha256(host_graph, "artifact.host_graph_sha256", E_CA_ARTIFACT_HASH)?;
        }
        Ok(())
    }

    fn validate_phase_against_roles(&self) -> CaResult<()> {
        self.phase.validate(self.sample_rate_hz)?;
        if self.phase.channels.len() != self.roles.len() {
            return Err(ca_err(
                E_CA_PHASE,
                "phase.channels",
                format!(
                    "phase ops {} 条与 plan role 数 {} 不一致（stale/错配）",
                    self.phase.channels.len(),
                    self.roles.len()
                ),
            ));
        }
        for (position, role) in self.roles.iter().enumerate() {
            let evidence = match self.phase.channel(&role.role_id) {
                Some(evidence) => evidence,
                None => {
                    return Err(ca_err(
                        E_CA_PHASE,
                        format!("roles[{position}].role_id"),
                        format!("phase ops 缺少角色 {:?}（T65 未覆盖）", role.role_id),
                    ))
                }
            };
            if evidence.delay_samples != role.delay_samples {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("roles[{position}].delay_samples"),
                    format!(
                        "role 延迟 {} 与 T65 op {} 不一致（禁止双来源/双扣）",
                        role.delay_samples, evidence.delay_samples
                    ),
                ));
            }
            if evidence.polarity != role.polarity {
                return Err(ca_err(
                    E_CA_PHASE,
                    format!("roles[{position}].polarity"),
                    format!(
                        "role 极性 {} 与 T65 op {} 不一致（禁止双乘/漏乘）",
                        role.polarity, evidence.polarity
                    ),
                ));
            }
        }
        Ok(())
    }

    /// crossover 证据与 role 段来源一致性（缺证据 = 不可判定，不得编译）。
    fn validate_crossover_against_roles(&self) -> CaResult<()> {
        match self.crossover {
            Some(evidence) => evidence.validate(self),
            None => {
                for role in self.roles {
                    if role.has_crossover_segment() {
                        return Err(ca_err(
                            E_CA_CROSSOVER,
                            format!("roles[{}].fir_segments", role.role_index),
                            "role 含 crossover 段但缺 CrossoverEvidence（D7 绑定缺失，不得编译）",
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 编译产物（表 + 记录 + 报告）
// ---------------------------------------------------------------------------

/// 编译事实（T12 成本/选择/长度；写进报告）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileFacts {
    /// 选择的形态。
    pub chosen_form: &'static str,
    /// 选择原因。
    pub selection_reason: &'static str,
    /// L_F = L_G + L_P - 1。
    pub fused_len_gp: u32,
    /// L_total（含 A）。
    pub full_path_len: u32,
    /// fused 是否可行。
    pub fused_feasible: bool,
    /// 非零矩阵路径数。
    pub route_count: u32,
    /// factorized 每采样 MAC。
    pub factorized_macs_per_sample: u64,
    /// fused 每采样 MAC。
    pub fused_macs_per_sample: u64,
    /// saved MAC/sample（>0 = fused 更省）。
    pub saved_macs_per_sample: i64,
    /// factorized FLOP/s（模型，非实测）。
    pub factorized_flops_per_second: f64,
    /// fused FLOP/s（模型，非实测）。
    pub fused_flops_per_second: f64,
    /// profile taps 上限。
    pub taps_limit: u32,
    /// extended 是否准入。
    pub extended_taps_admitted: bool,
    /// 系数预算（字节）。
    pub coeff_budget_bytes: u64,
}

/// 单条 plan path 编译记录（报告/审计用）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledPathRecord {
    /// 物理输出索引（容器槽位）。
    pub out_index: u16,
    /// 输入源通道索引。
    pub in_index: u16,
    /// 输出角色 id。
    pub out_role: String,
    /// 输入角色 id（G 列；fused 由源通道反查）。
    pub in_role: String,
    /// 输出 DAC。
    pub out_dac: u16,
    /// taps。
    pub taps: u32,
    /// 整数延迟（baseline D 已含在支撑内；kernel 不得重复加）。
    pub delay_samples: u32,
    /// 次采样延迟（ABI v1.0 恒 0）。
    pub delay_q16: u32,
    /// 系数外极性（融合/矩阵系数已含时恒 +1）。
    pub polarity: i8,
    /// path flags。
    pub flags: u8,
    /// 是否含 diag baseline。
    pub diag_baseline: bool,
    /// 是否 fused 路径。
    pub fused: bool,
    /// f32 系数 canonical sha256（报告绑定）。
    pub coeff_sha256: String,
    /// 系数峰值绝对值。
    pub peak_abs: f64,
}

/// 单 role 编译记录（报告/审计用）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledRoleRecord {
    /// role 序号。
    pub role_index: u16,
    /// role id。
    pub role_id: String,
    /// 物理输出首槽位。
    pub out_channel: u16,
    /// role 表延迟（RoleMetadata 编码为 D；CoeffEmbedded 为 0）。
    pub delay_samples: u32,
    /// role 表极性（RoleMetadata 编码为实测极性；CoeffEmbedded 为 +1）。
    pub polarity: i8,
    /// Q15 linear gain。
    pub gain_q15: i16,
    /// role 表 PEQ 段数（本模块恒 0）。
    pub peq_count: u16,
    /// P FIR taps。
    pub p_taps: u32,
    /// P FIR 峰值绝对值。
    pub p_peak_abs: f64,
    /// P FIR canonical sha256。
    pub p_coeff_sha256: String,
    /// T65 实测延迟（= role 表延迟或折入系数）。
    pub phase_delay_samples: u32,
    /// T65 实测极性。
    pub phase_polarity: i8,
    /// 是否纯 delay/polarity（目标形状 = identity）。
    pub delay_polarity_only: bool,
}

/// 编译报告里的 crossover 摘要（仅在 P_post 含分频段时出现；缺省报告字节不变）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompileCrossoverSummary {
    /// crossover 证据 canonical sha256（D7 绑定）。
    pub evidence_sha256: String,
    /// T39 设计报告 canonical sha256。
    pub design_report_sha256: String,
    /// T39 类型（LR4 等）。
    pub kind: String,
    /// 交叉点（Hz）。
    pub fc_hz: f64,
    /// 每支 taps。
    pub taps: u32,
    /// 含 crossover 段的 diag 行数。
    pub diag_count: u16,
    /// 含 crossover 段的 role（role_id, branch）。
    pub roles: Vec<(String, String)>,
    /// 卷积后 P FIR 最大长度。
    pub p_taps_max: u32,
}

/// 编译报告里的 plan ABI v2 per-role IIR 摘要（仅在有 T64 PEQ 段时出现；无 IIR 时
/// 报告 canonical/JSON 字节与历史完全一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompileIirSummary {
    /// plan abi_version（有 IIR 时恒 2）。
    pub abi_version: u16,
    /// 含 IIR 表项的 role 数。
    pub role_count: u16,
    /// IIR 段总数。
    pub section_count: u32,
    /// IIR 表字节 sha256（表头(crc 含)+表项+系数 blob 布局后的表字节）。
    pub table_sha256: String,
    /// IIR 表起址（plan 字节偏移）。
    pub table_offset: u32,
    /// 每 role 的段数（role_id, section_count；按 role_id 升序）。
    pub roles: Vec<(String, u16)>,
}

/// 编译报告（确定性 JSON；artifact compile/compile-report.json）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompileReport {
    pub sample_rate_hz: u32,
    pub block_size: u16,
    pub n_in: u16,
    pub container_channels: u16,
    pub workers: u8,
    pub encoding: &'static str,
    pub layout_id: String,
    pub mapping_hash: String,
    pub overlay_source_sha256: String,
    pub binding_hash: String,
    pub role_hash: String,
    pub participants_binding_hash: String,
    pub stereo_strict: bool,
    pub stereo_mask: Vec<String>,
    pub dense_to_dac: Vec<u16>,
    pub unused_outputs: Vec<u16>,
    pub phase_hash: String,
    pub phase_ops_sha256: String,
    pub phase_design_report_sha256: String,
    pub phase_reference_channel: String,
    pub phase_residual_max_samples: f64,
    pub phase_tolerance_samples: f64,
    pub phase_direction_ok: bool,
    pub phase_channels: Vec<PhaseChannelEvidence>,
    pub headroom_hash: String,
    pub headroom_report_sha256: String,
    pub headroom_measured_peak_dbfs: Option<f64>,
    pub d7_warnings: Vec<&'static str>,
    pub artifact_hash: String,
    pub artifact_manifest_sha256: String,
    pub artifact_wav_aggregate_sha256: String,
    pub artifact_solution_hash: String,
    pub artifact_design_plant_hash: String,
    pub artifact_target_sha256: String,
    pub artifact_baseline_hash: String,
    pub artifact_status: String,
    pub host_graph_sha256: String,
    pub graph_id: [u8; 32],
    pub plan_sha256: String,
    pub plan_version: u32,
    pub path_count: u16,
    pub diag_count: u16,
    pub role_count: u16,
    pub low_band_count: u16,
    pub d_samples: u32,
    pub l_g: u32,
    pub l_p: u32,
    pub max_taps: u32,
    pub total_coeff_bytes: u32,
    pub facts: CompileFacts,
    pub paths: Vec<CompiledPathRecord>,
    pub roles: Vec<CompiledRoleRecord>,
    /// crossover 摘要（None = 无分频段；canonical/JSON 均不出现该节，历史字节不变）。
    pub crossover: Option<CompileCrossoverSummary>,
    /// plan ABI v2 per-role IIR 摘要（None = 无 T64 PEQ 段；canonical/JSON 均不出现该节，
    /// 无 PEQ 时报告字节与历史 bit-exact）。
    pub iir: Option<CompileIirSummary>,
}

impl CompileReport {
    /// canonical 文本（确定性；同输入同字节）。
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        text.push_str(COMPILE_REPORT_HASH_DOMAIN);
        text.push('\n');
        let _ = writeln!(text, "schema={COMPILE_APPLY_SCHEMA}");
        let _ = writeln!(
            text,
            "tool={} {}",
            COMPILE_APPLY_TOOL_NAME, COMPILE_APPLY_TOOL_VERSION
        );
        let _ = writeln!(
            text,
            "fs={} block={} n_in={} container={} workers={} encoding={}",
            self.sample_rate_hz,
            self.block_size,
            self.n_in,
            self.container_channels,
            self.workers,
            self.encoding
        );
        let _ = writeln!(
            text,
            "mapping layout={} hash={} overlay={} binding={} role={} participants={} stereo_strict={}",
            self.layout_id,
            self.mapping_hash,
            self.overlay_source_sha256,
            self.binding_hash,
            self.role_hash,
            self.participants_binding_hash,
            self.stereo_strict as u8
        );
        text.push_str("dense_to_dac=");
        for (index, dac) in self.dense_to_dac.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(text, "{dac}");
        }
        text.push('\n');
        text.push_str("unused_outputs=");
        for (index, output) in self.unused_outputs.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(text, "{output}");
        }
        text.push('\n');
        text.push_str("stereo_mask=");
        for (index, role) in self.stereo_mask.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(role);
        }
        text.push('\n');
        let _ = writeln!(
            text,
            "phase hash={} ops={} design={} direction_ok={} residual={:.12} tol={:.12} reference={}",
            self.phase_hash,
            self.phase_ops_sha256,
            self.phase_design_report_sha256,
            self.phase_direction_ok as u8,
            self.phase_residual_max_samples,
            self.phase_tolerance_samples,
            self.phase_reference_channel
        );
        for channel in &self.phase_channels {
            let _ = writeln!(
                text,
                "phase_channel {} delay={} polarity={}",
                channel.channel, channel.delay_samples, channel.polarity
            );
        }
        let _ = writeln!(
            text,
            "headroom hash={} report={} measured_peak={} warnings={:?}",
            self.headroom_hash,
            self.headroom_report_sha256,
            self.headroom_measured_peak_dbfs
                .map(|value| format!("{value:.12}"))
                .unwrap_or_else(|| "none".to_string()),
            self.d7_warnings
        );
        let _ = writeln!(
            text,
            "artifact hash={} manifest={} wav_aggregate={} solution={} design_plant={} target={} baseline={} status={}",
            self.artifact_hash,
            self.artifact_manifest_sha256,
            self.artifact_wav_aggregate_sha256,
            self.artifact_solution_hash,
            self.artifact_design_plant_hash,
            self.artifact_target_sha256,
            self.artifact_baseline_hash,
            self.artifact_status
        );
        let _ = writeln!(
            text,
            "plan graph_id={} sha256={} version={} paths={} diags={} roles={} m={} d={} l_g={} l_p={} max_taps={} coeff_bytes={}",
            hex32(&self.graph_id),
            self.plan_sha256,
            self.plan_version,
            self.path_count,
            self.diag_count,
            self.role_count,
            self.low_band_count,
            self.d_samples,
            self.l_g,
            self.l_p,
            self.max_taps,
            self.total_coeff_bytes
        );
        // plan ABI v2 IIR 摘要只在有 T64 PEQ 段时出现：无 PEQ 的 canonical 字节与历史完全一致。
        if let Some(iir) = self.iir.as_ref() {
            let roles = iir
                .roles
                .iter()
                .map(|(role, sections)| format!("{role}:{sections}"))
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(
                text,
                "iir abi={} roles={} sections={} table_offset={} table_sha256={} role_sections={roles}",
                iir.abi_version,
                iir.role_count,
                iir.section_count,
                iir.table_offset,
                iir.table_sha256
            );
        }
        // crossover 摘要只在含分频段时出现：无 crossover 的报告 canonical 字节与历史完全一致。
        if let Some(crossover) = self.crossover.as_ref() {
            let roles = crossover
                .roles
                .iter()
                .map(|(role, branch)| format!("{role}:{branch}"))
                .collect::<Vec<_>>()
                .join(",");
            let _ = writeln!(
                text,
                "crossover evidence={} design={} kind={} fc={:.17e} taps={} diag_rows={} p_taps_max={} roles={}",
                crossover.evidence_sha256,
                crossover.design_report_sha256,
                crossover.kind,
                crossover.fc_hz,
                crossover.taps,
                crossover.diag_count,
                crossover.p_taps_max,
                roles
            );
        }
        let _ = writeln!(
            text,
            "facts chosen={} reason={} fused_len_gp={} full_path_len={} fused_feasible={} routes={} saved_macs={}",
            self.facts.chosen_form,
            self.facts.selection_reason,
            self.facts.fused_len_gp,
            self.facts.full_path_len,
            self.facts.fused_feasible as u8,
            self.facts.route_count,
            self.facts.saved_macs_per_sample
        );
        for path in &self.paths {
            let _ = writeln!(
                text,
                "path out={} in={} role_out={} role_in={} dac={} taps={} delay={} q16={} pol={} flags={} coeff_sha={}",
                path.out_index,
                path.in_index,
                path.out_role,
                path.in_role,
                path.out_dac,
                path.taps,
                path.delay_samples,
                path.delay_q16,
                path.polarity,
                path.flags,
                path.coeff_sha256
            );
        }
        for role in &self.roles {
            let _ = writeln!(
                text,
                "role {} {} dac={} delay={} pol={} gain_q15={} peq={} p_taps={} p_sha={} phase_delay={} phase_pol={}",
                role.role_index,
                role.role_id,
                role.out_channel,
                role.delay_samples,
                role.polarity,
                role.gain_q15,
                role.peq_count,
                role.p_taps,
                role.p_coeff_sha256,
                role.phase_delay_samples,
                role.phase_polarity
            );
        }
        text.push_str("host_graph_sha256=");
        text.push_str(&self.host_graph_sha256);
        text.push('\n');
        text
    }

    /// canonical sha256（报告内容 hash）。
    pub fn report_sha256(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }

    /// 确定性 JSON（artifact compile/compile-report.json）。
    pub fn to_json(&self) -> String {
        let mut text = String::new();
        text.push_str("{\n");
        let _ = writeln!(text, "  \"schema\": \"{COMPILE_APPLY_SCHEMA}\",");
        let _ = writeln!(
            text,
            "  \"tool\": {{ \"name\": \"{COMPILE_APPLY_TOOL_NAME}\", \"version\": \"{COMPILE_APPLY_TOOL_VERSION}\" }},"
        );
        let _ = writeln!(text, "  \"compiled\": true,");
        let _ = writeln!(text, "  \"applied\": false,");
        let _ = writeln!(text, "  \"measured\": false,");
        let _ = writeln!(text, "  \"requires_post_apply_remeasurement\": true,");
        let _ = writeln!(
            text,
            "  \"environment\": {{ \"fs\": {}, \"block_size\": {}, \"n_in\": {}, \"container_channels\": {}, \"workers\": {}, \"encoding\": \"{}\" }},",
            self.sample_rate_hz,
            self.block_size,
            self.n_in,
            self.container_channels,
            self.workers,
            self.encoding
        );
        let _ = writeln!(text, "  \"mapping\": {{");
        let _ = writeln!(text, "    \"layout_id\": \"{}\",", self.layout_id);
        let _ = writeln!(text, "    \"mapping_hash\": \"{}\",", self.mapping_hash);
        let _ = writeln!(
            text,
            "    \"overlay_source_sha256\": \"{}\",",
            self.overlay_source_sha256
        );
        let _ = writeln!(text, "    \"binding_hash\": \"{}\",", self.binding_hash);
        let _ = writeln!(text, "    \"role_hash\": \"{}\",", self.role_hash);
        let _ = writeln!(
            text,
            "    \"participants_binding_hash\": \"{}\",",
            self.participants_binding_hash
        );
        let _ = writeln!(text, "    \"stereo_strict\": {},", self.stereo_strict);
        let _ = writeln!(
            text,
            "    \"dense_to_dac\": {},",
            u16_array_json(&self.dense_to_dac)
        );
        let _ = writeln!(
            text,
            "    \"unused_outputs\": {},",
            u16_array_json(&self.unused_outputs)
        );
        let _ = writeln!(
            text,
            "    \"stereo_mask\": {},",
            string_array_json(&self.stereo_mask)
        );
        text.push_str("    \"complete\": true\n");
        text.push_str("  },\n");
        let _ = writeln!(text, "  \"phase\": {{");
        let _ = writeln!(text, "    \"schema\": \"{PHASE_FIT_SCHEMA}\",");
        let _ = writeln!(text, "    \"phase_hash\": \"{}\",", self.phase_hash);
        let _ = writeln!(text, "    \"ops_sha256\": \"{}\",", self.phase_ops_sha256);
        let _ = writeln!(
            text,
            "    \"design_report_sha256\": \"{}\",",
            self.phase_design_report_sha256
        );
        let _ = writeln!(text, "    \"direction_ok\": {},", self.phase_direction_ok);
        let _ = writeln!(
            text,
            "    \"residual_max_samples\": {:.12},",
            self.phase_residual_max_samples
        );
        let _ = writeln!(
            text,
            "    \"tolerance_samples\": {:.12},",
            self.phase_tolerance_samples
        );
        let _ = writeln!(
            text,
            "    \"reference_channel\": \"{}\",",
            self.phase_reference_channel
        );
        text.push_str("    \"channels\": [");
        for (index, channel) in self.phase_channels.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(
                text,
                "{{\"channel\": \"{}\", \"delay_samples\": {}, \"polarity\": {}, \"index\": {}}}",
                channel.channel,
                channel.delay_samples,
                channel.polarity,
                match channel.index {
                    Some(index) => format!("{index}"),
                    None => "null".to_string(),
                }
            );
        }
        text.push_str("]\n  },\n");
        let _ = writeln!(text, "  \"headroom\": {{");
        let _ = writeln!(text, "    \"schema\": \"{PEQ_FIT_SCHEMA}\",");
        let _ = writeln!(text, "    \"headroom_hash\": \"{}\",", self.headroom_hash);
        let _ = writeln!(
            text,
            "    \"report_sha256\": \"{}\",",
            self.headroom_report_sha256
        );
        match self.headroom_measured_peak_dbfs {
            Some(peak) => {
                let _ = writeln!(text, "    \"measured_peak_dbfs\": {peak:.12},");
            }
            None => text.push_str("    \"measured_peak_dbfs\": null,\n"),
        }
        let _ = writeln!(
            text,
            "    \"warnings\": {},",
            string_slice_array_json(&self.d7_warnings)
        );
        text.push_str("    \"pass\": true\n");
        text.push_str("  },\n");
        // crossover 摘要：仅含分频段时出现（无 crossover 的报告 JSON 字节与历史完全一致）。
        if let Some(crossover) = self.crossover.as_ref() {
            text.push_str("  \"crossover\": {\n");
            let _ = writeln!(text, "    \"schema\": \"{CROSSOVER_EVIDENCE_SCHEMA}\",");
            let _ = writeln!(
                text,
                "    \"evidence_sha256\": \"{}\",",
                crossover.evidence_sha256
            );
            let _ = writeln!(
                text,
                "    \"design_report_sha256\": \"{}\",",
                crossover.design_report_sha256
            );
            let _ = writeln!(text, "    \"kind\": \"{}\",", crossover.kind);
            let _ = writeln!(text, "    \"fc_hz\": {:.12},", crossover.fc_hz);
            let _ = writeln!(text, "    \"taps\": {},", crossover.taps);
            let _ = writeln!(text, "    \"diag_count\": {},", crossover.diag_count);
            let _ = writeln!(text, "    \"p_taps_max\": {},", crossover.p_taps_max);
            text.push_str("    \"roles\": [");
            for (index, (role, branch)) in crossover.roles.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                let _ = write!(
                    text,
                    "{{\"role_id\": \"{role}\", \"branch\": \"{branch}\"}}"
                );
            }
            text.push_str("]\n");
            text.push_str("  },\n");
        }
        let _ = writeln!(text, "  \"artifact\": {{");
        let _ = writeln!(text, "    \"schema\": \"{ARTIFACT_SCHEMA}\",");
        let _ = writeln!(text, "    \"status\": \"{}\",", self.artifact_status);
        let _ = writeln!(text, "    \"artifact_hash\": \"{}\",", self.artifact_hash);
        let _ = writeln!(
            text,
            "    \"manifest_sha256\": \"{}\",",
            self.artifact_manifest_sha256
        );
        let _ = writeln!(
            text,
            "    \"wav_aggregate_sha256\": \"{}\",",
            self.artifact_wav_aggregate_sha256
        );
        let _ = writeln!(
            text,
            "    \"solution_hash\": \"{}\",",
            self.artifact_solution_hash
        );
        let _ = writeln!(
            text,
            "    \"design_plant_hash\": \"{}\",",
            self.artifact_design_plant_hash
        );
        let _ = writeln!(
            text,
            "    \"target_sha256\": \"{}\",",
            self.artifact_target_sha256
        );
        let _ = writeln!(
            text,
            "    \"baseline_hash\": \"{}\",",
            self.artifact_baseline_hash
        );
        let _ = writeln!(
            text,
            "    \"host_graph_sha256\": \"{}\"",
            self.host_graph_sha256
        );
        text.push_str("  },\n");
        let _ = writeln!(text, "  \"plan\": {{");
        let _ = writeln!(text, "    \"graph_id\": \"{}\",", hex32(&self.graph_id));
        let _ = writeln!(text, "    \"plan_sha256\": \"{}\",", self.plan_sha256);
        let _ = writeln!(text, "    \"plan_version\": {},", self.plan_version);
        let _ = writeln!(text, "    \"path_count\": {},", self.path_count);
        let _ = writeln!(text, "    \"diag_count\": {},", self.diag_count);
        let _ = writeln!(text, "    \"role_count\": {},", self.role_count);
        let _ = writeln!(text, "    \"low_band_count\": {},", self.low_band_count);
        let _ = writeln!(text, "    \"d_samples\": {},", self.d_samples);
        let _ = writeln!(text, "    \"l_g\": {},", self.l_g);
        let _ = writeln!(text, "    \"l_p\": {},", self.l_p);
        let _ = writeln!(text, "    \"max_taps\": {},", self.max_taps);
        let _ = writeln!(
            text,
            "    \"total_coeff_bytes\": {},",
            self.total_coeff_bytes
        );
        let _ = writeln!(text, "    \"chosen_form\": \"{}\",", self.facts.chosen_form);
        let _ = writeln!(
            text,
            "    \"selection_reason\": \"{}\",",
            self.facts.selection_reason
        );
        let _ = writeln!(text, "    \"fused_len_gp\": {},", self.facts.fused_len_gp);
        let _ = writeln!(text, "    \"full_path_len\": {},", self.facts.full_path_len);
        let _ = writeln!(
            text,
            "    \"fused_feasible\": {},",
            self.facts.fused_feasible
        );
        let _ = writeln!(text, "    \"route_count\": {},", self.facts.route_count);
        let _ = writeln!(
            text,
            "    \"factorized_macs_per_sample\": {},",
            self.facts.factorized_macs_per_sample
        );
        let _ = writeln!(
            text,
            "    \"fused_macs_per_sample\": {},",
            self.facts.fused_macs_per_sample
        );
        let _ = writeln!(
            text,
            "    \"saved_macs_per_sample\": {},",
            self.facts.saved_macs_per_sample
        );
        let _ = writeln!(
            text,
            "    \"factorized_flops_per_second\": {:.6},",
            self.facts.factorized_flops_per_second
        );
        let _ = writeln!(
            text,
            "    \"fused_flops_per_second\": {:.6},",
            self.facts.fused_flops_per_second
        );
        let _ = writeln!(text, "    \"taps_limit\": {},", self.facts.taps_limit);
        let _ = writeln!(
            text,
            "    \"extended_taps_admitted\": {},",
            self.facts.extended_taps_admitted
        );
        let _ = writeln!(
            text,
            "    \"coeff_budget_bytes\": {},",
            self.facts.coeff_budget_bytes
        );
        let _ = writeln!(text, "    \"plan_bytes\": {},", self.total_coeff_bytes);
        text.push_str("    \"compiled\": true\n");
        text.push_str("  },\n");
        // plan ABI v2 IIR 摘要：仅在有 T64 PEQ 段时出现（无 PEQ 的 JSON 字节与历史一致）。
        if let Some(iir) = self.iir.as_ref() {
            text.push_str("  \"iir\": {\n");
            let _ = writeln!(text, "    \"schema\": \"tspi-plan-iir/v2\",");
            let _ = writeln!(text, "    \"abi_version\": {},", iir.abi_version);
            let _ = writeln!(text, "    \"role_count\": {},", iir.role_count);
            let _ = writeln!(text, "    \"section_count\": {},", iir.section_count);
            let _ = writeln!(text, "    \"table_offset\": {},", iir.table_offset);
            let _ = writeln!(text, "    \"table_sha256\": \"{}\",", iir.table_sha256);
            text.push_str("    \"roles\": [");
            for (index, (role, sections)) in iir.roles.iter().enumerate() {
                if index > 0 {
                    text.push(',');
                }
                let _ = write!(
                    text,
                    "{{\"role_id\": \"{role}\", \"section_count\": {sections}}}"
                );
            }
            text.push_str("]\n");
            text.push_str("  },\n");
        }
        text.push_str("  \"paths\": [");
        for (index, path) in self.paths.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push('\n');
            let _ = write!(
                text,
                "    {{\"out_index\": {}, \"in_index\": {}, \"out_role\": \"{}\", \"in_role\": \"{}\", \"out_dac\": {}, \"taps\": {}, \"delay_samples\": {}, \"delay_q16\": {}, \"polarity\": {}, \"flags\": {}, \"diag_baseline\": {}, \"fused\": {}, \"coeff_sha256\": \"{}\", \"peak_abs\": {:.12}}}",
                path.out_index,
                path.in_index,
                path.out_role,
                path.in_role,
                path.out_dac,
                path.taps,
                path.delay_samples,
                path.delay_q16,
                path.polarity,
                path.flags,
                path.diag_baseline,
                path.fused,
                path.coeff_sha256,
                path.peak_abs
            );
        }
        text.push_str("\n  ],\n");
        text.push_str("  \"roles\": [");
        for (index, role) in self.roles.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push('\n');
            let _ = write!(
                text,
                "    {{\"role_index\": {}, \"role_id\": \"{}\", \"out_channel\": {}, \"delay_samples\": {}, \"polarity\": {}, \"gain_q15\": {}, \"peq_count\": {}, \"p_taps\": {}, \"p_peak_abs\": {:.12}, \"p_coeff_sha256\": \"{}\", \"phase_delay_samples\": {}, \"phase_polarity\": {}, \"delay_polarity_only\": {}}}",
                role.role_index,
                role.role_id,
                role.out_channel,
                role.delay_samples,
                role.polarity,
                role.gain_q15,
                role.peq_count,
                role.p_taps,
                role.p_peak_abs,
                role.p_coeff_sha256,
                role.phase_delay_samples,
                role.phase_polarity,
                role.delay_polarity_only
            );
        }
        text.push_str("\n  ],\n");
        let _ = writeln!(
            text,
            "  \"host_graph_sha256\": \"{}\",",
            self.host_graph_sha256
        );
        text.push_str("  \"checks\": {\n");
        let _ = writeln!(text, "    \"mapping_complete\": true,");
        let _ = writeln!(text, "    \"phase_direction_ok\": true,");
        let _ = writeln!(text, "    \"headroom_pass\": true,");
        let _ = writeln!(text, "    \"artifact_ready\": true,");
        let _ = writeln!(text, "    \"plan_verified\": true,");
        let _ = writeln!(text, "    \"camilla_involved\": false");
        text.push_str("  },\n");
        text.push_str("  \"limits\": [\n");
        text.push_str("    \"design/compile side only: not applied, not measured; T3 post-apply remeasurement is required for CONFIRMED\",\n");
        text.push_str("    \"fused/factorized FLOPs are model estimates, not measurements\",\n");
        text.push_str("    \"plan ABI v1.0 has no delay_q16 (GAP-T66-2), no role IIR coefficient area (GAP-T66-3), role gain_q15 <= 1.0 (GAP-T66-4)\"\n");
        text.push_str("  ]\n");
        text.push_str("}\n");
        text
    }
}

/// 编译产物（plan 字节 + 表 + 记录 + 报告）。
#[derive(Clone, Debug)]
pub struct CompiledPlan {
    /// plan 头（plan_version=0，等 T20 registry 分配）。
    pub header: TspiPlanHeaderV1,
    /// 规范化 graph_id（SHA-256）。
    pub graph_id: [u8; 32],
    /// 完整 plan 字节（T20 parse 可直接吃）。
    pub plan_bytes: Vec<u8>,
    /// path 表（按 (out_index,in_index) 升序）。
    pub paths: Vec<TspiPathV1>,
    /// diag 表（按 out_index 升序；fused 形态为空）。
    pub diags: Vec<TspiDiagFirV1>,
    /// role 表（按 role_id 升序）。
    pub roles: Vec<TspiRoleV1>,
    /// 系数区（含 64B 对齐零填充；总字节 = header.total_coeff_bytes）。
    pub coeffs: Vec<u8>,
    /// ctl_meta（role 名区；graph_id 哈希输入）。
    pub ctl_meta: Vec<u8>,
    /// 系数 blob（按表顺序拼接，不含 64B padding；graph_id 哈希输入）。
    pub coeff_blobs: Vec<Vec<u8>>,
    /// plan ABI v2 per-role IIR 表项（按 role_id 升序；空 = v1 plan）。
    pub iir_entries: Vec<TspiRoleIirV2>,
    /// plan ABI v2 IIR 表字节（16 B 表头 + 表项；空 = 无 IIR）。
    pub iir_table: Vec<u8>,
    /// IIR 系数 blob（按表项顺序；graph_id / 表 CRC 输入）。
    pub iir_blobs: Vec<Vec<u8>>,
    /// 编译报告。
    pub report: CompileReport,
    /// delay/polarity 编码。
    pub encoding: DelayPolarityEncoding,
    /// D7 warning。
    pub d7_warnings: Vec<&'static str>,
    /// 实验模式（**OFF = 生产产物**；active 时本 plan 一律 EXPERIMENT/NON_ACCEPTABLE）。
    pub experiment: ExperimentMode,
    /// 实验旁路记录（生产门绕过明细；只作证据）。
    pub experiment_notes: Vec<String>,
}

impl CompiledPlan {
    /// 拥有型 token（T20 registry admit 入口）。
    pub fn token(&self) -> PlanToken {
        PlanToken::new(PlanBuf::new(self.plan_bytes.clone()), self.graph_id)
    }

    /// 用指定字节（负向测试/宿主重打包）构造 token（graph_id 用本编译结论）。
    pub fn token_from_bytes(&self, bytes: Vec<u8>) -> PlanToken {
        PlanToken::new(PlanBuf::new(bytes), self.graph_id)
    }

    /// 只读 plan view（T18 admission 输入）。
    pub fn view(&self) -> tspi_dsp_core::rt::PlanView<'_> {
        tspi_dsp_core::rt::PlanView {
            header: &self.header,
            paths: &self.paths,
            diags: &self.diags,
            roles: &self.roles,
            coeffs: &self.coeffs,
        }
    }

    /// graph_id hex（64 字符）。
    pub fn graph_id_hex(&self) -> String {
        hex32(&self.graph_id)
    }

    /// plan 字节 sha256。
    pub fn plan_sha256(&self) -> String {
        sha256_hex(&self.plan_bytes)
    }

    /// T54 host_graph_sha256 回填值（= graph_id hex）。
    pub fn host_graph_sha256(&self) -> String {
        self.graph_id_hex()
    }

    /// 报告 JSON（实验模式下追加 verdict/acceptability/applied=false 等明确字段）。
    pub fn report_json(&self) -> String {
        let base = self.report.to_json();
        if !self.experiment.is_active() {
            return base;
        }
        // 生产报告里 \"checks.artifact_ready\" 只在 artifact READY 时成立；实验模式必须如实改 false。
        let patched = base.replace("\"artifact_ready\": true", "\"artifact_ready\": false");
        let block = format!(
            "  \"experiment\": {},\n  \"gates_bypassed\": {}",
            self.experiment.markers_json(),
            string_array_json(&self.experiment_notes)
        );
        inject_top_level_block(&patched, &block)
    }

    /// 实验 envelope artifact（**仅实验模式**；生产路径返回 None，不产任何新 artifact）。
    ///
    /// 内容固定包含 verdict=EXPERIMENT / acceptability=NON_ACCEPTABLE / applied=false /
    /// certifiable=false / scope=<single-point|multipoint-pilot 实验作用域> + plan 事实 + 绕过的生产门。
    pub fn experiment_artifact(&self) -> Option<CompileArtifact> {
        if !self.experiment.is_active() {
            return None;
        }
        let bytes = self.experiment_envelope_json().into_bytes();
        Some(CompileArtifact {
            kind: ArtifactKind::Other("experiment_envelope".to_string()),
            path: EXPERIMENT_ENVELOPE_ARTIFACT_PATH.to_string(),
            sha256: sha256_hex(&bytes),
            bytes,
        })
    }

    /// 实验 envelope JSON（确定性字节）。
    pub fn experiment_envelope_json(&self) -> String {
        let paths = self.report.paths.len() as u64;
        let diag_paths = self
            .report
            .paths
            .iter()
            .filter(|record| record.diag_baseline)
            .count() as u64;
        let offdiag_paths = paths.saturating_sub(diag_paths);
        let mut text = String::new();
        text.push_str("{\n");
        let _ = writeln!(text, "  \"schema\": \"{EXPERIMENT_SCHEMA}\",");
        let _ = writeln!(text, "  \"verdict\": \"{EXPERIMENT_VERDICT}\",");
        let _ = writeln!(text, "  \"acceptability\": \"{EXPERIMENT_ACCEPTABILITY}\",");
        let _ = writeln!(text, "  \"applied\": false,");
        let _ = writeln!(text, "  \"certifiable\": false,");
        let _ = writeln!(text, "  \"scope\": \"{}\",", self.experiment.scope());
        let _ = writeln!(
            text,
            "  \"experiment\": {},",
            self.experiment.markers_json()
        );
        let _ = writeln!(
            text,
            "  \"gates_bypassed\": {},",
            string_array_json(&self.experiment_notes)
        );
        // T64 修正是否真的进了 plan：plan ABI v2 per-role IIR 表存在 <=> T64 PEQ 段已承载。
        // 无 PEQ 时字符串与历史完全一致（false）；有 PEQ 时才追加 plan_abi/iir_sections 字段。
        let t64_correction_in_plan = !self.iir_entries.is_empty();
        let _ = writeln!(
            text,
            "  \"t64_correction_in_plan\": {t64_correction_in_plan},"
        );
        if t64_correction_in_plan {
            let _ = writeln!(text, "  \"plan_abi\": {},", self.header.abi_version);
            let _ = writeln!(
                text,
                "  \"iir_sections\": {},",
                self.iir_entries
                    .iter()
                    .map(|entry| entry.section_count as u32)
                    .sum::<u32>()
            );
        }
        let _ = writeln!(
            text,
            "  \"d7_warnings\": {},",
            string_array_json(
                &self
                    .d7_warnings
                    .iter()
                    .map(|warning| warning.to_string())
                    .collect::<Vec<String>>()
            )
        );
        let _ = writeln!(
            text,
            "  \"artifact_status_at_compile\": \"{}\",",
            self.report.artifact_status
        );
        let _ = writeln!(text, "  \"plan\": {{");
        let _ = writeln!(text, "    \"plan_sha256\": \"{}\",", self.plan_sha256());
        let _ = writeln!(text, "    \"graph_id\": \"{}\",", self.graph_id_hex());
        let _ = writeln!(text, "    \"plan_bytes\": {},", self.plan_bytes.len());
        let _ = writeln!(text, "    \"path_count\": {paths},");
        let _ = writeln!(text, "    \"diag_paths\": {diag_paths},");
        let _ = writeln!(text, "    \"offdiag_paths\": {offdiag_paths},");
        let _ = writeln!(text, "    \"l_g\": {},", self.report.l_g);
        let _ = writeln!(text, "    \"l_p\": {},", self.report.l_p);
        let _ = writeln!(text, "    \"max_taps\": {},", self.report.max_taps);
        let _ = writeln!(
            text,
            "    \"chosen_form\": \"{}\",",
            self.report.facts.chosen_form
        );
        let _ = writeln!(text, "    \"layout_id\": \"{}\",", self.report.layout_id);
        let _ = writeln!(
            text,
            "    \"participants\": {}",
            u16_array_json(&self.report.dense_to_dac)
        );
        text.push_str("  },\n");
        text.push_str("  \"limits\": [\n");
        text.push_str("    \"EXPERIMENT ONLY: 本 plan 不是交付/验收产物（NON_ACCEPTABLE）\",\n");
        if t64_correction_in_plan {
            text.push_str("    \"plan 承载 injected G + role delay/polarity + T64 PEQ per-role IIR 表（plan ABI v2）；T64 target FIR 仍由 P 行承载（本 plan 仍 EXPERIMENT/NON_ACCEPTABLE）\",\n");
        } else {
            text.push_str("    \"plan 只承载 injected G + role delay/polarity；T64 PEQ/IIR 与 target FIR 修正不进 plan（plan ABI v1.0 无 IIR 区）\",\n");
        }
        if self.experiment.multipoint_pilot {
            text.push_str("    \"T54 artifact 非 READY（多点 pilot 点数低于 T6 最低 6 design+3 holdout 恒 INCOMPLETE）；artifact 状态门被显式实验 flag 绕过\",\n");
            text.push_str("    \"MULTIPOINT_PILOT_NOT_CERTIFIABLE：design>=2 + 独立 validation>=1 只用于接线/预测评估，不产生 T6/holdout 通过结论\",\n");
        } else {
            text.push_str("    \"T54 artifact 非 READY（单点 P<M 恒 INCOMPLETE）；artifact 状态门被显式实验 flag 绕过\",\n");
            text.push_str("    \"SINGLE_POINT_NOT_SPATIAL：不产生任何空间/多点/MIMO 结论\",\n");
        }
        text.push_str("    \"applied=false：本文件生成时未下发；实际 apply 结果另见 host-report / job summary\",\n");
        text.push_str("    \"不得把本产物写成 READY/PASS/CERTIFIED\"\n");
        text.push_str("  ]\n");
        text.push_str("}\n");
        text
    }

    /// 报告 sha256。
    pub fn report_sha256(&self) -> String {
        self.report.report_sha256()
    }

    /// 待发布 artifact（plan.bin + compile-report.json）。
    pub fn artifacts(&self) -> CompileArtifacts {
        let plan_bytes = self.plan_bytes.clone();
        let report_bytes = self.report_json().into_bytes();
        let report_sha256 = sha256_hex(&report_bytes);
        CompileArtifacts {
            plan: CompileArtifact {
                kind: ArtifactKind::FirBundle,
                path: PLAN_ARTIFACT_PATH.to_string(),
                sha256: sha256_hex(&plan_bytes),
                bytes: plan_bytes,
            },
            report: CompileArtifact {
                kind: ArtifactKind::DesignReport,
                path: COMPILE_REPORT_ARTIFACT_PATH.to_string(),
                sha256: report_sha256,
                bytes: report_bytes,
            },
        }
    }
}

/// 一个待发布 artifact（内容 + sha256）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileArtifact {
    /// 类别（T60 job 登记用）。
    pub kind: ArtifactKind,
    /// 相对路径（job artifacts_dir 语义）。
    pub path: String,
    /// 内容 sha256。
    pub sha256: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
}

/// 编译 artifact 集合（plan.bin + compile-report.json）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileArtifacts {
    /// 打包 plan 字节。
    pub plan: CompileArtifact,
    /// 编译报告 JSON。
    pub report: CompileArtifact,
}

impl CompileArtifacts {
    /// 直接写到目录（自动建父目录；确定性字节）。
    pub fn write_to_dir(&self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let entries = [&self.plan, &self.report];
        let mut written = Vec::with_capacity(entries.len());
        for entry in entries {
            let path = dir.join(&entry.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &entry.bytes)?;
            written.push(path);
        }
        Ok(written)
    }

    /// 通过 T60 StageCtx 登记 artifact（同 job 重跑幂等；内容变化由 job.rs 拒收）。
    pub fn write_stage_artifacts(
        &self,
        ctx: &StageCtx<'_>,
    ) -> Result<Vec<ArtifactRecord>, StageFailure> {
        let entries = [&self.plan, &self.report];
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            records.push(ctx.write_artifact(
                entry.kind.clone(),
                &entry.path,
                &entry.bytes,
                &entry.sha256,
            )?);
        }
        Ok(records)
    }
}

/// 在顶层 JSON 对象闭合前注入一个已有字段块（`block` 不带行尾逗号）。
///
/// 只用于实验模式给 compile-report.json 增加 verdict/acceptability 字段；生产路径不调用，
/// 因此生产报告字节完全不变。
fn inject_top_level_block(base: &str, block: &str) -> String {
    let trimmed = base.trim_end();
    let Some(rest) = trimmed.strip_suffix('}') else {
        return base.to_string();
    };
    let need_comma = !rest.trim_end().ends_with('{');
    let mut out = String::with_capacity(base.len() + block.len() + 8);
    out.push_str(rest);
    if need_comma {
        out.push(',');
    }
    out.push('\n');
    out.push_str(block);
    out.push('\n');
    out.push_str("}\n");
    out
}

/// JSON 数组（u16）。
fn u16_array_json(values: &[u16]) -> String {
    let mut text = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        let _ = write!(text, "{value}");
    }
    text.push(']');
    text
}

/// JSON 数组（&str）。
fn string_array_json(values: &[String]) -> String {
    let mut text = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        let _ = write!(text, "\"{value}\"");
    }
    text.push(']');
    text
}

/// JSON 数组（&'static str）。
fn string_slice_array_json(values: &[&'static str]) -> String {
    let mut text = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        let _ = write!(text, "\"{value}\"");
    }
    text.push(']');
    text
}

// ---------------------------------------------------------------------------
// 编译入口
// ---------------------------------------------------------------------------

/// P/G 每 path FIR 编译入口（T12 图 -> T20 兼容 plan 字节 -> 自校验 -> 报告）。
///
/// 步骤：请求校验 -> G dense/ P rows（含 delay/polarity 编码）-> phase 方向校验 ->
/// T12 compile（factorized+fused 成本/选择）-> 表/系数区打包 -> graph_id/CRC -> 自校验 ->
/// 确定性报告。任何一步失败返回稳定错误码，绝不产出半成品 plan。
pub fn compile_plan(req: &CompileApplyRequest<'_>) -> CaResult<CompiledPlan> {
    req.validate()?;
    let participants = req.participant_role_indices()?;
    let m = participants.len();
    if m > tspi_dsp_core::rt::MAX_LOW_BAND_PARTICIPANTS {
        return Err(ca_err(
            E_CA_ROLES,
            "mapping.dense_to_dac",
            "participants M 超过 MAX_LOW_BAND_PARTICIPANTS",
        ));
    }

    // 1) G dense（M x M x L_G；不含 baseline）与 P rows（编码后 O x L_P）。
    let g_dense = build_dense_g(req, m)?;
    let p_rows = build_p_rows(req)?;
    verify_phase_direction(req, &p_rows)?;
    // F-35：D7 硬门用真正下发的编码 P 行重算（并与绑定的 T64 报告字节交叉绑定），
    // 不再只信 HeadroomEvidence 的申报数字；放在 T12 编译/打包前，失败不产出半成品 plan。
    let d7_warnings = verify_d7_from_encoded_p_rows(req, &p_rows)?;

    // 2) T12 编译（identity A_map、cost/选择/长度）。
    let a = vec![1.0_f64; req.n_in as usize];
    let src_of_role: Vec<u16> = req
        .roles
        .iter()
        .map(|role| role.source_channel.unwrap_or(COMPILE_NO_SOURCE))
        .collect();
    let scatter: Vec<u16> = req.roles.iter().map(|role| role.out_channel).collect();
    let input = CompileInput {
        n_src: req.n_in,
        n_role: req.roles.len() as u16,
        container_channels: req.container_channels,
        d_samples: req.low_band_d,
        participants: &participants,
        src_of_role: &src_of_role,
        scatter: &scatter,
        a: &a,
        l_a: 1,
        g: &g_dense,
        l_g: req.l_g,
        p: &p_rows,
        l_p: req.l_p,
    };
    let t12_options = CompileOptions {
        sample_rate_hz: req.sample_rate_hz,
        block_size: req.block_size,
        max_taps: req.options.max_taps,
        extended_taps_admitted: req.options.extended_taps_admitted,
        coeff_budget_bytes: req.options.coeff_budget_bytes,
    };
    let plans = compile(&input, &t12_options)
        .map_err(|status| ca_status(E_CA_COMPILE, "compile", status))?;

    // 3) 形态选择（显式 force 或 T12 成本报告；禁止静默选择）。
    let chosen = match req.options.force_form {
        Some(FusionForm::Factorized) => FusionForm::Factorized,
        Some(FusionForm::FusedGp) => {
            if plans.fused.is_none() {
                return Err(ca_err(
                    E_CA_TAPS,
                    "options.force_form",
                    format!(
                        "fused 不可行（T12 reason={}）：禁止截尾/假融合",
                        selection_reason_str(plans.report.reason)
                    ),
                ));
            }
            FusionForm::FusedGp
        }
        None => plans.report.chosen,
    };

    // 4) 打包表 + 系数区 + graph_id/CRC。
    let packed = pack_tables(req, &plans, chosen, &p_rows)?;
    let (mut header, graph_id) = pack_plan_header(req, &packed, m)?;
    let plan_bytes = pack_plan_bytes(req, &packed, &mut header, &graph_id)?;

    // 5) 报告 + 自校验（拒坏图）。
    let report = build_compile_report(
        req,
        &plans,
        chosen,
        &packed,
        &header,
        &graph_id,
        &plan_bytes,
    )?;
    let compiled = CompiledPlan {
        header,
        graph_id,
        plan_bytes,
        paths: packed.paths,
        diags: packed.diags,
        roles: packed.roles,
        coeffs: packed.coeff_region,
        ctl_meta: build_ctl_meta(req.roles),
        coeff_blobs: packed.coeff_blobs,
        iir_entries: packed.iir_entries,
        iir_table: packed.iir_table,
        iir_blobs: packed.iir_blobs,
        report,
        encoding: req.encoding,
        d7_warnings,
        experiment: req.experiment,
        experiment_notes: req.experiment_notes.clone(),
    };
    compiled.verify()?;
    Ok(compiled)
}

// ---------------------------------------------------------------------------
// 输入构造：G dense / P rows / phase 方向
// ---------------------------------------------------------------------------

/// G dense：M x M x L_G（行=out dense，列=in dense；**不含** delta_D baseline）。
fn build_dense_g(req: &CompileApplyRequest<'_>, m: usize) -> CaResult<Vec<f64>> {
    let l_g = req.l_g as usize;
    let total = m
        .checked_mul(m)
        .and_then(|value| value.checked_mul(l_g))
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "l_g", "M*M*L_G 溢出"))?;
    let mut g = vec![0.0_f64; total];
    let mut seen = vec![false; m * m];
    for path in req.paths {
        let out = path.out_dense as usize;
        let in_ = path.in_dense as usize;
        if out >= m || in_ >= m {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                format!("paths[{}][{}]", path.out_dense, path.in_dense),
                "dense 索引越界（M 见 mapping.dense_to_dac）",
            ));
        }
        path.validate_lg(Some(req.l_g))?;
        let key = out * m + in_;
        if seen[key] {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                format!("paths[{}][{}]", path.out_dense, path.in_dense),
                "G cell 重复（dense 行列必须唯一）",
            ));
        }
        seen[key] = true;
        let start = key * l_g;
        g[start..start + l_g].copy_from_slice(&path.coeffs);
    }
    Ok(g)
}

/// P rows：O x L_P（delay/polarity 按编码折入或留在 role 表）。
fn build_p_rows(req: &CompileApplyRequest<'_>) -> CaResult<Vec<f64>> {
    let l_p = req.l_p as usize;
    let n_role = req.roles.len();
    let mut rows = vec![0.0_f64; n_role * l_p];
    for role in req.roles {
        // F-16 唯一延迟定义守卫（两种编码都适用）：线性相位 P FIR 的 GD_p>0 时
        // role.delay_samples(D_i) 不得非 0，否则总延迟 = GD_p + D_i 双扣；一律拒收。
        reject_double_count(role)?;
        let offset = role.role_index as usize * l_p;
        let encoded = match req.encoding {
            DelayPolarityEncoding::RoleMetadata => role.p_fir.clone(),
            DelayPolarityEncoding::CoeffEmbedded => {
                convolve_delta(&role.p_fir, role.delay_samples, role.polarity)
            }
        };
        if encoded.len() > l_p {
            return Err(ca_err(
                E_CA_TAPS,
                format!("roles[{}].p_fir", role.role_index),
                "编码后 P FIR 长度超过 L_P（禁止截尾）",
            ));
        }
        rows[offset..offset + encoded.len()].copy_from_slice(&encoded);
    }
    Ok(rows)
}

/// 与单个 delta 卷积（= 因果延迟 D + 极性乘子；用于 CoeffEmbedded）。
fn convolve_delta(fir: &[f64], delay_samples: u32, polarity: i8) -> Vec<f64> {
    let mut out = vec![0.0_f64; fir.len() + delay_samples as usize];
    let sign = polarity as f64;
    for (index, value) in fir.iter().enumerate() {
        out[index + delay_samples as usize] += value * sign;
    }
    out
}

/// 有效支撑首/末非零索引（阈值同 `is_identity_fir`）；全零 FIR 返回 None。
fn nonzero_support(fir: &[f64]) -> Option<(usize, usize)> {
    let mut first = None;
    let mut last = None;
    for (index, value) in fir.iter().enumerate() {
        if value.abs() > 1e-12 {
            if first.is_none() {
                first = Some(index);
            }
            last = Some(index);
        }
    }
    match (first, last) {
        (Some(first), Some(last)) => Some((first, last)),
        _ => None,
    }
}

/// 线性相位 FIR 的固有群延迟（samples）；非对称 FIR 无常群延迟，返回 None。
///
/// **唯一延迟定义（F-16）**：线性相位 FIR 满足 h[n]=h[N-1-n]，群延迟为对称中心
/// `GD_p = (首个非零索引 + 末个非零索引)/2 = (taps-1)/2`（奇 taps 整数、偶 taps 半整数）。
/// 该延迟**只由系数承载**；编译期 `role.delay_samples`（T65 D_i）是额外整数延迟，
/// 总延迟 = GD_p + D_i —— 二者同时非 0 即双扣，[`reject_double_count`] 一律拒收。
///
/// 非对称 FIR（如 `[1.0, 0.05]`）只有频变群延迟，不存在这句公式；T64 目标 FIR 恒为
/// 线性相位（见 peq_fit 目标 FIR 设计），故本检查覆盖实际链路。支撑两端的对称裁剪
/// 不改变中心（(first+last)/2 不变），因此系数首尾为 0 也不影响判定。
fn linear_phase_group_delay_samples(fir: &[f64]) -> Option<f64> {
    let (first, last) = nonzero_support(fir)?;
    if first == last {
        return Some(first as f64);
    }
    let peak = fir[first..=last]
        .iter()
        .fold(0.0_f64, |acc, value| acc.max(value.abs()));
    let tolerance = 1e-9 * peak;
    for offset in 0..=(last - first) / 2 {
        if (fir[first + offset] - fir[last - offset]).abs() > tolerance {
            return None;
        }
    }
    Some((first as f64 + last as f64) / 2.0)
}

/// F-16 双扣守卫：线性相位 P FIR 固有群延迟非 0 且 role 整数延迟 D_i 非 0 → 稳定拒收。
///
/// 覆盖 `RoleMetadata`（role 表延迟）与 `CoeffEmbedded`（`convolve_delta` 再叠加 D_i）
/// 两种编码；错误码稳定为 E_CA_DOUBLE_COUNT。定义落地前不接受「P FIR 固有延迟 + D_i」并存
/// （单点 delta 的 GD 即其下标，多抽头线性相位 FIR 的 GD 即 (taps-1)/2）。
fn reject_double_count(role: &RoleCompileInput) -> CaResult<()> {
    if role.delay_samples == 0 {
        return Ok(());
    }
    if let Some(group_delay) = linear_phase_group_delay_samples(&role.p_fir) {
        if group_delay > 0.0 {
            return Err(ca_err(
                E_CA_DOUBLE_COUNT,
                format!("roles[{}].p_fir", role.role_index),
                format!(
                    "P FIR 为线性相位 FIR（固有群延迟 {group_delay} samples，已含在系数里），\
                     与 role delay_samples={} 并存会双扣：总延迟 = GD_p + D_i；\
                     定义落地前只允许 D_i=0 或 GD_p=0",
                    role.delay_samples
                ),
            ));
        }
    }
    Ok(())
}

fn to_t40_polarity(polarity: i8) -> PhasePolarity {
    if polarity >= 0 {
        PhasePolarity::Normal
    } else {
        PhasePolarity::Invert
    }
}

/// phase 方向校验（T65 §4 / T40 因果实现）：方向反了会把相对错位放大而不是抵消。
///
/// - F-16 唯一延迟定义：线性相位 P FIR 的 GD_p=(taps-1)/2 已含在系数里，
///   GD_p>0 与 role 延迟 D_i>0 不得并存（总延迟 = GD_p + D_i；双扣一律
///   E_CA_DOUBLE_COUNT，见 [`reject_double_count`]）；
/// - CoeffEmbedded + 纯 delay/polarity 通路：编码系数必须逐样本等于 T40 design_integer
///   的 delta(D)·polarity（H=e^-jwD，索引 0..D 全零 = 因果延迟）；
/// - 任意编码下 role 延迟上限由请求校验（<=floor(0.2*fs)）。
fn verify_phase_direction(req: &CompileApplyRequest<'_>, p_rows: &[f64]) -> CaResult<()> {
    let l_p = req.l_p as usize;
    for role in req.roles {
        // F-16：与 build_p_rows 同一守卫，打包后自校验再兜底一次。
        reject_double_count(role)?;
        match req.encoding {
            DelayPolarityEncoding::CoeffEmbedded => {
                if !is_identity_fir(&role.p_fir) {
                    continue;
                }
                let spec =
                    IntegerDelaySpec::new(req.sample_rate_hz as f64, role.delay_samples as u64)
                        .with_polarity(to_t40_polarity(role.polarity));
                let design = design_integer(&spec).map_err(|error| {
                    ca_err(
                        E_CA_PHASE_DIRECTION,
                        format!("roles[{}]", role.role_index),
                        format!("T40 整数延迟设计失败：{error}"),
                    )
                })?;
                let expected = delta_fir(role.delay_samples, role.polarity);
                if !firs_equal(&design.coefficients, &expected) {
                    return Err(ca_err(
                        E_CA_PHASE_DIRECTION,
                        format!("roles[{}].delay_samples", role.role_index),
                        "T40 因果延迟参考（delta(D)*polarity）与本地编码不一致：禁止反向/提前",
                    ));
                }
                let offset = role.role_index as usize * l_p;
                let row = &p_rows[offset..offset + expected.len()];
                if !firs_equal(row, &expected) {
                    return Err(ca_err(
                        E_CA_PHASE_DIRECTION,
                        format!("roles[{}].p_fir", role.role_index),
                        "CoeffEmbedded 纯 delay/polarity 行未按因果 delta(D) 编码（方向错误）",
                    ));
                }
            }
            DelayPolarityEncoding::RoleMetadata => {
                let offset = role.role_index as usize * l_p;
                let row = &p_rows[offset..offset + role.p_fir.len()];
                if !firs_equal(row, &role.p_fir) {
                    return Err(ca_err(
                        E_CA_INTERNAL,
                        format!("roles[{}]", role.role_index),
                        "RoleMetadata P 行与输入 FIR 不一致",
                    ));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 表 / 系数区 / plan 字节打包（T20 parse_plan_arena 兼容）
// ---------------------------------------------------------------------------

/// path 表打包中间态。
#[derive(Clone, Debug)]
struct RawPath {
    out_index: u16,
    in_index: u16,
    taps: u32,
    delay_samples: u32,
    delay_q16: u32,
    polarity: i8,
    flags: u8,
    diag_baseline: bool,
    fused: bool,
    coeffs: Vec<f32>,
}

/// diag 表打包中间态。
#[derive(Clone, Debug)]
struct RawDiag {
    out_index: u16,
    taps: u32,
    delay_samples: u32,
    flags: u16,
    coeffs: Vec<f32>,
}

/// 打包结果（表 + 系数区 + 报告记录 + plan ABI v2 IIR 表）。
struct PackedTables {
    paths: Vec<TspiPathV1>,
    diags: Vec<TspiDiagFirV1>,
    roles: Vec<TspiRoleV1>,
    coeff_region: Vec<u8>,
    coeff_blobs: Vec<Vec<u8>>,
    path_records: Vec<CompiledPathRecord>,
    role_records: Vec<CompiledRoleRecord>,
    max_taps: u32,
    /// plan ABI v2 IIR 表项（按 role_id 升序；空 = v1 plan，字节与历史一致）。
    iir_entries: Vec<TspiRoleIirV2>,
    /// 每 IIR 表项的 f32 系数 blob（与 iir_entries 同序；graph_id/表 CRC 输入）。
    iir_blobs: Vec<Vec<u8>>,
    /// 序列化后的 IIR 表（16 B 表头 + 表项；空 = 无 IIR）。
    iir_table: Vec<u8>,
}

pub(crate) fn build_ctl_meta(roles: &[RoleCompileInput]) -> Vec<u8> {
    let mut meta: Vec<u8> = Vec::new();
    for role in roles {
        let bytes = role.role_id.as_bytes();
        meta.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        meta.extend_from_slice(bytes);
        while meta.len() % 4 != 0 {
            meta.push(0);
        }
    }
    meta
}

fn path_coeff_sha256(coeffs: &[f32]) -> String {
    let mut canonical = String::with_capacity(coeffs.len() * 18 + 32);
    canonical.push_str("tspi-plan-path-coeffs/v1\n");
    for value in coeffs {
        let _ = writeln!(canonical, "{value:e}");
    }
    sha256_hex(canonical.as_bytes())
}

/// 有效支撑长度（去掉尾部零；至少 1）。
fn effective_taps(coeffs: &[f64]) -> usize {
    let mut len = coeffs.len();
    while len > 1 && coeffs[len - 1] == 0.0 {
        len -= 1;
    }
    len
}

fn diag_flags_for(req: &CompileApplyRequest<'_>, role: &RoleCompileInput, p_rows: &[f64]) -> u16 {
    let offset = role.role_index as usize * req.l_p as usize;
    let row = &p_rows[offset..offset + req.l_p as usize];
    let taps = effective_taps(row);
    let mut flags = 0u16;
    if role.fir_segments.is_empty() {
        // 传统单段路径：按行内容启发式（与历史完全一致；无 crossover 时 bit2 恒 0）。
        let target = !(taps == 1 && row[0] == 1.0);
        if target {
            flags |= DIAG_FLAG_TARGET;
        }
    } else {
        // 显式段来源：直接映射 core FirSegmentKind::diag_flag（crossover → bit2）。
        for kind in &role.fir_segments {
            flags |= kind.diag_flag();
        }
    }
    if req.encoding == DelayPolarityEncoding::CoeffEmbedded
        && (role.delay_samples > 0 || role.polarity < 0)
    {
        flags |= DIAG_FLAG_PHASE;
    }
    flags
}

fn role_record_for(
    req: &CompileApplyRequest<'_>,
    role: &RoleCompileInput,
    p_rows: &[f64],
) -> CompiledRoleRecord {
    let offset = role.role_index as usize * req.l_p as usize;
    let row = &p_rows[offset..offset + req.l_p as usize];
    let taps = effective_taps(row);
    let coeffs_f32: Vec<f32> = row[..taps].iter().map(|value| *value as f32).collect();
    let peak = max_abs(&f64_from_f32(&coeffs_f32));
    CompiledRoleRecord {
        role_index: role.role_index,
        role_id: role.role_id.clone(),
        out_channel: role.out_channel,
        delay_samples: if req.encoding == DelayPolarityEncoding::RoleMetadata {
            role.delay_samples
        } else {
            0
        },
        polarity: if req.encoding == DelayPolarityEncoding::RoleMetadata {
            role.polarity
        } else {
            1
        },
        gain_q15: role.gain_q15(),
        peq_count: role.peq_count,
        p_taps: taps as u32,
        p_peak_abs: peak,
        p_coeff_sha256: path_coeff_sha256(&coeffs_f32),
        phase_delay_samples: role.delay_samples,
        phase_polarity: role.polarity,
        delay_polarity_only: taps == 1 && row[0] == 1.0,
    }
}

fn pack_tables(
    req: &CompileApplyRequest<'_>,
    plans: &CompiledPlans,
    chosen: FusionForm,
    p_rows: &[f64],
) -> CaResult<PackedTables> {
    let scatter: Vec<u16> = req.roles.iter().map(|role| role.out_channel).collect();
    let mut raw_paths: Vec<RawPath> = match chosen {
        FusionForm::Factorized => plans
            .factorized
            .matrix_stage_plans()
            .into_iter()
            .map(|plan| {
                let mut flags = 0u8;
                if plan.diag_baseline {
                    flags |= PATH_FLAG_DIAG_BASELINE_INCLUDED;
                }
                RawPath {
                    out_index: scatter[plan.out_index as usize],
                    in_index: plan.in_source,
                    taps: plan.taps,
                    delay_samples: plan.delay_samples,
                    delay_q16: plan.delay_q16,
                    polarity: 1,
                    flags,
                    diag_baseline: plan.diag_baseline,
                    fused: false,
                    coeffs: plan.coeffs_f32,
                }
            })
            .collect(),
        FusionForm::FusedGp => {
            let fused = match plans.fused.as_ref() {
                Some(fused) => fused,
                None => {
                    return Err(ca_err(
                        E_CA_COMPILE,
                        "compile.fused",
                        "选择 fused 但 T12 未生成 fused 图（禁止凭空打包）",
                    ))
                }
            };
            fused
                .plan_paths()
                .into_iter()
                .map(|plan| RawPath {
                    out_index: scatter[plan.out_index as usize],
                    in_index: plan.in_index,
                    taps: plan.taps,
                    delay_samples: plan.delay_samples,
                    delay_q16: plan.delay_q16,
                    polarity: plan.polarity,
                    flags: plan.flags,
                    diag_baseline: plan.flags & PATH_FLAG_DIAG_BASELINE_INCLUDED != 0,
                    fused: true,
                    coeffs: plan.coeffs_f32,
                })
                .collect()
        }
    };
    // 防御：ABI v1.0 无 delay_q16（GAP-T66-2）。
    for path in &raw_paths {
        if path.delay_q16 != 0 {
            return Err(ca_err(
                E_CA_DELAY_Q16_ABI,
                "path.delay_q16",
                "T12 输出 delay_q16!=0，但 ABI v1.0 无该字段（GAP-T66-2）",
            ));
        }
        if path.taps == 0 {
            return Err(ca_err(E_CA_TAPS, "path.taps", "path taps 不能为 0"));
        }
    }
    raw_paths.sort_by_key(|path| (path.out_index, path.in_index));
    for window in raw_paths.windows(2) {
        if window[0].out_index == window[1].out_index && window[0].in_index == window[1].in_index {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                "paths",
                "打包后 path 表 (out_index,in_index) 重复（scatter 单射/源唯一被破坏）",
            ));
        }
    }
    if raw_paths.len() > MAX_MATRIX_PATHS {
        return Err(ca_err(
            E_CA_CAPACITY,
            "paths",
            format!("path 数 {} 超过 {MAX_MATRIX_PATHS}", raw_paths.len()),
        ));
    }

    // diag 表：factorized 保留 P 行；fused 已折入路径（不得再留 P 行，防双算）。
    let mut raw_diags: Vec<RawDiag> = match chosen {
        FusionForm::Factorized => plans
            .factorized
            .output_stage_plans()
            .into_iter()
            .filter_map(|plan| req.role(plan.index).map(|role| (role, plan)))
            .map(|(role, plan)| {
                let offset = role.role_index as usize * req.l_p as usize;
                let row = &p_rows[offset..offset + req.l_p as usize];
                let taps = effective_taps(row);
                RawDiag {
                    out_index: scatter[role.role_index as usize],
                    taps: taps as u32,
                    delay_samples: plan.delay_samples,
                    flags: diag_flags_for(req, role, p_rows),
                    coeffs: row[..taps].iter().map(|value| *value as f32).collect(),
                }
            })
            .collect(),
        FusionForm::FusedGp => Vec::new(),
    };
    for diag in &raw_diags {
        if diag.coeffs.is_empty() {
            return Err(ca_err(E_CA_TAPS, "diag.taps", "diag taps 不能为 0"));
        }
    }
    raw_diags.sort_by_key(|diag| diag.out_index);
    for window in raw_diags.windows(2) {
        if window[0].out_index == window[1].out_index {
            return Err(ca_err(
                E_CA_PATH_TABLE,
                "diags",
                "打包后 diag 表 out_index 重复（scatter 单射被破坏）",
            ));
        }
    }
    if raw_diags.len() > MAX_DIAG_FIRS {
        return Err(ca_err(
            E_CA_CAPACITY,
            "diags",
            format!("diag 数 {} 超过 {MAX_DIAG_FIRS}", raw_diags.len()),
        ));
    }

    // role 表（按 role_id 严格升序；接口要求）。
    let mut roles: Vec<TspiRoleV1> = Vec::with_capacity(req.roles.len());
    for role in req.roles {
        let flags = if role.source_channel.is_some() {
            ROLE_FLAG_ENABLED
        } else {
            ROLE_FLAG_ENABLED | ROLE_FLAG_ZERO_FILL
        };
        roles.push(TspiRoleV1 {
            role_id: role.role_index,
            out_index: role.out_channel,
            flags,
            reserved0: 0,
            delay_samples: if req.encoding == DelayPolarityEncoding::RoleMetadata {
                role.delay_samples
            } else {
                0
            },
            gain_q15: role.gain_q15(),
            polarity: if req.encoding == DelayPolarityEncoding::RoleMetadata {
                role.polarity
            } else {
                1
            },
            reserved1: 0,
            peq_count: role.peq_count,
            reserved2: 0,
            name_hash: crc32c(role.role_id.as_bytes()),
            reserved3: 0,
            reserved4: 0,
        });
    }
    roles.sort_by_key(|role| role.role_id);
    for window in roles.windows(2) {
        if window[0].role_id >= window[1].role_id {
            return Err(ca_err(
                E_CA_ROLES,
                "roles",
                "role 表必须按 role_id 严格升序",
            ));
        }
    }

    // 系数区（path 表顺序 -> diag 表顺序；每 blob 64B 对齐，零填充）。
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(raw_paths.len() + raw_diags.len());
    for path in &raw_paths {
        let mut bytes = Vec::with_capacity(path.coeffs.len() * 4);
        for value in &path.coeffs {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        blobs.push(bytes);
    }
    for diag in &raw_diags {
        let mut bytes = Vec::with_capacity(diag.coeffs.len() * 4);
        for value in &diag.coeffs {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        blobs.push(bytes);
    }
    // plan ABI v2（GAP-T66-3）：T64 PEQ 级联作为 per-role IIR 系数 blob 追加在 path/diag
    // blob 之后（role_id 升序；与 plan 的 IIR 表项同序）。无 PEQ = 不追加，v1 字节不变。
    let mut iir_blobs: Vec<Vec<u8>> = Vec::new();
    let mut iir_role_sections: Vec<(u16, u16)> = Vec::new();
    for role in &roles {
        let input = req.role(role.role_id).ok_or_else(|| {
            ca_err(
                E_CA_INTERNAL,
                format!("roles[{}]", role.role_id),
                "role 表项在请求 roles 中找不到（内部错误）",
            )
        })?;
        if input.peq_sections.is_empty() {
            continue;
        }
        let mut bytes = Vec::with_capacity(
            input.peq_sections.len() * PLAN_IIR_SECTION_COEFFS * 4,
        );
        for section in &input.peq_sections {
            for value in peq_section_coeffs_f32(section) {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        iir_role_sections.push((role.role_id, input.peq_sections.len() as u16));
        blobs.push(bytes.clone());
        iir_blobs.push(bytes);
    }
    let (coeff_region, offsets) = build_coeff_region(&blobs);
    if coeff_region.len() as u64 > req.options.coeff_budget_bytes {
        return Err(ca_err(
            E_CA_CAPACITY,
            "coeff_region",
            format!(
                "系数区 {} B 超过预算 {} B（禁止截尾）",
                coeff_region.len(),
                req.options.coeff_budget_bytes
            ),
        ));
    }

    // path 表 + 记录。
    let mut paths = Vec::with_capacity(raw_paths.len());
    let mut path_records = Vec::with_capacity(raw_paths.len());
    for (index, path) in raw_paths.iter().enumerate() {
        let coeff_offset = offsets[index];
        paths.push(TspiPathV1 {
            in_index: path.in_index,
            out_index: path.out_index,
            taps: path.taps,
            delay_samples: path.delay_samples,
            polarity: path.polarity,
            flags: path.flags,
            reserved0: 0,
            coeff_offset_bytes: coeff_offset,
            coeff_len_bytes: path.taps * 4,
            reserved1: 0,
        });
        let out_role = req
            .roles
            .iter()
            .find(|role| role.out_channel == path.out_index)
            .map(|role| role.role_id.clone())
            .unwrap_or_else(|| format!("ch{}", path.out_index));
        let in_role = req
            .roles
            .iter()
            .find(|role| role.source_channel == Some(path.in_index))
            .map(|role| role.role_id.clone())
            .unwrap_or_else(|| format!("src{}", path.in_index));
        path_records.push(CompiledPathRecord {
            out_index: path.out_index,
            in_index: path.in_index,
            out_role,
            in_role,
            out_dac: path.out_index,
            taps: path.taps,
            delay_samples: path.delay_samples,
            delay_q16: path.delay_q16,
            polarity: path.polarity,
            flags: path.flags,
            diag_baseline: path.diag_baseline,
            fused: path.fused,
            coeff_sha256: path_coeff_sha256(&path.coeffs),
            peak_abs: max_abs(&f64_from_f32(&path.coeffs)),
        });
    }

    // diag 表。
    let mut diags = Vec::with_capacity(raw_diags.len());
    for (index, diag) in raw_diags.iter().enumerate() {
        let coeff_offset = offsets[raw_paths.len() + index];
        diags.push(TspiDiagFirV1 {
            out_index: diag.out_index,
            flags: diag.flags,
            taps: diag.taps,
            delay_samples: diag.delay_samples,
            reserved0: 0,
            coeff_offset_bytes: coeff_offset,
            coeff_len_bytes: diag.taps * 4,
            reserved1: 0,
        });
    }
    let role_records: Vec<CompiledRoleRecord> = req
        .roles
        .iter()
        .map(|role| role_record_for(req, role, p_rows))
        .collect();

    let max_taps = paths
        .iter()
        .map(|path| path.taps)
        .chain(diags.iter().map(|diag| diag.taps))
        .max()
        .unwrap_or(1);
    // plan ABI v2 IIR 表项：系数偏移相对系数区起点（64 B 对齐；append 在 path/diag blob 之后）。
    let iir_base = raw_paths.len() + raw_diags.len();
    let mut iir_entries: Vec<TspiRoleIirV2> = Vec::with_capacity(iir_role_sections.len());
    for (index, (role_id, section_count)) in iir_role_sections.iter().enumerate() {
        let coeff_offset = offsets[iir_base + index];
        iir_entries.push(TspiRoleIirV2 {
            role_id: *role_id,
            order: 2,
            section_count: *section_count,
            flags: 0,
            coeff_offset_bytes: coeff_offset,
            coeff_len_bytes: *section_count as u32 * 4 * PLAN_IIR_SECTION_COEFFS as u32,
            reserved0: 0,
            reserved1: 0,
            reserved2: 0,
        });
    }
    let iir_table = if iir_entries.is_empty() {
        Vec::new()
    } else {
        iir_table_bytes(&iir_entries, &iir_blobs)?
    };
    Ok(PackedTables {
        paths,
        diags,
        roles,
        coeff_region,
        coeff_blobs: blobs,
        path_records,
        role_records,
        max_taps,
        iir_entries,
        iir_blobs,
        iir_table,
    })
}

/// 系数区布局：每 blob 起点 64B 对齐（零填充），返回 (region, 相对偏移)。
pub(crate) fn build_coeff_region(blobs: &[Vec<u8>]) -> (Vec<u8>, Vec<u64>) {
    let mut region: Vec<u8> = Vec::new();
    let mut offsets = Vec::with_capacity(blobs.len());
    for blob in blobs {
        let offset =
            ((region.len() + COEFF_ALIGN_BYTES - 1) / COEFF_ALIGN_BYTES) * COEFF_ALIGN_BYTES;
        if region.len() < offset {
            region.resize(offset, 0);
        }
        offsets.push(offset as u64);
        region.extend_from_slice(blob);
    }
    (region, offsets)
}

fn hex_to_32(hex: &str) -> CaResult<[u8; 32]> {
    if hex.len() != 64 {
        return Err(ca_err(E_CA_INTERNAL, "sha256", "SHA-256 hex 长度必须 64"));
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    let mut index = 0usize;
    while index < 32 {
        let hi = hex_nibble(bytes[index * 2])?;
        let lo = hex_nibble(bytes[index * 2 + 1])?;
        out[index] = (hi << 4) | lo;
        index += 1;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> CaResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ca_err(E_CA_INTERNAL, "sha256", "非法 hex 字符")),
    }
}

/// plan header 序列化（128 B；与 tests/T20/swap_models.rs 的 header_bytes 逐字节一致）。
pub(crate) fn header_bytes(header: &TspiPlanHeaderV1) -> [u8; PLAN_HEADER_BYTES] {
    let mut out: Vec<u8> = Vec::with_capacity(PLAN_HEADER_BYTES);
    out.extend_from_slice(&header.magic.to_le_bytes());
    out.extend_from_slice(&header.abi_version.to_le_bytes());
    out.extend_from_slice(&header.header_size.to_le_bytes());
    out.extend_from_slice(&header.plan_version.to_le_bytes());
    out.extend_from_slice(&header.sample_rate.to_le_bytes());
    out.extend_from_slice(&header.block_size.to_le_bytes());
    out.extend_from_slice(&header.n_in.to_le_bytes());
    out.extend_from_slice(&header.n_out.to_le_bytes());
    out.extend_from_slice(&header.low_band_count.to_le_bytes());
    out.extend_from_slice(&header.path_count.to_le_bytes());
    out.extend_from_slice(&header.diag_fir_count.to_le_bytes());
    out.extend_from_slice(&header.max_taps.to_le_bytes());
    out.push(header.workers);
    out.push(header.flags);
    out.push(header.precision);
    out.push(header.reserved0);
    out.extend_from_slice(&header.graph_id);
    out.extend_from_slice(&header.total_coeff_bytes.to_le_bytes());
    out.extend_from_slice(&header.path_table_offset.to_le_bytes());
    out.extend_from_slice(&header.diag_table_offset.to_le_bytes());
    out.extend_from_slice(&header.in_stride.to_le_bytes());
    out.extend_from_slice(&header.out_stride.to_le_bytes());
    out.extend_from_slice(&header.role_table_offset.to_le_bytes());
    out.extend_from_slice(&header.role_table_count.to_le_bytes());
    out.extend_from_slice(&header.reserved1.to_le_bytes());
    out.extend_from_slice(&header.affinity_table_offset.to_le_bytes());
    out.extend_from_slice(&header.affinity_count.to_le_bytes());
    out.extend_from_slice(&header.reserved2.to_le_bytes());
    out.extend_from_slice(&header.ctl_meta_offset.to_le_bytes());
    out.extend_from_slice(&header.ctl_meta_len.to_le_bytes());
    for value in header.reserved3 {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out.extend_from_slice(&header.header_crc32c.to_le_bytes());
    let mut bytes = [0u8; PLAN_HEADER_BYTES];
    bytes.copy_from_slice(&out);
    bytes
}

pub(crate) fn path_table_bytes(paths: &[TspiPathV1]) -> Vec<u8> {
    let mut out = Vec::with_capacity(paths.len() * PATH_ENTRY_BYTES);
    for path in paths {
        out.extend_from_slice(&path.in_index.to_le_bytes());
        out.extend_from_slice(&path.out_index.to_le_bytes());
        out.extend_from_slice(&path.taps.to_le_bytes());
        out.extend_from_slice(&path.delay_samples.to_le_bytes());
        out.push(path.polarity as u8);
        out.push(path.flags);
        out.extend_from_slice(&path.reserved0.to_le_bytes());
        out.extend_from_slice(&path.coeff_offset_bytes.to_le_bytes());
        out.extend_from_slice(&path.coeff_len_bytes.to_le_bytes());
        out.extend_from_slice(&path.reserved1.to_le_bytes());
    }
    out
}

pub(crate) fn diag_table_bytes(diags: &[TspiDiagFirV1]) -> Vec<u8> {
    let mut out = Vec::with_capacity(diags.len() * DIAG_ENTRY_BYTES);
    for diag in diags {
        out.extend_from_slice(&diag.out_index.to_le_bytes());
        out.extend_from_slice(&diag.flags.to_le_bytes());
        out.extend_from_slice(&diag.taps.to_le_bytes());
        out.extend_from_slice(&diag.delay_samples.to_le_bytes());
        out.extend_from_slice(&diag.reserved0.to_le_bytes());
        out.extend_from_slice(&diag.coeff_offset_bytes.to_le_bytes());
        out.extend_from_slice(&diag.coeff_len_bytes.to_le_bytes());
        out.extend_from_slice(&diag.reserved1.to_le_bytes());
    }
    out
}

pub(crate) fn role_table_bytes(roles: &[TspiRoleV1]) -> Vec<u8> {
    let mut out = Vec::with_capacity(roles.len() * ROLE_ENTRY_BYTES);
    for role in roles {
        out.extend_from_slice(&role.role_id.to_le_bytes());
        out.extend_from_slice(&role.out_index.to_le_bytes());
        out.extend_from_slice(&role.flags.to_le_bytes());
        out.extend_from_slice(&role.reserved0.to_le_bytes());
        out.extend_from_slice(&role.delay_samples.to_le_bytes());
        out.extend_from_slice(&role.gain_q15.to_le_bytes());
        out.push(role.polarity as u8);
        out.push(role.reserved1);
        out.extend_from_slice(&role.peq_count.to_le_bytes());
        out.extend_from_slice(&role.reserved2.to_le_bytes());
        out.extend_from_slice(&role.name_hash.to_le_bytes());
        out.extend_from_slice(&role.reserved3.to_le_bytes());
        out.extend_from_slice(&role.reserved4.to_le_bytes());
    }
    out
}

/// 8 字节对齐（IIR 表起址；表区/系数区均按 8 B 表边界，T20 parse 硬门）。
pub(crate) fn align8_checked(value: usize) -> CaResult<usize> {
    value
        .checked_add(7)
        .map(|sum| sum / 8 * 8)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "align8", "偏移溢出"))
}

/// 单段 PEQ biquad 的 plan 编码（f32 [b0,b1,b2,a1,a2]；a0 归一化为 1）。
pub(crate) fn peq_section_coeffs_f32(section: &PeqBiquad) -> [f32; PLAN_IIR_SECTION_COEFFS] {
    [
        section.b0 as f32,
        section.b1 as f32,
        section.b2 as f32,
        section.a1 as f32,
        section.a2 as f32,
    ]
}

/// plan ABI v2 IIR 表头序列化（16 B；table_crc32c 由 [`iir_table_bytes`] 回填）。
fn iir_header_bytes(header: &TspiPlanIirHeaderV2) -> [u8; PLAN_IIR_HEADER_BYTES] {
    let mut out = [0u8; PLAN_IIR_HEADER_BYTES];
    out[0..4].copy_from_slice(&header.magic.to_le_bytes());
    out[4..6].copy_from_slice(&header.version.to_le_bytes());
    out[6..8].copy_from_slice(&header.entry_bytes.to_le_bytes());
    out[8..10].copy_from_slice(&header.entry_count.to_le_bytes());
    out[10..12].copy_from_slice(&header.reserved0.to_le_bytes());
    out[12..16].copy_from_slice(&header.table_crc32c.to_le_bytes());
    out
}

/// plan ABI v2 每 role IIR 表项序列化（32 B）。
pub(crate) fn iir_entry_bytes(entry: &TspiRoleIirV2) -> [u8; PLAN_IIR_ENTRY_BYTES] {
    let mut out = [0u8; PLAN_IIR_ENTRY_BYTES];
    out[0..2].copy_from_slice(&entry.role_id.to_le_bytes());
    out[2..4].copy_from_slice(&entry.order.to_le_bytes());
    out[4..6].copy_from_slice(&entry.section_count.to_le_bytes());
    out[6..8].copy_from_slice(&entry.flags.to_le_bytes());
    out[8..16].copy_from_slice(&entry.coeff_offset_bytes.to_le_bytes());
    out[16..20].copy_from_slice(&entry.coeff_len_bytes.to_le_bytes());
    out[20..24].copy_from_slice(&entry.reserved0.to_le_bytes());
    out[24..28].copy_from_slice(&entry.reserved1.to_le_bytes());
    out[28..32].copy_from_slice(&entry.reserved2.to_le_bytes());
    out
}

/// plan ABI v2 IIR 表字节（表头 crc 置 0 → entries → 系数 blob，按 entry 顺序算 CRC-32C）。
///
/// 表布局位于 ctl_meta 之后、系数区之前；[table_crc32c] 覆盖表头（本字段置 0）+ 表项 +
/// 按 entry 顺序的系数 blob 原始字节（GAP-T66-3；防系数被替换而 graph_id 校验缺失的洞）。
pub(crate) fn iir_table_bytes(
    entries: &[TspiRoleIirV2],
    iir_blobs: &[Vec<u8>],
) -> CaResult<Vec<u8>> {
    if entries.len() != iir_blobs.len() {
        return Err(ca_err(
            E_CA_INTERNAL,
            "iir_table",
            "IIR 表项数与系数 blob 数不一致（内部错误）",
        ));
    }
    let header = TspiPlanIirHeaderV2 {
        magic: TSPI_PLAN_IIR_MAGIC,
        version: TSPI_PLAN_IIR_VERSION,
        entry_bytes: PLAN_IIR_ENTRY_BYTES as u16,
        entry_count: entries.len() as u16,
        reserved0: 0,
        table_crc32c: 0,
    };
    let mut table: Vec<u8> =
        Vec::with_capacity(PLAN_IIR_HEADER_BYTES + entries.len() * PLAN_IIR_ENTRY_BYTES);
    table.extend_from_slice(&iir_header_bytes(&header));
    for entry in entries {
        table.extend_from_slice(&iir_entry_bytes(entry));
    }
    let mut crc_input = table.clone();
    for blob in iir_blobs {
        crc_input.extend_from_slice(blob);
    }
    let crc = crc32c(&crc_input);
    table[12..16].copy_from_slice(&crc.to_le_bytes());
    Ok(table)
}

/// 从 plan 系数区解析某 IIR 表项的 f32 biquad 段（有限 + 稳定；host RT parse 同口径）。
fn parse_encoded_peq_sections(
    code: &'static str,
    coeffs: &[u8],
    offset: u64,
    len: u32,
) -> CaResult<Vec<[f32; PLAN_IIR_SECTION_COEFFS]>> {
    if offset % COEFF_ALIGN_BYTES as u64 != 0 {
        return Err(ca_err(code, "iir.coeff_offset_bytes", "IIR 系数偏移未 64B 对齐"));
    }
    let start = offset as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| ca_err(code, "iir.coeff_len_bytes", "IIR 系数区间溢出"))?;
    if end > coeffs.len() {
        return Err(ca_err(code, "iir.coeff_len_bytes", "IIR 系数区间越界"));
    }
    let section_bytes = 4 * PLAN_IIR_SECTION_COEFFS;
    if len as usize % section_bytes != 0 {
        return Err(ca_err(
            code,
            "iir.coeff_len_bytes",
            "IIR 系数长度必须是 5×f32 的整数倍",
        ));
    }
    let mut sections = Vec::with_capacity(len as usize / section_bytes);
    let mut base = start;
    while base < end {
        let mut section = [0.0f32; PLAN_IIR_SECTION_COEFFS];
        for (index, value) in section.iter_mut().enumerate() {
            let at = base + index * 4;
            *value = f32::from_le_bytes([coeffs[at], coeffs[at + 1], coeffs[at + 2], coeffs[at + 3]]);
            if !value.is_finite() {
                return Err(ca_err(code, "iir.coeff", "IIR 系数含 NaN/Inf"));
            }
        }
        let (a1, a2) = (section[3], section[4]);
        if !(a2.abs() < 1.0 && a1.abs() < 1.0 + a2) {
            return Err(ca_err(
                code,
                "iir.coeff",
                format!("IIR 段不稳定（a1={a1}, a2={a2}；需 |a2|<1 且 |a1|<1+a2）"),
            ));
        }
        sections.push(section);
        base += section_bytes;
    }
    Ok(sections)
}

/// f32 编码单段 biquad 的幅度响应（dB；a0=1）。
fn encoded_section_response_db(
    section: &[f32; PLAN_IIR_SECTION_COEFFS],
    fs: f64,
    freq_hz: f64,
) -> f64 {
    let w = 2.0 * std::f64::consts::PI * freq_hz / fs;
    let (b0, b1, b2) = (section[0] as f64, section[1] as f64, section[2] as f64);
    let (a1, a2) = (section[3] as f64, section[4] as f64);
    let num_re = b0 + b1 * w.cos() + b2 * (2.0 * w).cos();
    let num_im = -(b1 * w.sin() + b2 * (2.0 * w).sin());
    let den_re = 1.0 + a1 * w.cos() + a2 * (2.0 * w).cos();
    let den_im = -(a1 * w.sin() + a2 * (2.0 * w).sin());
    let num = (num_re * num_re + num_im * num_im).sqrt();
    let den = (den_re * den_re + den_im * den_im).sqrt();
    20.0 * (num / den).log10()
}

/// graph_id：SHA-256(domain || header(graph_id/crc/plan_version 置零) || path 表 || diag 表 ||
/// role 表 || affinity 表 || ctl_meta || 系数 blob 按表顺序拼接 || plan ABI v2 IIR 表 + IIR blobs)
/// （core-v1 §11.10 规则 4/注 5；v2 additive 段见 GAP-T66-3）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_graph_id(
    header: &TspiPlanHeaderV1,
    paths: &[TspiPathV1],
    diags: &[TspiDiagFirV1],
    roles: &[TspiRoleV1],
    affinity: &[u8],
    ctl_meta: &[u8],
    coeff_blobs: &[Vec<u8>],
    iir_table: &[u8],
    iir_blobs: &[Vec<u8>],
) -> CaResult<[u8; 32]> {
    let mut zeroed = *header;
    zeroed.graph_id = [0u8; 32];
    zeroed.header_crc32c = 0;
    zeroed.plan_version = 0;
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(GRAPH_HASH_DOMAIN);
    payload.extend_from_slice(&header_bytes(&zeroed));
    payload.extend_from_slice(&path_table_bytes(paths));
    payload.extend_from_slice(&diag_table_bytes(diags));
    payload.extend_from_slice(&role_table_bytes(roles));
    payload.extend_from_slice(affinity);
    payload.extend_from_slice(ctl_meta);
    for blob in coeff_blobs {
        payload.extend_from_slice(blob);
    }
    // plan ABI v2 additive 段（GAP-T66-3）：IIR 表 + 按 entry 顺序的 IIR 系数 blob。
    // v1（iir_table/iir_blobs 为空）时 payload 与旧规则逐字节相同。
    payload.extend_from_slice(iir_table);
    for blob in iir_blobs {
        payload.extend_from_slice(blob);
    }
    hex_to_32(&sha256_hex(&payload))
}

fn plan_tables_end(packed: &PackedTables, ctl_meta_len: usize) -> CaResult<usize> {
    let mut end = PLAN_HEADER_BYTES;
    end = end
        .checked_add(packed.paths.len() * PATH_ENTRY_BYTES)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "plan_bytes", "长度溢出"))?;
    end = end
        .checked_add(packed.diags.len() * DIAG_ENTRY_BYTES)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "plan_bytes", "长度溢出"))?;
    end = end
        .checked_add(packed.roles.len() * ROLE_ENTRY_BYTES)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "plan_bytes", "长度溢出"))?;
    end = end
        .checked_add(ctl_meta_len)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "plan_bytes", "长度溢出"))?;
    if packed.iir_entries.is_empty() {
        return Ok(end);
    }
    // plan ABI v2：IIR 表 8 B 对齐起址 + 表字节（表后紧跟系数区）。
    let table_offset = align8_checked(end)?;
    table_offset
        .checked_add(packed.iir_table.len())
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "plan_bytes", "长度溢出"))
}

/// 组 header（offsets/counts/flags；graph_id/CRC 置 0 待写）。
fn pack_plan_header(
    req: &CompileApplyRequest<'_>,
    packed: &PackedTables,
    m: usize,
) -> CaResult<(TspiPlanHeaderV1, [u8; 32])> {
    let path_table_offset = PLAN_HEADER_BYTES as u32;
    let diag_table_offset = path_table_offset
        .checked_add((packed.paths.len() * PATH_ENTRY_BYTES) as u32)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "diag_table_offset", "偏移溢出"))?;
    let role_table_offset = diag_table_offset
        .checked_add((packed.diags.len() * DIAG_ENTRY_BYTES) as u32)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "role_table_offset", "偏移溢出"))?;
    let ctl_meta = build_ctl_meta(req.roles);
    let ctl_meta_offset = role_table_offset
        .checked_add((packed.roles.len() * ROLE_ENTRY_BYTES) as u32)
        .ok_or_else(|| ca_err(E_CA_CAPACITY, "ctl_meta_offset", "偏移溢出"))?;
    let ctl_meta_len = ctl_meta.len() as u32;
    // plan ABI v2（GAP-T66-3）：IIR 表紧接 ctl_meta（8 B 对齐）；无 IIR = 0/无表。
    let iir_table_offset = if packed.iir_entries.is_empty() {
        0u32
    } else {
        align8_checked(ctl_meta_offset as usize + ctl_meta.len())? as u32
    };
    let abi_version = if packed.iir_entries.is_empty() {
        TSPI_PLAN_ABI_V1
    } else {
        TSPI_PLAN_ABI_V2
    };
    let total_coeff_bytes = packed.coeff_region.len() as u32;
    let mut flags = 0u8;
    if req.mapping.stereo_strict {
        flags |= PLAN_FLAG_STEREO_STRICT;
    }
    if packed.max_taps > TAPS_NORMAL_MAX {
        flags |= PLAN_FLAG_EXTENDED_TAPS_ADMITTED;
    }
    let header = TspiPlanHeaderV1 {
        magic: TSPI_PLAN_MAGIC,
        abi_version,
        header_size: PLAN_HEADER_BYTES as u16,
        plan_version: PLAN_VERSION_UNASSIGNED,
        sample_rate: req.sample_rate_hz,
        block_size: req.block_size,
        n_in: req.n_in,
        n_out: req.container_channels,
        low_band_count: m as u16,
        path_count: packed.paths.len() as u16,
        diag_fir_count: packed.diags.len() as u16,
        max_taps: packed.max_taps,
        workers: req.options.workers,
        flags,
        precision: 0,
        reserved0: 0,
        graph_id: [0u8; 32],
        total_coeff_bytes,
        path_table_offset,
        diag_table_offset,
        in_stride: req.block_size as u32,
        out_stride: req.block_size as u32,
        role_table_offset,
        role_table_count: packed.roles.len() as u16,
        reserved1: 0,
        affinity_table_offset: 0,
        affinity_count: 0,
        reserved2: 0,
        ctl_meta_offset,
        ctl_meta_len,
        reserved3: if packed.iir_entries.is_empty() {
            [0u32; 3]
        } else {
            [iir_table_offset, packed.iir_entries.len() as u32, 0]
        },
        header_crc32c: 0,
    };
    let graph_id = compute_graph_id(
        &header,
        &packed.paths,
        &packed.diags,
        &packed.roles,
        &[],
        &ctl_meta,
        &packed.coeff_blobs,
        &packed.iir_table,
        &packed.iir_blobs,
    )?;
    Ok((header, graph_id))
}

fn pack_plan_bytes(
    req: &CompileApplyRequest<'_>,
    packed: &PackedTables,
    header: &mut TspiPlanHeaderV1,
    graph_id: &[u8; 32],
) -> CaResult<Vec<u8>> {
    let ctl_meta = build_ctl_meta(req.roles);
    header.graph_id = *graph_id;
    header.header_crc32c = 0;
    header.header_crc32c = crc32c(&header_bytes(header));
    let tables_end = plan_tables_end(packed, ctl_meta.len())?;
    let mut bytes: Vec<u8> = Vec::with_capacity(tables_end + packed.coeff_region.len());
    bytes.extend_from_slice(&header_bytes(header));
    bytes.extend_from_slice(&path_table_bytes(&packed.paths));
    bytes.extend_from_slice(&diag_table_bytes(&packed.diags));
    bytes.extend_from_slice(&role_table_bytes(&packed.roles));
    bytes.extend_from_slice(&ctl_meta);
    if !packed.iir_entries.is_empty() {
        // plan ABI v2：ctl_meta 与 IIR 表之间零填充到 8 B 对齐（表区字节确定性）。
        let iir_offset = header.reserved3[0] as usize;
        while bytes.len() < iir_offset {
            bytes.push(0);
        }
        bytes.extend_from_slice(&packed.iir_table);
    }
    bytes.extend_from_slice(&packed.coeff_region);
    Ok(bytes)
}

impl CompiledPlan {
    /// 自校验（拒坏图）：header/表/系数区/graph_id/CRC 全部按 core-v1 §11.10 + T20 parse 口径复核。
    ///
    /// 任一项不成立 = 拒绝（E_CA_PLAN_VERIFY），不进入 prepare。
    pub fn verify(&self) -> CaResult<()> {
        let header = &self.header;
        if header.magic != TSPI_PLAN_MAGIC {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.magic",
                format!("magic={:#010x} 不是 TSPI_PLAN_MAGIC", header.magic),
            ));
        }
        if !plan_abi_supported(header.abi_version) {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.abi_version",
                format!(
                    "abi_version={} 不受支持（只接受 1=无 IIR / 2=per-role IIR；GAP-T66-3）",
                    header.abi_version
                ),
            ));
        }
        // abi_version 与 IIR 表存在性一致：有 IIR 表必须 v2；v1 必须无 reserved3/无 IIR。
        if !self.iir_entries.is_empty() && header.abi_version != TSPI_PLAN_ABI_V2 {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.abi_version",
                "plan 含 IIR 表项但 abi_version != 2",
            ));
        }
        if header.abi_version == TSPI_PLAN_ABI_V1 && header.reserved3 != [0u32; 3] {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.reserved3",
                "v1 plan 的 reserved3 必须为 0（A-ABI-3）",
            ));
        }
        if header.header_size != PLAN_HEADER_BYTES as u16 {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.header_size",
                "header_size 必须为 128",
            ));
        }
        if header.plan_version != PLAN_VERSION_UNASSIGNED {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.plan_version",
                "编译产物 plan_version 必须为 0（由 T20 registry 分配）",
            ));
        }
        if header.reserved0 != 0 || header.reserved1 != 0 || header.reserved2 != 0 {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.reserved",
                "reserved 字段必须为 0（A-ABI-3；reserved3 见 IIR 表校验）",
            ));
        }
        if header.flags & !PLAN_FLAG_MASK != 0 {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.flags",
                "plan flags 含未知位",
            ));
        }
        if header.precision != 0 {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.precision",
                "runtime 只接受 f32 plan（precision=0）",
            ));
        }
        if !SAMPLE_RATES_HZ.contains(&header.sample_rate) {
            return Err(ca_err(E_CA_PLAN_VERIFY, "header.sample_rate", "fs 非法"));
        }
        if !BLOCK_SIZES.contains(&header.block_size) {
            return Err(ca_err(E_CA_PLAN_VERIFY, "header.block_size", "block 非法"));
        }
        if header.n_in == 0
            || header.n_out == 0
            || header.n_in as usize > MAX_INPUT_CHANNELS
            || header.n_out as usize > MAX_OUTPUT_CHANNELS
        {
            return Err(ca_err(E_CA_PLAN_VERIFY, "header.n_in/n_out", "通道数越界"));
        }
        if header.in_stride < header.block_size as u32
            || header.in_stride % 16 != 0
            || header.out_stride < header.block_size as u32
            || header.out_stride % 16 != 0
        {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.stride",
                "stride 必须 >= block 且为 16 的倍数（A-ABI-9/ERR_STRIDE_ALIGN）",
            ));
        }
        if header.path_count as usize != self.paths.len()
            || header.diag_fir_count as usize != self.diags.len()
            || header.role_table_count as usize != self.roles.len()
        {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.count",
                "header 表计数与实际表长度不一致",
            ));
        }
        if header.path_count as usize > MAX_MATRIX_PATHS
            || header.diag_fir_count as usize > MAX_DIAG_FIRS
            || header.role_table_count as usize > MAX_OUTPUT_ROLES
        {
            return Err(ca_err(E_CA_PLAN_VERIFY, "header.count", "表规模超上限"));
        }
        let computed_max_taps = self
            .paths
            .iter()
            .map(|path| path.taps)
            .chain(self.diags.iter().map(|diag| diag.taps))
            .max()
            .unwrap_or(0);
        if computed_max_taps != header.max_taps {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.max_taps",
                format!(
                    "max_taps={} 与实际最大 taps={computed_max_taps} 不一致",
                    header.max_taps
                ),
            ));
        }
        if header.max_taps > TAPS_EXTENDED_MAX {
            return Err(ca_err(E_CA_PLAN_VERIFY, "header.max_taps", "taps 超 16384"));
        }
        if header.max_taps > TAPS_NORMAL_MAX && header.flags & PLAN_FLAG_EXTENDED_TAPS_ADMITTED == 0
        {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.flags",
                "max_taps>4096 缺 extended 准入位",
            ));
        }
        // 布局偏移（与打包时固定顺序一致）。
        let expected_path_off = PLAN_HEADER_BYTES as u32;
        let expected_diag_off = expected_path_off + (self.paths.len() * PATH_ENTRY_BYTES) as u32;
        let expected_role_off = expected_diag_off + (self.diags.len() * DIAG_ENTRY_BYTES) as u32;
        let expected_ctl_off = expected_role_off + (self.roles.len() * ROLE_ENTRY_BYTES) as u32;
        if header.path_table_offset != expected_path_off
            || header.diag_table_offset != expected_diag_off
            || header.role_table_offset != expected_role_off
            || header.ctl_meta_offset != expected_ctl_off
            || header.ctl_meta_len as usize != self.ctl_meta.len()
            || header.affinity_table_offset != 0
            || header.affinity_count != 0
        {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.offset",
                "表偏移/ctl_meta/affinity 与打包顺序不一致（坏图）",
            ));
        }
        let tables_end = expected_ctl_off as usize + self.ctl_meta.len();
        // plan ABI v2：IIR 表起址 = align8(表区结束)，紧随其后是 16 B 表头 + 表项，再进系数区。
        if self.iir_entries.is_empty() {
            if tables_end > PLAN_HEADER_BYTES && tables_end % 8 != 0 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "header.offset",
                    "表区结束未 8B 对齐（T20 table_region 要求）",
                ));
            }
        } else {
            let expected_iir_off = align8_checked(tables_end)?;
            if header.reserved3
                != [
                    expected_iir_off as u32,
                    self.iir_entries.len() as u32,
                    0,
                ]
            {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "header.reserved3",
                    "v2 reserved3 必须为 [IIR 表偏移, 表项数, 0] 且与表区布局一致",
                ));
            }
            let expected_table_len =
                PLAN_IIR_HEADER_BYTES + self.iir_entries.len() * PLAN_IIR_ENTRY_BYTES;
            if self.iir_table.len() != expected_table_len {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "iir_table",
                    "IIR 表长度与表项数不一致",
                ));
            }
            let iir_end = expected_iir_off
                .checked_add(expected_table_len)
                .ok_or_else(|| ca_err(E_CA_CAPACITY, "iir_table", "偏移溢出"))?;
            let coeff_base = self
                .plan_bytes
                .len()
                .checked_sub(self.coeffs.len())
                .ok_or_else(|| ca_err(E_CA_PLAN_VERIFY, "plan_bytes", "系数区起点越界"))?;
            if iir_end != coeff_base {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "iir_table",
                    "IIR 表尾未与系数区起点对齐（坏图：表区/系数区之间有缝）",
                ));
            }
        }
        let total_coeff = header.total_coeff_bytes as usize;
        if total_coeff != self.coeffs.len() {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.total_coeff_bytes",
                "系数区长度与表声明不一致",
            ));
        }
        let (rebuilt_region, rebuilt_offsets) = build_coeff_region(&self.coeff_blobs);
        if rebuilt_region != self.coeffs {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "coeff_region",
                "系数区与 blob 序列（64B 对齐）不一致",
            ));
        }
        let mut blob_index = 0usize;
        for path in &self.paths {
            match rebuilt_offsets.get(blob_index) {
                Some(offset) if *offset == path.coeff_offset_bytes => {}
                _ => {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "paths.coeff_offset_bytes",
                        "path 系数偏移与 blob 布局不一致",
                    ))
                }
            }
            blob_index += 1;
        }
        for diag in &self.diags {
            match rebuilt_offsets.get(blob_index) {
                Some(offset) if *offset == diag.coeff_offset_bytes => {}
                _ => {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "diags.coeff_offset_bytes",
                        "diag 系数偏移与 blob 布局不一致",
                    ))
                }
            }
            blob_index += 1;
        }
        for entry in &self.iir_entries {
            match rebuilt_offsets.get(blob_index) {
                Some(offset) if *offset == entry.coeff_offset_bytes => {}
                _ => {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "iir.coeff_offset_bytes",
                        "IIR 系数偏移与 blob 布局不一致",
                    ))
                }
            }
            blob_index += 1;
        }

        // path 表：排序/越界/taps/polarity/flags/系数区间。
        let mut previous_key: Option<(u16, u16)> = None;
        for (index, path) in self.paths.iter().enumerate() {
            if path.in_index >= header.n_in || path.out_index >= header.n_out {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}]"),
                    "in/out 索引越界",
                ));
            }
            if path.taps == 0 || path.taps > header.max_taps {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}].taps"),
                    "taps 越界",
                ));
            }
            if path.polarity != 1 && path.polarity != -1 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}].polarity"),
                    "polarity 必须 ±1",
                ));
            }
            if path.flags & !0x0F != 0 || path.reserved0 != 0 || path.reserved1 != 0 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}].flags"),
                    "path flags/reserved 非法",
                ));
            }
            if path.coeff_len_bytes != path.taps * 4 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}].coeff_len_bytes"),
                    "coefficients 长度必须 = taps*4",
                ));
            }
            if path.coeff_offset_bytes % COEFF_ALIGN_BYTES as u64 != 0
                || path.coeff_offset_bytes + path.coeff_len_bytes as u64 > total_coeff as u64
            {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("paths[{index}].coeff_offset_bytes"),
                    "系数区间越界/未 64B 对齐",
                ));
            }
            let key = (path.out_index, path.in_index);
            if let Some(previous) = previous_key {
                if key <= previous {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "paths",
                        "path 表必须按 (out_index,in_index) 严格升序",
                    ));
                }
            }
            previous_key = Some(key);
        }
        let mut previous_diag: Option<u16> = None;
        for (index, diag) in self.diags.iter().enumerate() {
            if diag.out_index >= header.n_out {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("diags[{index}].out_index"),
                    "out_index 越界",
                ));
            }
            if diag.taps == 0 || diag.taps > header.max_taps {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("diags[{index}].taps"),
                    "taps 越界",
                ));
            }
            if diag.flags & !DIAG_FLAG_MASK != 0 || diag.reserved0 != 0 || diag.reserved1 != 0 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("diags[{index}].flags"),
                    "diag flags/reserved 非法",
                ));
            }
            if diag.coeff_len_bytes != diag.taps * 4 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("diags[{index}].coeff_len_bytes"),
                    "coefficients 长度必须 = taps*4",
                ));
            }
            if diag.coeff_offset_bytes % COEFF_ALIGN_BYTES as u64 != 0
                || diag.coeff_offset_bytes + diag.coeff_len_bytes as u64 > total_coeff as u64
            {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("diags[{index}].coeff_offset_bytes"),
                    "系数区间越界/未 64B 对齐",
                ));
            }
            if let Some(previous) = previous_diag {
                if diag.out_index <= previous {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "diags",
                        "diag 表必须按 out_index 严格升序",
                    ));
                }
            }
            previous_diag = Some(diag.out_index);
        }
        // crossover flag/证据一致性：report 摘要存在 <=> plan 至少一行带 DIAG_FLAG_CROSSOVER，
        // 且行数必须与摘要一致（无 crossover 时两者都必须为空/0，历史行为不变）。
        let crossover_rows = self
            .diags
            .iter()
            .filter(|diag| diag.flags & DIAG_FLAG_CROSSOVER != 0)
            .count() as u16;
        match self.report.crossover.as_ref() {
            Some(summary) => {
                if summary.diag_count != crossover_rows {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "report.crossover.diag_count",
                        format!(
                            "报告 crossover diag_count={} 与实际带 crossover flag 的 diag 行 {crossover_rows} 不一致",
                            summary.diag_count
                        ),
                    ));
                }
            }
            None => {
                if crossover_rows != 0 {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "diags.flags",
                        format!(
                            "diag 行含 DIAG_FLAG_CROSSOVER 但报告无 crossover 摘要（{crossover_rows} 行）"
                        ),
                    ));
                }
            }
        }
        let mut previous_role: Option<u16> = None;
        for (index, role) in self.roles.iter().enumerate() {
            if role.role_id as usize >= MAX_OUTPUT_ROLES {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("roles[{index}].role_id"),
                    "role_id 越界",
                ));
            }
            if role.out_index != u16::MAX && role.out_index >= header.n_out {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("roles[{index}].out_index"),
                    "out_index 越界",
                ));
            }
            if role.flags & !ROLE_FLAG_MASK != 0
                || role.reserved0 != 0
                || role.reserved1 != 0
                || role.reserved2 != 0
                || role.reserved3 != 0
                || role.reserved4 != 0
            {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("roles[{index}].flags"),
                    "role flags/reserved 非法",
                ));
            }
            if role.polarity != 1 && role.polarity != -1 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("roles[{index}].polarity"),
                    "role polarity 必须 ±1",
                ));
            }
            // F-38：与入口 compile_plan / RoleCompileInput::validate 使用同一条规则——
            // plan ABI v2 的 role.peq_count 必须与 IIR 表项段数一致；无表项（含 v1 plan）
            // 的 peq_count != 0 一律拒收坏图（禁止静默丢弃 PEQ）。
            match self
                .iir_entries
                .iter()
                .find(|entry| entry.role_id == role.role_id)
            {
                Some(entry) => {
                    if role.peq_count != entry.section_count {
                        return Err(ca_err(
                            E_CA_PLAN_VERIFY,
                            format!("roles[{index}].peq_count"),
                            format!(
                                "role.peq_count={} != IIR 表项 section_count={}（plan ABI v2 role/IIR 绑定不一致）",
                                role.peq_count, entry.section_count
                            ),
                        ));
                    }
                }
                None => {
                    if role.peq_count != 0 {
                        return Err(ca_err(
                            E_CA_PLAN_VERIFY,
                            format!("roles[{index}].peq_count"),
                            "peq_count != 0 但 IIR 表无该 role 表项（v1 plan 无系数区；v2 必须成对出现；F-38）",
                        ));
                    }
                }
            }
            if role.delay_samples as u64 > header.sample_rate as u64 / 5 {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    format!("roles[{index}].delay_samples"),
                    "role 延迟超过 200 ms（floor(0.2*fs)）",
                ));
            }
            if let Some(previous) = previous_role {
                if role.role_id <= previous {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        "roles",
                        "role 表必须按 role_id 严格升序",
                    ));
                }
            }
            previous_role = Some(role.role_id);
        }

        // plan ABI v2 IIR 表自校验（GAP-T66-3；与 T20 swap::parse_plan_arena 同口径）：
        // 表项排序/role 绑定/段数/系数区间、下发 f32 系数有限 + biquad 稳定、表 CRC-32C
        // （表头 crc 置 0 + 表项 + 按表项顺序的系数 blob）与字节重建一致。
        if !self.iir_entries.is_empty() {
            let expected_table_len =
                PLAN_IIR_HEADER_BYTES + self.iir_entries.len() * PLAN_IIR_ENTRY_BYTES;
            if self.iir_table.len() != expected_table_len {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "iir_table",
                    "IIR 表长度与表项数不一致",
                ));
            }
            let rebuilt_table = iir_table_bytes(&self.iir_entries, &self.iir_blobs)?;
            if rebuilt_table != self.iir_table {
                return Err(ca_err(
                    E_CA_PLAN_VERIFY,
                    "iir_table",
                    "IIR 表字节与表项/系数 blob 不一致（表头/表项/CRC 被篡改）",
                ));
            }
            let mut previous_iir_role: Option<u16> = None;
            for (index, entry) in self.iir_entries.iter().enumerate() {
                if entry.order != 2
                    || entry.flags != 0
                    || entry.reserved0 != 0
                    || entry.reserved1 != 0
                    || entry.reserved2 != 0
                {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        format!("iir_entries[{index}]"),
                        "order 必须 2（biquad）且 flags/reserved 必须 0",
                    ));
                }
                if entry.section_count == 0
                    || entry.section_count as usize > PLAN_IIR_MAX_SECTIONS
                {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        format!("iir_entries[{index}].section_count"),
                        "IIR 段数必须在 1..=32",
                    ));
                }
                let expected_len =
                    entry.section_count as u64 * 4 * PLAN_IIR_SECTION_COEFFS as u64;
                if entry.coeff_len_bytes as u64 != expected_len {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        format!("iir_entries[{index}].coeff_len_bytes"),
                        "IIR 系数长度必须 = section_count*5*4",
                    ));
                }
                if entry.coeff_offset_bytes % COEFF_ALIGN_BYTES as u64 != 0
                    || entry.coeff_offset_bytes + entry.coeff_len_bytes as u64
                        > total_coeff as u64
                {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        format!("iir_entries[{index}].coeff_offset_bytes"),
                        "IIR 系数区间越界/未 64B 对齐",
                    ));
                }
                if let Some(previous) = previous_iir_role {
                    if entry.role_id <= previous {
                        return Err(ca_err(
                            E_CA_PLAN_VERIFY,
                            "iir_entries",
                            "IIR 表项必须按 role_id 严格升序",
                        ));
                    }
                }
                previous_iir_role = Some(entry.role_id);
                // 下发系数有限 + biquad 稳定（host RT 硬门同一口径；f32 量化后仍须稳定）。
                parse_encoded_peq_sections(
                    E_CA_PLAN_VERIFY,
                    &self.coeffs,
                    entry.coeff_offset_bytes,
                    entry.coeff_len_bytes,
                )?;
            }
            for role in &self.roles {
                if role.peq_count != 0
                    && !self
                        .iir_entries
                        .iter()
                        .any(|entry| entry.role_id == role.role_id)
                {
                    return Err(ca_err(
                        E_CA_PLAN_VERIFY,
                        format!("roles[{}].peq_count", role.role_id),
                        "role.peq_count>0 但 IIR 表无对应表项（禁止静默丢弃 PEQ）",
                    ));
                }
            }
        }

        // graph_id / CRC / 字节重建。
        let expected_graph = compute_graph_id(
            header,
            &self.paths,
            &self.diags,
            &self.roles,
            &[],
            &self.ctl_meta,
            &self.coeff_blobs,
            &self.iir_table,
            &self.iir_blobs,
        )?;
        if expected_graph != self.graph_id || header.graph_id != self.graph_id {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "graph_id",
                "graph_id 与内容不符（ERR_HASH）",
            ));
        }
        let mut crc_header = *header;
        crc_header.header_crc32c = 0;
        if crc32c(&header_bytes(&crc_header)) != header.header_crc32c {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "header.header_crc32c",
                "header CRC-32C 不符",
            ));
        }
        let mut rebuilt: Vec<u8> = Vec::with_capacity(self.plan_bytes.len());
        rebuilt.extend_from_slice(&header_bytes(header));
        rebuilt.extend_from_slice(&path_table_bytes(&self.paths));
        rebuilt.extend_from_slice(&diag_table_bytes(&self.diags));
        rebuilt.extend_from_slice(&role_table_bytes(&self.roles));
        rebuilt.extend_from_slice(&self.ctl_meta);
        if !self.iir_entries.is_empty() {
            let iir_offset = header.reserved3[0] as usize;
            while rebuilt.len() < iir_offset {
                rebuilt.push(0);
            }
            rebuilt.extend_from_slice(&self.iir_table);
        }
        rebuilt.extend_from_slice(&self.coeffs);
        if rebuilt != self.plan_bytes {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "plan_bytes",
                "plan 字节与表/系数区不一致（坏图/被篡改）",
            ));
        }
        // report.iir 必须与 plan 的 IIR 表存在性/规模一致（无 IIR 时 None）。
        let report_iir_ok = match self.report.iir.as_ref() {
            Some(iir) => {
                !self.iir_entries.is_empty()
                    && iir.role_count as usize == self.iir_entries.len()
                    && iir.abi_version == header.abi_version
            }
            None => self.iir_entries.is_empty(),
        };
        if self.report.graph_id != self.graph_id
            || self.report.plan_sha256 != sha256_hex(&self.plan_bytes)
            || self.report.paths.len() != self.paths.len()
            || self.report.roles.len() != self.roles.len()
            || !report_iir_ok
        {
            return Err(ca_err(
                E_CA_PLAN_VERIFY,
                "report",
                "编译报告与打包产物不一致",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 编译报告构建
// ---------------------------------------------------------------------------

fn build_compile_report(
    req: &CompileApplyRequest<'_>,
    plans: &CompiledPlans,
    chosen: FusionForm,
    packed: &PackedTables,
    header: &TspiPlanHeaderV1,
    graph_id: &[u8; 32],
    plan_bytes: &[u8],
) -> CaResult<CompileReport> {
    let selection_reason = if req.options.force_form.is_some() {
        "forced_by_caller"
    } else {
        selection_reason_str(plans.report.reason)
    };
    let facts = CompileFacts {
        chosen_form: fusion_form_str(chosen),
        selection_reason,
        fused_len_gp: plans.report.fused_len_gp,
        full_path_len: plans.report.full_path_len,
        fused_feasible: plans.report.fused_feasible,
        route_count: plans.report.route_count,
        factorized_macs_per_sample: plans.report.factorized.macs_per_sample,
        fused_macs_per_sample: plans.report.fused.macs_per_sample,
        saved_macs_per_sample: plans.report.saved_macs_per_sample,
        factorized_flops_per_second: plans.report.factorized.flops_per_second,
        fused_flops_per_second: plans.report.fused.flops_per_second,
        taps_limit: plans.report.taps_limit,
        extended_taps_admitted: plans.report.extended_taps_admitted,
        coeff_budget_bytes: plans.report.coeff_budget_bytes,
    };
    let role_ids: Vec<String> = req.roles.iter().map(|role| role.role_id.clone()).collect();
    let d7_warnings = req.headroom.validate(&role_ids)?;
    // crossover 摘要与真正下发的 diag flags 双向核对（证据有段 <=> plan 有 flag）。
    let crossover = match req.crossover {
        Some(evidence) => {
            let diag_count = packed
                .diags
                .iter()
                .filter(|diag| diag.flags & DIAG_FLAG_CROSSOVER != 0)
                .count() as u16;
            if diag_count == 0 {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    "packed.diags.flags",
                    "crossover 证据存在但 plan diag 行无 DIAG_FLAG_CROSSOVER（flag/证据不一致）",
                ));
            }
            if diag_count as usize != evidence.roles.len() {
                return Err(ca_err(
                    E_CA_CROSSOVER,
                    "packed.diags.flags",
                    format!(
                        "crossover diag 行数 {diag_count} != 证据 role 数 {}（flag/证据不一致）",
                        evidence.roles.len()
                    ),
                ));
            }
            let p_taps_max = evidence
                .roles
                .iter()
                .map(|record| record.composed_taps as u32)
                .max()
                .unwrap_or(0);
            Some(CompileCrossoverSummary {
                evidence_sha256: evidence.evidence_sha256(),
                design_report_sha256: evidence.design_report_sha256.clone(),
                kind: evidence.kind.clone(),
                fc_hz: evidence.fc_hz,
                taps: evidence.taps as u32,
                diag_count,
                roles: evidence
                    .roles
                    .iter()
                    .map(|record| (record.role_id.clone(), record.branch.clone()))
                    .collect(),
                p_taps_max,
            })
        }
        None => None,
    };
    // plan ABI v2 per-role IIR 摘要（仅在有 T64 PEQ 段时 Some；无 PEQ 报告字节不变）。
    let iir = if packed.iir_entries.is_empty() {
        None
    } else {
        let mut roles = Vec::with_capacity(packed.iir_entries.len());
        let mut section_count = 0u32;
        for entry in &packed.iir_entries {
            let role_id = packed
                .roles
                .iter()
                .find(|role| role.role_id == entry.role_id)
                .and_then(|role| req.role(role.role_id))
                .map(|role| role.role_id.clone())
                .unwrap_or_else(|| format!("role{}", entry.role_id));
            section_count += entry.section_count as u32;
            roles.push((role_id, entry.section_count));
        }
        Some(CompileIirSummary {
            abi_version: header.abi_version,
            role_count: packed.iir_entries.len() as u16,
            section_count,
            table_sha256: sha256_hex(&packed.iir_table),
            table_offset: header.reserved3[0],
            roles,
        })
    };
    Ok(CompileReport {
        sample_rate_hz: req.sample_rate_hz,
        block_size: req.block_size,
        n_in: req.n_in,
        container_channels: req.container_channels,
        workers: req.options.workers,
        encoding: req.encoding.as_str(),
        layout_id: req.mapping.layout_id.clone(),
        mapping_hash: req.mapping.mapping_hash(),
        overlay_source_sha256: req.mapping.overlay_source_sha256.clone(),
        binding_hash: req.mapping.binding_hash.clone(),
        role_hash: req.mapping.role_hash.clone(),
        participants_binding_hash: req.mapping.participants_binding_hash.clone(),
        stereo_strict: req.mapping.stereo_strict,
        stereo_mask: req.mapping.stereo_mask.clone(),
        dense_to_dac: req.mapping.dense_to_dac.clone(),
        unused_outputs: req.mapping.unused_outputs.clone(),
        phase_hash: req.phase.phase_hash(),
        phase_ops_sha256: req.phase.ops_sha256.clone(),
        phase_design_report_sha256: req.phase.design_report_sha256.clone(),
        phase_reference_channel: req.phase.reference_channel.clone(),
        phase_residual_max_samples: req.phase.residual_max_samples,
        phase_tolerance_samples: req.phase.tolerance_samples,
        phase_direction_ok: req.phase.direction_ok,
        phase_channels: req.phase.channels.clone(),
        headroom_hash: req.headroom.headroom_hash(),
        headroom_report_sha256: req.headroom.report_sha256.clone(),
        headroom_measured_peak_dbfs: req.headroom.measured_peak_dbfs,
        d7_warnings,
        artifact_hash: req.artifact.artifact_hash.clone(),
        artifact_manifest_sha256: req.artifact.manifest_sha256.clone(),
        artifact_wav_aggregate_sha256: req.artifact.wav_aggregate_sha256.clone(),
        artifact_solution_hash: req.artifact.solution_hash.clone(),
        artifact_design_plant_hash: req.artifact.design_plant_hash.clone(),
        artifact_target_sha256: req.artifact.target_sha256.clone(),
        artifact_baseline_hash: req.artifact.baseline_hash.clone(),
        artifact_status: req.artifact.status.clone(),
        host_graph_sha256: hex32(graph_id),
        graph_id: *graph_id,
        plan_sha256: sha256_hex(plan_bytes),
        plan_version: header.plan_version,
        path_count: header.path_count,
        diag_count: header.diag_fir_count,
        role_count: header.role_table_count,
        low_band_count: header.low_band_count,
        d_samples: req.low_band_d,
        l_g: req.l_g,
        l_p: req.l_p,
        max_taps: header.max_taps,
        total_coeff_bytes: header.total_coeff_bytes,
        facts,
        paths: packed.path_records.clone(),
        roles: packed.role_records.clone(),
        crossover,
        iir,
    })
}

// ---------------------------------------------------------------------------
// 热更 apply 会话：prepare -> commit -> all-ack -> T3 -> last-good rollback
// ---------------------------------------------------------------------------

/// 一次已生效（ack 完成）的 plan 记录（last-good 回滚目标）。
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedPlan {
    /// plan 版本。
    pub plan_version: PlanVersion,
    /// graph_id。
    pub graph_id: [u8; 32],
    /// plan 字节（回滚重新准入用；只自研产物，不含任何外部进程/系数）。
    pub plan_bytes: Vec<u8>,
    /// 编译报告 sha256。
    pub report_sha256: String,
    /// T3 应用后复测是否通过。
    pub t3_confirmed: bool,
}

/// 事件记录（core-v1 §8.2 kind；NONRT 导出，不写 RT 事件环）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyEvent {
    /// 事件 kind（1..=40）。
    pub kind: u16,
    /// value_a。
    pub value_a: u64,
    /// value_b。
    pub value_b: u64,
}

impl ApplyEvent {
    /// 规范事件名。
    pub fn label(&self) -> &'static str {
        event_name(self.kind)
    }
}

/// prepare 回执（admission 通过但未生效；actual version 尚未改变）。
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedPlan {
    /// T20 registry 分配的 plan 版本。
    pub plan_version: PlanVersion,
    /// graph_id。
    pub graph_id: [u8; 32],
    /// 显示用 planned version（未生效）。
    pub planned_version: String,
    /// expected workers。
    pub expected_workers: u8,
    /// ACK_MASK(expected)。
    pub ack_mask: u32,
    /// arena 字节。
    pub arena_bytes: u64,
    /// 单图 PlanArena 字节。
    pub graph_bytes: u64,
    /// 预算合计字节（admission 模型）。
    pub total_budget_bytes: u64,
    /// path 数。
    pub path_count: u32,
    /// diag 数。
    pub diag_count: u32,
    /// admission 摘要（人读；有界）。
    pub admission_note: String,
}

/// apply ack（§5.3：plan_version 已在完整 block + all-ack 后才允许返回）。
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyAck {
    /// 实际生效 plan 版本。
    pub plan_version: PlanVersion,
    /// 之前生效版本（0 = 无）。
    pub previous_plan_version: PlanVersion,
    /// graph_id hex（64 字符）。
    pub graph_id_hex: String,
    /// expected workers。
    pub expected_workers: u8,
    /// 已 ack 位图。
    pub ack_mask: u32,
    /// 完成 all-ack 的 block 数。
    pub blocks_committed: u64,
    /// 切换所在 block。
    pub switched_at_block: u64,
    /// 是否回滚（本 ack 恒 false；回滚走 RollbackReceipt）。
    pub rolled_back: bool,
    /// 是否已生效。
    pub applied: bool,
    /// 是否必须 T3 应用后复测才能 CONFIRMED。
    pub t3_required: bool,
}

/// commit 回执（已 commit，等待新版本 all-ack）。
#[derive(Clone, Debug, PartialEq)]
pub struct CommitReceipt {
    /// 待 ack 版本。
    pub pending_version: PlanVersion,
    /// 切换所在 block。
    pub switched_at_block: u64,
    /// 是否等待 ack。
    pub awaiting_ack: bool,
}

/// T3 应用后复测证据（ACCEPTANCE §5 / K4；measured only，禁止 predicted 冒充）。
#[derive(Clone, Debug, PartialEq)]
pub struct T3RetestEvidence {
    /// schema（必须 = tspi-cal-t3-retest/v1）。
    pub schema: String,
    /// 复测 run id。
    pub run_id: String,
    /// 复测时的实际生效版本。
    pub applied_version: PlanVersion,
    /// 复测时的实际 graph_id hex。
    pub applied_graph_id_hex: String,
    /// 是否 measured（应用后实测；false = 不可判定/PASS_OPEN_LOOP 禁止）。
    pub measured: bool,
    /// 残余延迟最大值（samples）。
    pub residual_delay_max_samples: f64,
    /// 全通道极性是否与 YAML/T65 一致。
    pub polarity_match_all: bool,
    /// 幅频校正偏差最大值（dB）。
    pub fr_deviation_max_db: f64,
    /// 目标偏差最大值（dB；D7 允许 ≤+3 dB boost 填谷）。
    pub target_deviation_max_db: f64,
    /// PEQ 实现偏差最大值（dB）。
    pub peq_implementation_max_db: f64,
    /// 实测峰值（dBFS；必须 ≤−1）。
    pub measured_peak_dbfs: Option<f64>,
    /// holdout 是否已测并报告（K4）。
    pub holdout_present: bool,
    /// 原始证据 sha256（raw sweep/capture/报告；非空）。
    pub raw_evidence_sha256: Vec<String>,
}

/// T3 fail 码。
pub const T3_NOT_MEASURED: &str = "T3_NOT_MEASURED";
/// T3 fail 码。
pub const T3_RESIDUAL_DELAY: &str = "T3_RESIDUAL_DELAY";
/// T3 fail 码。
pub const T3_POLARITY: &str = "T3_POLARITY";
/// T3 fail 码。
pub const T3_FR_DEVIATION: &str = "T3_FR_DEVIATION";
/// T3 fail 码。
pub const T3_TARGET_DEVIATION: &str = "T3_TARGET_DEVIATION";
/// T3 fail 码。
pub const T3_PEQ_IMPLEMENTATION: &str = "T3_PEQ_IMPLEMENTATION";
/// T3 fail 码。
pub const T3_PEAK: &str = "T3_PEAK";
/// T3 fail 码。
pub const T3_HOLDOUT_MISSING: &str = "T3_HOLDOUT_MISSING";
/// T3 fail 码。
pub const T3_VERSION_MISMATCH: &str = "T3_VERSION_MISMATCH";
/// T3 fail 码。
pub const T3_EVIDENCE_MISSING: &str = "T3_EVIDENCE_MISSING";

impl T3RetestEvidence {
    /// 构造（默认 measured=false/全 0；调用方填写实测字段）。
    pub fn new(
        run_id: impl Into<String>,
        applied_version: PlanVersion,
        applied_graph_id_hex: impl Into<String>,
    ) -> Self {
        Self {
            schema: T3_RETEST_SCHEMA.to_string(),
            run_id: run_id.into(),
            applied_version,
            applied_graph_id_hex: applied_graph_id_hex.into(),
            measured: false,
            residual_delay_max_samples: 0.0,
            polarity_match_all: false,
            fr_deviation_max_db: 0.0,
            target_deviation_max_db: 0.0,
            peq_implementation_max_db: 0.0,
            measured_peak_dbfs: None,
            holdout_present: false,
            raw_evidence_sha256: Vec::new(),
        }
    }

    /// 证据自洽校验（schema/run id/graph hex/有限值/原始 hash）。
    pub fn validate(&self) -> CaResult<()> {
        if self.schema != T3_RETEST_SCHEMA {
            return Err(ca_err(
                E_CA_T3,
                "t3.schema",
                format!("schema 必须为 {T3_RETEST_SCHEMA}，得到 {:?}", self.schema),
            ));
        }
        if self.run_id.trim().is_empty() {
            return Err(ca_err(E_CA_T3, "t3.run_id", "复测 run id 必须非空"));
        }
        if self.applied_version == 0 {
            return Err(ca_err(
                E_CA_T3,
                "t3.applied_version",
                "实际生效版本必须非 0",
            ));
        }
        require_sha256(
            &self.applied_graph_id_hex,
            "t3.applied_graph_id_hex",
            E_CA_T3,
        )?;
        require_finite(
            self.residual_delay_max_samples,
            "t3.residual_delay_max_samples",
        )?;
        require_finite(self.fr_deviation_max_db, "t3.fr_deviation_max_db")?;
        require_finite(self.target_deviation_max_db, "t3.target_deviation_max_db")?;
        require_finite(
            self.peq_implementation_max_db,
            "t3.peq_implementation_max_db",
        )?;
        if let Some(peak) = self.measured_peak_dbfs {
            require_finite(peak, "t3.measured_peak_dbfs")?;
        }
        if self.raw_evidence_sha256.is_empty() {
            return Err(ca_err(
                E_CA_T3,
                "t3.raw_evidence_sha256",
                "原始证据 hash 不能为空（缺原始数据=不可判定）",
            ));
        }
        for (index, hash) in self.raw_evidence_sha256.iter().enumerate() {
            require_sha256(hash, &format!("t3.raw_evidence_sha256[{index}]"), E_CA_T3)?;
        }
        Ok(())
    }

    /// 判据明细（对照 ACCEPTANCE §5 / D7）；空 = 全过。
    pub fn failures(
        &self,
        expected_version: PlanVersion,
        expected_graph_id_hex: &str,
    ) -> Vec<&'static str> {
        let mut failures = Vec::new();
        if !self.measured {
            failures.push(T3_NOT_MEASURED);
        }
        if self.applied_version != expected_version {
            failures.push(T3_VERSION_MISMATCH);
        }
        if !self
            .applied_graph_id_hex
            .eq_ignore_ascii_case(expected_graph_id_hex)
        {
            failures.push(T3_VERSION_MISMATCH);
        }
        if self.residual_delay_max_samples.abs() > T3_RESIDUAL_DELAY_TOLERANCE_SAMPLES {
            failures.push(T3_RESIDUAL_DELAY);
        }
        if !self.polarity_match_all {
            failures.push(T3_POLARITY);
        }
        if self.fr_deviation_max_db.abs() > T3_FR_DEVIATION_TOLERANCE_DB {
            failures.push(T3_FR_DEVIATION);
        }
        if self.target_deviation_max_db.abs() > T3_TARGET_DEVIATION_DB {
            failures.push(T3_TARGET_DEVIATION);
        }
        if self.peq_implementation_max_db.abs() > T3_PEQ_TOLERANCE_DB {
            failures.push(T3_PEQ_IMPLEMENTATION);
        }
        match self.measured_peak_dbfs {
            Some(peak) => {
                if peak > T3_MEASURED_PEAK_LIMIT_DBFS {
                    failures.push(T3_PEAK);
                }
            }
            None => failures.push(T3_PEAK),
        }
        if !self.holdout_present {
            failures.push(T3_HOLDOUT_MISSING);
        }
        failures
    }
}

/// T3 复测回执。
#[derive(Clone, Debug, PartialEq)]
pub struct RetestReceipt {
    /// 是否通过（= 无 fail 码）。
    pub passed: bool,
    /// fail 码（空 = 通过）。
    pub failures: Vec<&'static str>,
    /// 实际生效版本。
    pub applied_version: PlanVersion,
    /// 会话状态。
    pub state: &'static str,
    /// 是否 measured。
    pub t3_measured: bool,
}

/// 回滚原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RollbackReason {
    /// prepare/admission 失败。
    PrepareFailed,
    /// commit 失败。
    CommitFailed,
    /// all-ack 超时/未达。
    AckTimeout,
    /// T3 应用后复测失败。
    T3Failed,
    /// 用户/控制面请求。
    OperatorRequest,
    /// 上游输入变化导致 stale。
    StaleRebind,
}

impl RollbackReason {
    /// 稳定原因码（REST/log/事件 value_b）。
    pub const fn code(self) -> u32 {
        match self {
            RollbackReason::PrepareFailed => 1,
            RollbackReason::CommitFailed => 2,
            RollbackReason::AckTimeout => 3,
            RollbackReason::T3Failed => 4,
            RollbackReason::OperatorRequest => 5,
            RollbackReason::StaleRebind => 6,
        }
    }

    /// 稳定原因名。
    pub const fn as_str(self) -> &'static str {
        match self {
            RollbackReason::PrepareFailed => "R_PREPARE_FAILED",
            RollbackReason::CommitFailed => "R_COMMIT_FAILED",
            RollbackReason::AckTimeout => "R_ACK_TIMEOUT",
            RollbackReason::T3Failed => "R_T3_FAILED",
            RollbackReason::OperatorRequest => "R_OPERATOR_REQUEST",
            RollbackReason::StaleRebind => "R_STALE_REBIND",
        }
    }
}

/// 回滚回执。
#[derive(Clone, Debug, PartialEq)]
pub struct RollbackReceipt {
    /// 回滚形态（aborted_before_commit / bounce_readmit_last_good / safety_mute）。
    pub mode: &'static str,
    /// 是否已回到安全状态（last-good 生效或 SafetyMute）。
    pub rolled_back: bool,
    /// 是否处于 SafetyMute。
    pub safety_mute: bool,
    /// SafetyMute 进入次数。
    pub safety_mute_cycles: u32,
    /// 生效版本（回滚后）。
    pub effective_version: Option<PlanVersion>,
    /// last-good graph_id hex。
    pub last_good_graph_id_hex: Option<String>,
    /// last-good 版本。
    pub last_good_version: Option<PlanVersion>,
    /// 回滚原因名。
    pub reason: &'static str,
    /// plan_version 计数是否随新 runtime session 重置（GAP-T66-6）。
    pub version_epoch_reset: bool,
    /// 非致命 warning（如 last-good 重新准入失败 -> 保持 mute）。
    pub warnings: Vec<String>,
}

struct PreparedState {
    plan: CompiledPlan,
    version: PlanVersion,
}

struct PendingCommit {
    plan: CompiledPlan,
    previous: Option<AppliedPlan>,
    ack: ApplyAck,
}

/// 热更 apply 会话（NONRT control-plane 模型；宿主 T26 负责真实 RT 线程/设备）。
pub struct ApplySession {
    sample_rate_hz: u32,
    block_size: u16,
    epoch_id: u64,
    block_seq: u64,
    registry: PlanRegistry,
    ctl: TspiRtControlV1,
    swap_ack: TspiSwapAckV1,
    prepared: Option<PreparedState>,
    pending: Option<PendingCommit>,
    effective: Option<AppliedPlan>,
    last_good: Option<AppliedPlan>,
    ack: Option<ApplyAck>,
    t3: Option<T3RetestEvidence>,
    env: Option<AdmissionEnv>,
    state: &'static str,
    events: Vec<ApplyEvent>,
    warnings: Vec<String>,
    safety_mute_cycles: u32,
}

impl ApplySession {
    /// 新会话（空 registry/ctl/swap-ack；fs/B 与 plan 一致性由 admission 复核）。
    pub fn new(sample_rate_hz: u32, block_size: u16) -> Self {
        Self {
            sample_rate_hz,
            block_size,
            epoch_id: 1,
            block_seq: 0,
            registry: PlanRegistry::new(),
            ctl: TspiRtControlV1::new(),
            swap_ack: TspiSwapAckV1::new(),
            prepared: None,
            pending: None,
            effective: None,
            last_good: None,
            ack: None,
            t3: None,
            env: None,
            state: APPLY_STATE_IDLE,
            events: Vec::new(),
            warnings: Vec::new(),
            safety_mute_cycles: 0,
        }
    }

    /// 当前状态。
    pub fn state(&self) -> &'static str {
        self.state
    }

    /// 事件记录（按发生顺序）。
    pub fn events(&self) -> &[ApplyEvent] {
        &self.events
    }

    /// 最近一次 ack（applied 证据）。
    pub fn apply_ack(&self) -> Option<&ApplyAck> {
        self.ack.as_ref()
    }

    /// 当前生效记录。
    pub fn effective(&self) -> Option<&AppliedPlan> {
        self.effective.as_ref()
    }

    /// last-good（回滚目标）。
    pub fn last_good(&self) -> Option<&AppliedPlan> {
        self.last_good.as_ref()
    }

    /// 非致命 warning 列表。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// 汇总状态（无 T3 measured = INCOMPLETE/APPLIED_NOT_CONFIRMED，禁止 PASS）。
    pub fn overall_status(&self) -> &'static str {
        match self.state {
            APPLY_STATE_CONFIRMED => OVERALL_CONFIRMED,
            APPLY_STATE_APPLIED => OVERALL_APPLIED_NOT_CONFIRMED,
            APPLY_STATE_ROLLED_BACK => OVERALL_ROLLED_BACK,
            APPLY_STATE_SAFETY_MUTE => OVERALL_SAFETY_MUTE,
            _ => OVERALL_INCOMPLETE,
        }
    }

    fn push_event(&mut self, kind: u16, value_a: u64, value_b: u64) {
        self.events.push(ApplyEvent {
            kind,
            value_a,
            value_b,
        });
    }

    /// prepare：自校验 + T18 admission 模型 + T20 registry admit（分配版本；未生效）。
    pub fn prepare_with<B: MemoryBackend>(
        &mut self,
        plan: &CompiledPlan,
        env: &AdmissionEnv,
        backend: &mut B,
    ) -> CaResult<PreparedPlan> {
        if self.state == APPLY_STATE_PREPARED || self.state == APPLY_STATE_AWAITING_ACK {
            return Err(ca_err(
                E_CA_STATE,
                "state",
                format!("当前状态 {} 不允许再次 prepare（switch busy）", self.state),
            ));
        }
        plan.verify()?;
        if plan.header.sample_rate != self.sample_rate_hz
            || plan.header.block_size != self.block_size
        {
            return Err(ca_err(
                E_CA_ADMISSION,
                "plan.header",
                format!(
                    "plan fs/B {}/{} 与会话 {}/{} 不一致（stale）",
                    plan.header.sample_rate,
                    plan.header.block_size,
                    self.sample_rate_hz,
                    self.block_size
                ),
            ));
        }
        if plan.header.sample_rate != env.sample_rate_hz {
            return Err(ca_err(
                E_CA_ADMISSION,
                "plan.header.sample_rate",
                format!(
                    "plan fs={} 与 admission env fs={} 不一致（stale）",
                    plan.header.sample_rate, env.sample_rate_hz
                ),
            ));
        }
        if plan.header.block_size != env.block_size {
            return Err(ca_err(
                E_CA_ADMISSION,
                "plan.header.block_size",
                format!(
                    "plan B={} 与 admission env B={} 不一致（stale）",
                    plan.header.block_size, env.block_size
                ),
            ));
        }
        if plan.header.workers != env.rt_workers {
            return Err(ca_err(
                E_CA_ADMISSION,
                "plan.header.workers",
                format!(
                    "plan workers={} 与 admission env workers={} 不一致（2 核是独立 profile）",
                    plan.header.workers, env.rt_workers
                ),
            ));
        }
        admission_validate(&plan.view(), env)
            .map_err(|status| ca_status(E_CA_ADMISSION, "admission.validate", status))?;
        let memory = plan_memory(&plan.view(), env)
            .map_err(|status| ca_status(E_CA_ADMISSION, "admission.plan_memory", status))?;
        let version = self
            .registry
            .admit_with(plan.token(), env, backend)
            .map_err(|status| ca_status(E_CA_ADMISSION, "registry.admit", status))?;
        let expected_workers = plan.header.workers;
        let admission_note = format!(
            "admitted:v{version} {}ch@{}Hz/B{} workers={} paths={} diags={} arena={}B",
            plan.header.n_out,
            plan.header.sample_rate,
            plan.header.block_size,
            expected_workers,
            plan.header.path_count,
            plan.header.diag_fir_count,
            memory.arena_bytes
        );
        let receipt = PreparedPlan {
            plan_version: version,
            graph_id: plan.graph_id,
            planned_version: format!("pv-{version}"),
            expected_workers,
            ack_mask: ack_mask(expected_workers),
            arena_bytes: memory.arena_bytes,
            graph_bytes: memory.graph_current_bytes,
            total_budget_bytes: memory.total_bytes,
            path_count: memory.path_count,
            diag_count: memory.diag_count,
            admission_note,
        };
        self.prepared = Some(PreparedState {
            plan: plan.clone(),
            version,
        });
        self.env = Some(*env);
        self.state = APPLY_STATE_PREPARED;
        self.push_event(
            EV_PLAN_PREPARED,
            version as u64,
            plan.header.path_count as u64,
        );
        self.push_event(EV_PLAN_ADMITTED, version as u64, memory.arena_bytes);
        Ok(receipt)
    }

    /// commit：stage -> arm -> [RT] block 边界 commit -> complete_swap（RG-1）。
    ///
    /// 返回时仍**未 applied**（必须等新版本 all-ack block）。
    pub fn commit(&mut self) -> CaResult<CommitReceipt> {
        if self.state != APPLY_STATE_PREPARED {
            return Err(ca_err(
                E_CA_STATE,
                "state",
                format!("当前状态 {} 不能 commit（需 prepared）", self.state),
            ));
        }
        let prepared = match self.prepared.take() {
            Some(prepared) => prepared,
            None => {
                return Err(ca_err(
                    E_CA_STATE,
                    "prepared",
                    "缺少 prepared plan（内部状态不一致）",
                ))
            }
        };
        let plan = prepared.plan;
        let version = prepared.version;
        let expected_workers = plan.header.workers;
        let token = self
            .registry
            .stage(version)
            .map_err(|status| ca_status(E_CA_COMMIT, "registry.stage", status))?;
        let status = self
            .registry
            .arm(token, &self.ctl, &mut self.swap_ack, self.epoch_id);
        if !status.is_ok() {
            self.prepared = Some(PreparedState { plan, version });
            return Err(ca_status(E_CA_COMMIT, "registry.arm", status));
        }
        self.push_event(EV_PLAN_STAGED, version as u64, self.epoch_id);
        let switched = commit_swap_at_block_boundary(&self.ctl)
            .map_err(|status| ca_status(E_CA_COMMIT, "commit_swap_at_block_boundary", status))?;
        let completed = self
            .registry
            .complete_swap(&self.ctl, &self.swap_ack)
            .map_err(|status| ca_status(E_CA_COMMIT, "registry.complete_swap", status))?;
        let previous_version = self
            .effective
            .as_ref()
            .map(|applied| applied.plan_version)
            .unwrap_or(0);
        self.push_event(EV_PLAN_SWAPPED, completed as u64, previous_version as u64);
        let ack = ApplyAck {
            plan_version: completed,
            previous_plan_version: previous_version,
            graph_id_hex: plan.graph_id_hex(),
            expected_workers,
            ack_mask: 0,
            blocks_committed: 0,
            switched_at_block: self.block_seq,
            rolled_back: false,
            applied: false,
            t3_required: true,
        };
        self.pending = Some(PendingCommit {
            plan,
            previous: self.effective.clone(),
            ack: ack.clone(),
        });
        self.state = APPLY_STATE_AWAITING_ACK;
        let _ = switched;
        Ok(CommitReceipt {
            pending_version: completed,
            switched_at_block: self.block_seq,
            awaiting_ack: true,
        })
    }

    /// master 侧喂入一个新版本 block ack（T09/T20 语义）：all-ack 且 retire_ready 才 applied。
    pub fn note_block_ack(&mut self, block: &TspiBlockAckV1) -> CaResult<Option<ApplyAck>> {
        if self.state != APPLY_STATE_AWAITING_ACK {
            return Err(ca_err(
                E_CA_STATE,
                "state",
                format!(
                    "当前状态 {} 不接受 block ack（需 awaiting_ack）",
                    self.state
                ),
            ));
        }
        let (pending_version, expected_workers) = match self.pending.as_ref() {
            Some(pending) => (pending.ack.plan_version, pending.ack.expected_workers),
            None => {
                return Err(ca_err(
                    E_CA_STATE,
                    "pending",
                    "缺少 pending commit（内部状态不一致）",
                ))
            }
        };
        if block.plan_version != pending_version {
            return Err(ca_err(
                E_CA_COMMIT,
                "block.plan_version",
                format!(
                    "block plan_version={} != pending {pending_version}（RT_E_PLAN_MISMATCH）",
                    block.plan_version
                ),
            ));
        }
        if block.expected_workers != expected_workers {
            return Err(ca_err(
                E_CA_COMMIT,
                "block.expected_workers",
                format!(
                    "block expected={} != plan expected={expected_workers}（RT_E_PLAN_MISMATCH）",
                    block.expected_workers
                ),
            ));
        }
        let status = self.swap_ack.note_block_all_ack(block);
        if !status.is_ok() {
            return Err(ca_status(
                E_CA_COMMIT,
                "swap_ack.note_block_all_ack",
                status,
            ));
        }
        if !self.swap_ack.is_retire_ready() {
            return Ok(None);
        }
        let pending = match self.pending.take() {
            Some(pending) => pending,
            None => {
                return Err(ca_err(
                    E_CA_STATE,
                    "pending",
                    "all-ack 后缺少 pending commit",
                ))
            }
        };
        let mut ack = pending.ack;
        ack.applied = true;
        ack.blocks_committed = 1;
        ack.ack_mask = self.swap_ack.required_mask();
        ack.switched_at_block = self.block_seq;
        let applied = AppliedPlan {
            plan_version: ack.plan_version,
            graph_id: pending.plan.graph_id,
            plan_bytes: pending.plan.plan_bytes.clone(),
            report_sha256: pending.plan.report_sha256(),
            t3_confirmed: false,
        };
        self.last_good = pending.previous;
        self.effective = Some(applied);
        self.state = APPLY_STATE_APPLIED;
        self.push_event(EV_SWAP_ACK, ack.plan_version as u64, ack.ack_mask as u64);
        self.ack = Some(ack.clone());
        Ok(Some(ack))
    }

    /// 合成一个"新版本 all-ack block"（离线模型/宿主 mock 用；真实路径由 T09 提供）。
    pub fn synthetic_all_ack_block(
        &self,
        plan_version: PlanVersion,
        expected_workers: u8,
    ) -> TspiBlockAckV1 {
        let block = TspiBlockAckV1::new(
            self.block_seq,
            self.epoch_id,
            plan_version,
            expected_workers,
        );
        let mask = ack_mask(expected_workers);
        for phase in 0..block.phase_done.len() {
            block.phase_done[phase].store(mask, Ordering::Release);
        }
        block
    }

    /// 要求已 applied：未收到 all-ack 则返回 E_CA_ACK_TIMEOUT（调用方随后 rollback）。
    pub fn require_apply_ack(&mut self) -> CaResult<ApplyAck> {
        match self.ack.as_ref() {
            Some(ack) if ack.applied => Ok(ack.clone()),
            _ => Err(ca_err(
                E_CA_ACK_TIMEOUT,
                "ack",
                format!(
                    "状态 {}：新版本 all-ack 未达，不得视为 applied（§5.3/T24）",
                    self.state
                ),
            )),
        }
    }

    /// T3 应用后复测：measured 证据全过 -> CONFIRMED；否则返回 fail 码列表（调用方 rollback）。
    pub fn record_t3_retest(&mut self, evidence: &T3RetestEvidence) -> CaResult<RetestReceipt> {
        if self.state != APPLY_STATE_APPLIED {
            return Err(ca_err(
                E_CA_STATE,
                "state",
                format!("当前状态 {} 不接受 T3 复测（需 applied）", self.state),
            ));
        }
        evidence.validate()?;
        let effective = match self.effective.as_ref() {
            Some(effective) => effective.clone(),
            None => {
                return Err(ca_err(
                    E_CA_STATE,
                    "effective",
                    "缺少 effective plan（内部状态不一致）",
                ))
            }
        };
        let failures = evidence.failures(effective.plan_version, &hex32(&effective.graph_id));
        if failures.is_empty() {
            self.t3 = Some(evidence.clone());
            let mut confirmed = effective.clone();
            confirmed.t3_confirmed = true;
            self.effective = Some(confirmed);
            self.state = APPLY_STATE_CONFIRMED;
        }
        Ok(RetestReceipt {
            passed: failures.is_empty(),
            failures,
            applied_version: effective.plan_version,
            state: self.state,
            t3_measured: evidence.measured,
        })
    }

    /// last-good 回滚（失败只自研 last-good 或 SafetyMute，不回旧 C）。
    ///
    /// - prepared（未 commit）-> abort_and_disarm，旧图保持生效；
    /// - committed/applied/confirmed -> SafetyMute + 完整 re-admission（§5.2）+ 重新提交并 all-ack
    ///   last-good plan（新 runtime session，plan_version 计数重置，GAP-T66-6）；
    /// - 无 last-good -> 只保持 SafetyMute（禁止半图输出）。
    pub fn rollback_with<B: MemoryBackend>(
        &mut self,
        reason: RollbackReason,
        backend: &mut B,
    ) -> CaResult<RollbackReceipt> {
        let reason_code = reason.code();
        match self.state {
            APPLY_STATE_PREPARED => {
                let prepared = self.prepared.take();
                if let Some(prepared) = prepared {
                    let token = StageToken {
                        version: prepared.version,
                    };
                    let status = if self.registry.is_armed() {
                        self.registry.abort_and_disarm(token, &self.ctl)
                    } else {
                        self.registry.abort(token)
                    };
                    if !status.is_ok() {
                        self.warnings.push(format!(
                            "abort 返回 RtStatus code {}（旧图仍生效；不部分切换）",
                            status.code()
                        ));
                    }
                    self.push_event(EV_PLAN_ABORTED, prepared.version as u64, reason_code as u64);
                }
                self.state = APPLY_STATE_ROLLED_BACK;
                Ok(RollbackReceipt {
                    mode: ROLLBACK_MODE_ABORTED,
                    rolled_back: true,
                    safety_mute: false,
                    safety_mute_cycles: self.safety_mute_cycles,
                    effective_version: self.effective.as_ref().map(|plan| plan.plan_version),
                    last_good_graph_id_hex: self
                        .last_good
                        .as_ref()
                        .map(|plan| hex32(&plan.graph_id)),
                    last_good_version: self.last_good.as_ref().map(|plan| plan.plan_version),
                    reason: reason.as_str(),
                    version_epoch_reset: false,
                    warnings: Vec::new(),
                })
            }
            APPLY_STATE_AWAITING_ACK | APPLY_STATE_APPLIED | APPLY_STATE_CONFIRMED => {
                let last_good = self.last_good.clone();
                let env = self.env.clone();
                let safety_mute_cycles = self.safety_mute_cycles + 1;
                self.safety_mute_cycles = safety_mute_cycles;
                // SafetyMute：只静音，不输出半图；随后走完整 re-admission（§5.2）。
                self.ctl.safety_request.store(1, Ordering::Release);
                self.push_event(
                    EV_SAFETY_MUTE_ENTER,
                    safety_mute_cycles as u64,
                    reason_code as u64,
                );
                self.registry.shutdown();
                self.registry = PlanRegistry::new();
                self.ctl = TspiRtControlV1::new();
                self.swap_ack = TspiSwapAckV1::new();
                self.epoch_id = self.epoch_id.wrapping_add(1);
                self.block_seq = 0;
                self.prepared = None;
                self.pending = None;
                self.effective = None;
                self.ack = None;
                self.state = APPLY_STATE_SAFETY_MUTE;
                self.push_event(EV_EPOCH_START, self.epoch_id, reason_code as u64);
                let (last_good, env) = match (last_good, env) {
                    (Some(last_good), Some(env)) => (last_good, env),
                    _ => {
                        self.warnings.push(
                            "no last-good plan: stay in SafetyMute (no partial graph output)"
                                .to_string(),
                        );
                        return Ok(RollbackReceipt {
                            mode: ROLLBACK_MODE_SAFETY_MUTE,
                            rolled_back: true,
                            safety_mute: true,
                            safety_mute_cycles,
                            effective_version: None,
                            last_good_graph_id_hex: None,
                            last_good_version: None,
                            reason: reason.as_str(),
                            version_epoch_reset: false,
                            warnings: Vec::new(),
                        });
                    }
                };
                // 完整 re-admission：fresh registry 重新准入 last-good 字节（同 graph_id 内容）。
                let token = PlanToken::new(
                    PlanBuf::new(last_good.plan_bytes.clone()),
                    last_good.graph_id,
                );
                let version = match self.registry.admit_with(token, &env, backend) {
                    Ok(version) => version,
                    Err(status) => {
                        self.warnings.push(format!(
                            "last-good re-admission failed: RtStatus code {}; stay in SafetyMute",
                            status.code()
                        ));
                        return Ok(RollbackReceipt {
                            mode: ROLLBACK_MODE_SAFETY_MUTE,
                            rolled_back: true,
                            safety_mute: true,
                            safety_mute_cycles,
                            effective_version: None,
                            last_good_graph_id_hex: Some(hex32(&last_good.graph_id)),
                            last_good_version: Some(last_good.plan_version),
                            reason: reason.as_str(),
                            version_epoch_reset: false,
                            warnings: Vec::new(),
                        });
                    }
                };
                let applied = match self.recommit_all_ack(version, &last_good, &env) {
                    Ok(applied) => applied,
                    Err(error) => {
                        self.warnings.push(format!(
                            "last-good re-commit failed: {error}; stay in SafetyMute"
                        ));
                        self.state = APPLY_STATE_SAFETY_MUTE;
                        self.effective = None;
                        return Ok(RollbackReceipt {
                            mode: ROLLBACK_MODE_SAFETY_MUTE,
                            rolled_back: true,
                            safety_mute: true,
                            safety_mute_cycles,
                            effective_version: None,
                            last_good_graph_id_hex: Some(hex32(&last_good.graph_id)),
                            last_good_version: Some(last_good.plan_version),
                            reason: reason.as_str(),
                            version_epoch_reset: false,
                            warnings: Vec::new(),
                        });
                    }
                };
                self.push_event(EV_ROLLBACK, applied.plan_version as u64, reason_code as u64);
                self.push_event(
                    EV_SAFETY_MUTE_EXIT,
                    safety_mute_cycles as u64,
                    applied.plan_version as u64,
                );
                self.state = APPLY_STATE_ROLLED_BACK;
                Ok(RollbackReceipt {
                    mode: ROLLBACK_MODE_READMIT_LAST_GOOD,
                    rolled_back: true,
                    safety_mute: false,
                    safety_mute_cycles,
                    effective_version: Some(applied.plan_version),
                    last_good_graph_id_hex: Some(hex32(&last_good.graph_id)),
                    last_good_version: Some(last_good.plan_version),
                    reason: reason.as_str(),
                    version_epoch_reset: true,
                    warnings: Vec::new(),
                })
            }
            _ => Err(ca_err(
                E_CA_ROLLBACK,
                "state",
                format!("当前状态 {} 无可回滚的切换", self.state),
            )),
        }
    }

    fn recommit_all_ack(
        &mut self,
        version: PlanVersion,
        last_good: &AppliedPlan,
        env: &AdmissionEnv,
    ) -> CaResult<AppliedPlan> {
        let _ = env;
        let token = self
            .registry
            .stage(version)
            .map_err(|status| ca_status(E_CA_ROLLBACK, "rollback.stage", status))?;
        let status = self
            .registry
            .arm(token, &self.ctl, &mut self.swap_ack, self.epoch_id);
        if !status.is_ok() {
            return Err(ca_status(E_CA_ROLLBACK, "rollback.arm", status));
        }
        commit_swap_at_block_boundary(&self.ctl)
            .map_err(|status| ca_status(E_CA_ROLLBACK, "rollback.commit", status))?;
        self.registry
            .complete_swap(&self.ctl, &self.swap_ack)
            .map_err(|status| ca_status(E_CA_ROLLBACK, "rollback.complete_swap", status))?;
        let expected = self
            .registry
            .plan_view(version)
            .map(|view| view.header.workers)
            .unwrap_or(RT_WORKERS);
        let block = self.synthetic_all_ack_block(version, expected);
        let status = self.swap_ack.note_block_all_ack(&block);
        if !status.is_ok() {
            return Err(ca_status(E_CA_ROLLBACK, "rollback.all_ack", status));
        }
        if !self.swap_ack.is_retire_ready() {
            return Err(ca_err(
                E_CA_ROLLBACK,
                "rollback.all_ack",
                "last-good 重新提交后 all-ack 未达（禁止视为生效）",
            ));
        }
        let applied = AppliedPlan {
            plan_version: version,
            graph_id: last_good.graph_id,
            plan_bytes: last_good.plan_bytes.clone(),
            report_sha256: last_good.report_sha256.clone(),
            t3_confirmed: last_good.t3_confirmed,
        };
        self.last_good = Some(applied.clone());
        self.effective = Some(applied.clone());
        self.ack = Some(ApplyAck {
            plan_version: version,
            previous_plan_version: last_good.plan_version,
            graph_id_hex: hex32(&last_good.graph_id),
            expected_workers: expected,
            ack_mask: self.swap_ack.required_mask(),
            blocks_committed: 1,
            switched_at_block: self.block_seq,
            rolled_back: true,
            applied: true,
            t3_required: true,
        });
        Ok(applied)
    }

    /// apply ack JSON（确定性；T42/T70 消费）。
    pub fn apply_ack_json(&self) -> Option<String> {
        let ack = self.ack.as_ref()?;
        let mut text = String::new();
        text.push_str("{\n");
        let _ = writeln!(text, "  \"schema\": \"{APPLY_ACK_SCHEMA}\",");
        let _ = writeln!(
            text,
            "  \"tool\": {{ \"name\": \"{COMPILE_APPLY_TOOL_NAME}\", \"version\": \"{COMPILE_APPLY_TOOL_VERSION}\" }},"
        );
        let _ = writeln!(text, "  \"state\": \"{}\",", self.state);
        let _ = writeln!(text, "  \"overall_status\": \"{}\",", self.overall_status());
        let _ = writeln!(text, "  \"plan_version\": {},", ack.plan_version);
        let _ = writeln!(
            text,
            "  \"previous_plan_version\": {},",
            ack.previous_plan_version
        );
        let _ = writeln!(text, "  \"graph_id\": \"{}\",", ack.graph_id_hex);
        let _ = writeln!(text, "  \"expected_workers\": {},", ack.expected_workers);
        let _ = writeln!(text, "  \"ack_mask\": {},", ack.ack_mask);
        let _ = writeln!(text, "  \"blocks_committed\": {},", ack.blocks_committed);
        let _ = writeln!(text, "  \"switched_at_block\": {},", ack.switched_at_block);
        let _ = writeln!(text, "  \"applied\": {},", ack.applied);
        let _ = writeln!(text, "  \"rolled_back\": {},", ack.rolled_back);
        let _ = writeln!(text, "  \"t3_required\": {},", ack.t3_required);
        let _ = writeln!(
            text,
            "  \"t3_confirmed\": {},",
            self.effective
                .as_ref()
                .map(|plan| plan.t3_confirmed)
                .unwrap_or(false)
        );
        let _ = writeln!(text, "  \"state\": \"{}\",", self.state);
        text.push_str("  \"device_owner\": \"calibration\",\n");
        text.push_str("  \"camilla_involved\": false,\n");
        text.push_str("  \"events\": [");
        for (index, event) in self.events.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            let _ = write!(
                text,
                "{{\"kind\": {}, \"name\": \"{}\", \"value_a\": {}, \"value_b\": {}}}",
                event.kind,
                event.label(),
                event.value_a,
                event.value_b
            );
        }
        text.push_str("]\n}\n");
        Some(text)
    }

    /// apply ack artifact（T60 job 登记用；无 ack 时 None）。
    pub fn apply_ack_artifact(&self) -> Option<CompileArtifact> {
        let bytes = self.apply_ack_json()?.into_bytes();
        Some(CompileArtifact {
            kind: ArtifactKind::ApplyReport,
            path: APPLY_ACK_ARTIFACT_PATH.to_string(),
            sha256: sha256_hex(&bytes),
            bytes,
        })
    }

    /// 会话状态快照（人读）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(
            "state={} overall={} epoch={}",
            self.state,
            self.overall_status(),
            self.epoch_id
        ));
        match self.effective.as_ref() {
            Some(plan) => lines.push(format!(
                "effective version={} graph_id={} t3_confirmed={}",
                plan.plan_version,
                hex32(&plan.graph_id),
                plan.t3_confirmed
            )),
            None => lines.push("effective=none".to_string()),
        }
        match self.last_good.as_ref() {
            Some(plan) => lines.push(format!(
                "last_good version={} graph_id={}",
                plan.plan_version,
                hex32(&plan.graph_id)
            )),
            None => lines.push("last_good=none".to_string()),
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// 私有测试（include! tests/T66；integrator 不要再注册进 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod t66_tests {
    use super::*;

    include!("../tests/T66/compile_apply_cases.rs");
    include!("../tests/T66/peq_plan_v2.rs");
}

// 私有测试（include! tests/T68；单点 G 实验旁路 EXPERIMENT/NON_ACCEPTABLE）
#[cfg(test)]
mod t68_experiment_tests {
    use super::*;

    include!("../tests/T68/experiment_single_point.rs");
}
