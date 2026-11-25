//! [NONRT] T67 前置：JobManager -> FitP(T64/T65) -> T66 compile/apply 端到端接线（StageDelegate 实现）。
//!
//! 本模块把 T60 JobManager 的 stage 状态机与 T64/T65/T66 的 leaf 入口接起来，补齐
//! stage_backend 测量段之后仍未接线的 FitP/SolveG/Compile/Prepared/Apply 五段：
//!
//!   JobManager Full job
//!     Preflight/SweepCapture/QualityCheck  -> StageBackend（T67 前置，真实测量段；本模块不改）
//!     FitP  (delegate) -> T64 fit_peq + T65 fit_phase -> fit/*.json + phase/*.json artifact
//!     SolveG(delegate) -> G 每 path 输入（T54/T50 上游产物；离线接线包显式声明来源）
//!     Compile(delegate)-> T66 compile_plan -> compile/plan.bin + compile/compile-report.json
//!     Prepared(delegate)-> T66 ApplySession::prepare_with（T18 admission 模型 + T20 registry）
//!     Apply (delegate) -> T66 commit + all-ack -> apply/apply-ack.json，并在 ack 后才 set_applied_version
//!     Remeasure/IndependentHoldout -> StageBackend（真实测量段；应用后复测证据归 T67/T68 流程）
//!
//! # 关键纪律（不得违反）
//!
//! 1. **不伪造测量**：FitP 的输入（plan/target/测量曲线/复响应）由调用方以
//!    [FitWiringDesign] 显式给出。生产路径应由 T63 多点/质量 + T62/T49 复响应提供；
//!    本机离线 dry-run 用 [synthetic_design] 夹具（summary artifact 里显式
//!    design_source=synthetic_offline / measured=false / simulated=true）。
//! 2. **不伪造 PASS**：任一 fit/phase/compile/apply 门失败都以稳定错误码返回 StageFailure，
//!    JobManager 按失败 stage 定位；apply 只有在 T66 all-ack 后才调用 ctx.set_applied_version。
//! 3. **apply 语义**：ack 由 [ApplyAckSource] 提供；默认 [OfflineSyntheticAck] 是 T66 明确
//!    标注的 NONRT 离线模型（不是真实 RT host）。all-ack 未达时按 T66 rollback 回
//!    last-good / SafetyMute 并如实失败（E_CA_ACK_TIMEOUT），绝不视为 applied。
//! 4. **T3 不在本模块**：apply ack 只表示版本切换完成；apply-ack.json 的
//!    overall_status=APPLIED_NOT_CONFIRMED、t3_required=true。应用后实测/独立 holdout 是
//!    T67/T68 的板端窗口工作，本模块的 apply 绝不写 CONFIRMED/PASS。
//! 5. **checkpoint 可重入**：进程崩溃后 resume 会从 in-progress stage 重跑；FitP 之前的内存产物
//!    不跨进程保留，因此在缺少 upstream 内存产物时返回 E_WIRING_RESUME_PENDING（要求重跑 job），
//!    不猜、不静默跳过。
//!
//! # 写域 / 状态
//!
//! 本模块是 T67 前置与 fit-wiring 接线（team task-190）写域；**未改** stage_backend.rs /
//! peq_fit.rs / phase_fit.rs / compile_apply.rs / job.rs / Cargo.toml（lib.rs 只 +1 行模块注册，
//! 若 integrator 需要统一注册位置可直接调整，不影响本模块 API）。真实测量/真实 apply/板端
//! T67-T69 全部 **NOT_RUN**（见 evidence/cursor-fix-fitwiring/NOT-RUN.md）。

use khaen_compat::ws::{json_encode, json_object, JsonValue};
use khaen_core::abi::KhaenBlockAckV1;
use khaen_core::rt::{PlanVersion, RT_WORKERS};
use khaen_modal_design::artifact::ArtifactStatus;
use khaen_rt_host::admission::{AdmissionEnv, MockMemory};

use crate::compile_apply::{
    compile_plan, p_row_chain_peak_gain_db, p_row_max_positive_db, ApplyAck, ApplySession,
    ArtifactBinding, CompileApplyOptions, CompileApplyRequest, CompiledPlan, CrossoverEvidence,
    CrossoverRoleEvidence, DelayPolarityEncoding, ExperimentMode, GPathInput, HeadroomEvidence,
    MappingBinding, PhaseEvidence, PreparedPlan, RoleBoostRecord, RoleCompileInput, RollbackReason,
    RollbackReceipt, EXPERIMENT_ACCEPTABILITY, EXPERIMENT_SCHEMA, EXPERIMENT_VERDICT,
    PEQ_FIT_SCHEMA as COMPILE_PEQ_FIT_SCHEMA,
};
use crate::crossover_post::{
    compose_role_p_post, kernel_sha256, CrossoverSpecConfig, CrossoverWiring, E_MEASURED_CROSSOVER,
};
use crate::job::{sha256_hex, ArtifactKind, JobStage, StageCtx, StageFailure};
use crate::peq_fit::{
    fit_peq, ChannelBand, FitArtifacts, PeqFitOptions, PeqFitOutput, PeqFitRequest, PositionCurve,
};
use crate::phase_fit::{
    fit_phase, DePhasedResponse, PhaseChannelBand, PhaseChannelIndex, PhaseChannelOp,
    PhaseFitArtifacts, PhaseFitOptions, PhaseFitOutput, PhaseFitRequest,
};
use crate::positions::{MultipointPlan, PositionQualityReport};
use crate::stage_backend::StageDelegate;
use khaen_core::compile::FusionForm;
use khaen_designer::phase::Polarity;

// ---------------------------------------------------------------------------
// schema / 常量 / 稳定错误码
// ---------------------------------------------------------------------------

/// 接线证据 artifact schema（fitp/compile/apply summary）。
pub const WIRING_SCHEMA: &str = "khaen-cal-fit-wiring/v1";
/// FitP 阶段接线摘要 artifact（job artifacts_dir 下）。
pub const FITP_SUMMARY_ARTIFACT_PATH: &str = "wiring/fitp-summary.json";
/// SolveG 阶段 G 来源 artifact。
pub const SOLVE_G_ARTIFACT_PATH: &str = "solve/g-source.json";
/// Compile 阶段接线摘要 artifact。
pub const COMPILE_SUMMARY_ARTIFACT_PATH: &str = "wiring/compile-summary.json";
/// Prepared 阶段 prepare 回执 artifact。
pub const PREPARED_ARTIFACT_PATH: &str = "prepared/prepared-plan.json";
/// Apply 阶段接线摘要 artifact。
pub const APPLY_SUMMARY_ARTIFACT_PATH: &str = "wiring/apply-summary.json";
/// Apply 回滚证据 artifact（ack 未达 -> T66 rollback 回执）。
pub const ROLLBACK_ARTIFACT_PATH: &str = "apply/rollback.json";
/// Host plan-arena apply 证据 artifact（真实 host 进程 all-ack；T67 补齐）。
pub const HOST_APPLY_ARTIFACT_PATH: &str = "apply/host-apply.json";

/// 接线输入结构非法（plan/mapping/participants/compile 配置互不一致）。
pub const E_WIRING_DESIGN: &str = "E_WIRING_DESIGN";
/// stage 顺序/重入前提缺失（resume 时内存上游产物不在）。
pub const E_WIRING_RESUME_PENDING: &str = "E_WIRING_RESUME_PENDING";
/// T64 fit 门失败（T64 report checks 未全过）。
pub const E_WIRING_FIT_GATE: &str = "E_WIRING_FIT_GATE";
/// T65 phase 门失败（T65 report checks 未全过）。
pub const E_WIRING_PHASE_GATE: &str = "E_WIRING_PHASE_GATE";
/// G 每 path 输入不满足 T66 编译前提。
pub const E_WIRING_SOLVE_G: &str = "E_WIRING_SOLVE_G";
/// 本 delegate 不负责的 stage。
pub const E_WIRING_STAGE: &str = "E_WIRING_STAGE";
/// crossover P_post 组合核超出 plan taps 预算（禁止截尾；extended 不在本次接线内）。
pub const E_WIRING_CROSSOVER_BUDGET: &str = "E_WIRING_CROSSOVER_BUDGET";
/// T64 PEQ 段 → T66 plan ABI v2 IIR 区接线失败（a0 未归一化/系数不稳定/段数绑定失败）。
pub const E_WIRING_PEQ_PLAN: &str = "E_WIRING_PEQ_PLAN";

fn wiring_fail(code: &str, message: impl Into<String>) -> StageFailure {
    StageFailure::failed(code, message)
}

fn ca_failure(error: crate::compile_apply::CompileApplyError) -> StageFailure {
    StageFailure::failed(error.code(), error.to_string())
}

fn map_join(error: crate::positions::PositionsError) -> String {
    error.to_string()
}

// ---------------------------------------------------------------------------
// 编译接线配置（T66 请求里不来自测量/fit 的常量）
// ---------------------------------------------------------------------------

/// 编译接线配置（T66 CompileApplyRequest 的工程常量部分）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompileWiringConfig {
    /// block 大小（64/128）。
    pub block_size: u16,
    /// 输入通道数 N（A_map 源域）。
    pub n_in: u16,
    /// 物理容器通道数 C。
    pub container_channels: u16,
    /// 低带 G baseline 整数延迟 D。
    pub low_band_d: u32,
    /// G 声明支撑 L_G。
    pub l_g: u32,
    /// P 声明支撑 L_P。
    pub l_p: u32,
    /// delay/polarity 编码方式（本接线默认 role_metadata）。
    pub encoding: DelayPolarityEncoding,
    /// 编译/打包选项。
    pub options: CompileApplyOptions,
    /// 实验模式（**默认 OFF**；显式开启后 FitP/T65 门不再阻断 compile，产物标 EXPERIMENT）。
    pub experiment: ExperimentMode,
    /// 把 T64 PEQ 段作为 plan ABI v2 per-role IIR 区写入 plan（**默认 ON**；GAP-T66-3）。
    ///
    /// - ON：measured 路径的 T64 报告 IIR 段随 P FIR/crossover 一并进 plan（H_eff=Hraw·P_fir·P_iir，
    ///   F-37）；无 PEQ 段时 plan 仍是 v1 字节（bit-exact 历史行为）。
    /// - OFF：回落旧行为（plan 只承载 P FIR）；生产路径若 T64 报告仍有 IIR 段会被 T66 拒收
    ///   （E_CA_PEQ_ABI，禁止静默丢弃），只有显式 experiment 旁路才可通过并记
    ///   W_EXPERIMENT_IIR_EXCLUDED。OFF 只用于回退/对照，不是生产策略。
    pub t64_peq_in_plan: bool,
}

impl Default for CompileWiringConfig {
    fn default() -> Self {
        CompileWiringConfig {
            block_size: 128,
            n_in: 8,
            container_channels: 8,
            low_band_d: 0,
            l_g: 16,
            l_p: 64,
            encoding: DelayPolarityEncoding::RoleMetadata,
            options: CompileApplyOptions::default(),
            experiment: ExperimentMode::OFF,
            t64_peq_in_plan: true,
        }
    }
}

// ---------------------------------------------------------------------------
// 接线输入（调用方显式提供；不做任何隐式默认）
// ---------------------------------------------------------------------------

/// 一次端到端接线的只读输入（T64/T65 fit 输入 + T66 compile/apply 输入）。
///
/// 生产路径：plan/target/curves/phase_responses 来自 T63/T62/T49 的多点测量与质量门；
/// mapping/artifact/g_paths 来自 T57/T54/T50 的设计产物。本机离线 dry-run 用
/// [synthetic_design]（显式标注 synthetic/offline，非板端数据）。
#[derive(Clone, Debug)]
pub struct FitWiringDesign {
    /// 多点测量计划（T63；T64/T65 共用，participants/positions/common_d0 一致）。
    pub plan: MultipointPlan,
    /// T64 目标曲线（只接受 T11 内置 flat/tilt10；T64 会再校验）。
    pub target: khaen_designer::targets::TargetCurve,
    /// T64 每（测点, 通道）实测幅度曲线。
    pub curves: Vec<PositionCurve>,
    /// T64 每通道工作频带（可空 = 全带）。
    pub channel_bands: Vec<ChannelBand>,
    /// T63 design scope 质量报告（T64/T65 require_design_quality=true 时必需）。
    pub design_quality: Option<PositionQualityReport>,
    /// T64 fit 选项（含目标 FIR / D7 参数）。
    pub peq_options: PeqFitOptions,
    /// T65 已去公共 D0 一次的实测复响应（design 测点 x participants）。
    pub phase_responses: Vec<DePhasedResponse>,
    /// T65 单通道频带覆盖（可空 = 默认频带）。
    pub phase_channel_bands: Vec<PhaseChannelBand>,
    /// T65 角色->设备通道号映射（可空 = 未提供，不猜）。
    pub phase_channel_indices: Vec<PhaseChannelIndex>,
    /// T65 fit 选项。
    pub phase_options: PhaseFitOptions,
    /// T57 overlay/mapping 绑定（role/DAC/participants/stereo mask）。
    pub mapping: MappingBinding,
    /// T54 artifact hash 链绑定（stale/status 由 T66 校验）。
    pub artifact: ArtifactBinding,
    /// T54 G 每 path 系数（dense MxM；不含 delta_D baseline）。
    pub g_paths: Vec<GPathInput>,
    /// 编译接线常量。
    pub compile: CompileWiringConfig,
    /// crossover（T39 输出 FIR 分频）接线（None = P_post 不含分频段，行为与历史完全一致）。
    pub crossover: Option<CrossoverWiring>,
    /// T18 admission 环境（fs/B/workers；与 plan/compile 一致）。
    pub admission: AdmissionEnv,
    /// 可选 role gain（dB；缺省 0 dB = unity；T66 只接受 <=0 dB）。
    pub role_gains_db: Vec<(String, f64)>,
    /// 输入来源标签（证据用；例如 synthetic_offline / fixture）。
    pub design_source: String,
    /// 输入是否来自真实测量（离线夹具必须 false）。
    pub measured: bool,
    /// 实验：将 role 整数延迟强制为 0（配合 target FIR 使用，避开 F-16 双扣；产物 EXPERIMENT）。
    pub zero_phase_delay: bool,
    /// created_unix（provenance；不参与确定性 hash）。
    pub created_unix: u64,
}

impl FitWiringDesign {
    /// 结构级校验（不代替 T64/T65/T66 各自的门；只保证接线字段自洽）。
    pub fn validate(&self) -> Result<(), String> {
        let fs = self.plan.sample_rate_hz;
        if fs != self.admission.sample_rate_hz {
            return Err(format!(
                "admission fs {} != plan fs {fs}",
                self.admission.sample_rate_hz
            ));
        }
        if self.compile.block_size != self.admission.block_size {
            return Err(format!(
                "admission block {} != compile block {}",
                self.admission.block_size, self.compile.block_size
            ));
        }
        if self.mapping.container_channels != self.compile.container_channels {
            return Err(format!(
                "mapping container {} != compile container {}",
                self.mapping.container_channels, self.compile.container_channels
            ));
        }
        if self.design_source.trim().is_empty() {
            return Err("design_source must be non-empty (evidence provenance)".to_string());
        }
        if self.measured {
            // measured=true 只允许真实测量输入；离线夹具误标必须在接线层拒绝（防证据漂移）。
            if self.design_source != "measured" && self.design_source != "board" {
                return Err(format!(
                    "measured=true requires measured source label (got {:?})",
                    self.design_source
                ));
            }
        }
        // participants：plan.participants 必须等于 mapping dense->DAC 的 role id 顺序。
        let m = self.mapping.dense_to_dac.len();
        if m == 0 {
            return Err("mapping.dense_to_dac must be non-empty".to_string());
        }
        let mut participant_roles = Vec::with_capacity(m);
        for (dense, dac) in self.mapping.dense_to_dac.iter().enumerate() {
            let role_index = self.mapping.role_index_for_dac(*dac).ok_or_else(|| {
                format!("mapping.dense_to_dac[{dense}] dac {dac} has no role binding")
            })?;
            let role = self.mapping.role(role_index).ok_or_else(|| {
                format!("mapping role_index {role_index} missing from role table")
            })?;
            participant_roles.push(role.role_id.clone());
        }
        if participant_roles != self.plan.participants {
            return Err(format!(
                "plan.participants {:?} != mapping dense participants {:?}",
                self.plan.participants, participant_roles
            ));
        }
        // G paths：dense MxM 全覆盖且系数长度 = L_G。
        if self.g_paths.len() != m * m {
            return Err(format!(
                "g_paths {} != dense M*M {} (M={m})",
                self.g_paths.len(),
                m * m
            ));
        }
        let mut seen = vec![false; m * m];
        for path in &self.g_paths {
            let out = path.out_dense as usize;
            let in_dense = path.in_dense as usize;
            if out >= m || in_dense >= m {
                return Err(format!(
                    "g_paths out/in {out}/{in_dense} out of dense range {m}"
                ));
            }
            let index = out * m + in_dense;
            if seen[index] {
                return Err(format!("duplicate g path out={out} in={in_dense}"));
            }
            seen[index] = true;
            if path.coeffs.len() != self.compile.l_g as usize {
                return Err(format!(
                    "g path {out}/{in_dense} coeffs {} != L_G {}",
                    path.coeffs.len(),
                    self.compile.l_g
                ));
            }
        }
        // role/T64 报告覆盖：T66 F-35 要求每个 role 都在 T64 报告里有 channel 记录。
        let curve_channels: Vec<&str> = self.curves.iter().map(|c| c.channel.as_str()).collect();
        for role in &self.mapping.roles {
            if !curve_channels.contains(&role.role_id.as_str()) {
                return Err(format!(
                    "T64 curves 未覆盖 role {:?}（F-35 需要报告逐 role channel；请提供该通道曲线）",
                    role.role_id
                ));
            }
        }
        if self.curves.is_empty() {
            return Err("curves must be non-empty".to_string());
        }
        if self.phase_responses.is_empty() {
            return Err("phase_responses must be non-empty".to_string());
        }
        // crossover 接线：fs/布局/输出槽位/L_P 预算自洽（fail-closed；None 时零影响）。
        if let Some(crossover) = self.crossover.as_ref() {
            let xo_fs = crossover.design.spec.fs;
            if (xo_fs - fs as f64).abs() > 1e-9 {
                return Err(format!(
                    "crossover 设计 fs={xo_fs} != plan fs={fs}（T39 §7.2 FIR fs 必须等于运行 fs）"
                ));
            }
            if !crate::crossover_post::layout_accepts_crossover(&self.plan.layout) {
                return Err(format!(
                    "{E_MEASURED_CROSSOVER}: crossover 需要 {} 布局（含 SUB1 的五执行器）；当前 plan.layout={:?}（非 4.1.2 拒绝策略）",
                    crate::crossover_post::CROSSOVER_LAYOUT_REQUIRED,
                    self.plan.layout
                ));
            }
            for (output, _branch) in &crossover.assignments {
                if *output >= self.compile.container_channels {
                    return Err(format!(
                        "{E_MEASURED_CROSSOVER}: crossover 赋值 output={} 越界（container_channels={}）",
                        output, self.compile.container_channels
                    ));
                }
            }
            let required = self.crossover_required_taps()?;
            if required > self.compile.options.max_taps as usize {
                return Err(format!(
                    "{E_WIRING_CROSSOVER_BUDGET}: P_post 组合核 {required} taps > options.max_taps={}（禁止截尾；extended 不在本次接线内）",
                    self.compile.options.max_taps
                ));
            }
            if required > self.compile.l_p as usize {
                return Err(format!(
                    "{E_WIRING_CROSSOVER_BUDGET}: compile.l_p={} < P_post 组合核 {required} taps（请先 sync_crossover_p_budget；禁止截尾）",
                    self.compile.l_p
                ));
            }
        }
        Ok(())
    }

    fn role_gain_db(&self, role_id: &str) -> f64 {
        self.role_gains_db
            .iter()
            .find(|(id, _)| id == role_id)
            .map(|(_, gain)| *gain)
            .unwrap_or(0.0)
    }

    /// T64 目标 FIR 抽头数（0 = 关闭）。
    pub fn target_fir_taps(&self) -> usize {
        if self.peq_options.target_fir.enabled {
            self.peq_options.target_fir.taps
        } else {
            0
        }
    }

    /// crossover P_post 组合核所需 L_P（0 = 未配置 crossover）。
    ///
    /// P_post = crossover ⊛ target：长度 = L_xo + L_target − 1（target 关闭时 = L_xo）。
    pub fn crossover_required_taps(&self) -> Result<usize, String> {
        let Some(crossover) = self.crossover.as_ref() else {
            return Ok(0);
        };
        let xo_taps = crossover.config.branch_taps();
        let target_taps = self.target_fir_taps();
        Ok(if target_taps == 0 {
            xo_taps
        } else {
            xo_taps + target_taps - 1
        })
    }

    /// 把 compile.l_p 同步到 crossover 组合核长度（None = no-op；超预算 fail-closed）。
    pub fn sync_crossover_p_budget(&mut self) -> Result<(), String> {
        let required = self.crossover_required_taps()?;
        if required == 0 {
            return Ok(());
        }
        if required > self.compile.options.max_taps as usize {
            return Err(format!(
                "{E_WIRING_CROSSOVER_BUDGET}: P_post 组合核 {required} taps > options.max_taps={}（禁止静默截尾/扩档）",
                self.compile.options.max_taps
            ));
        }
        self.compile.l_p = self.compile.l_p.max(required as u32);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// apply all-ack 来源 seam（真实 RT host 由 T09/T20 提供；默认离线合成模型）
// ---------------------------------------------------------------------------

/// all-ack block 来源（T66 ApplySession::note_block_ack 的输入）。
///
/// 真实路径：T09 四 worker / T20 swap 在 block 边界产出 KhaenBlockAckV1 并喂入；
/// 本机离线 dry-run：默认 [OfflineSyntheticAck]（T66 标注的 NONRT 离线模型）。
pub trait ApplyAckSource: Send {
    /// 返回新版本 all-ack block；None = ack 未达（触发 T66 rollback，不视为 applied）。
    fn all_ack_block(
        &mut self,
        session: &ApplySession,
        pending_version: PlanVersion,
        expected_workers: u8,
    ) -> Option<KhaenBlockAckV1>;
}

/// 离线合成 all-ack（T66 synthetic_all_ack_block；非真实 RT 证据）。
#[derive(Debug, Default, Clone, Copy)]
pub struct OfflineSyntheticAck;

impl ApplyAckSource for OfflineSyntheticAck {
    fn all_ack_block(
        &mut self,
        session: &ApplySession,
        pending_version: PlanVersion,
        expected_workers: u8,
    ) -> Option<KhaenBlockAckV1> {
        Some(session.synthetic_all_ack_block(pending_version, expected_workers))
    }
}

/// 永不给 ack 的来源（负向用例/rollback 证据；不用于成功链路）。
#[derive(Debug, Default, Clone, Copy)]
pub struct WithheldAck;

impl ApplyAckSource for WithheldAck {
    fn all_ack_block(
        &mut self,
        _session: &ApplySession,
        _pending_version: PlanVersion,
        _expected_workers: u8,
    ) -> Option<KhaenBlockAckV1> {
        None
    }
}

// ---------------------------------------------------------------------------
// 接线 delegate（StageBackend::with_delegate 注入）
// ---------------------------------------------------------------------------

/// FitP/SolveG/Compile/Prepared/Apply 五段的接线实现。
pub struct FitWiring {
    design: FitWiringDesign,
    ack_source: Box<dyn ApplyAckSource>,
    host_apply: Option<crate::host_apply::HostApplyConfig>,
    prebuilt_plan: Option<CompiledPlan>,
    peq: Option<PeqFitOutput>,
    peq_artifacts: Option<FitArtifacts>,
    phase: Option<PhaseFitOutput>,
    phase_artifacts: Option<PhaseFitArtifacts>,
    crossover_evidence: Option<CrossoverEvidence>,
    compiled: Option<CompiledPlan>,
    session: Option<ApplySession>,
    prepared: Option<PreparedPlan>,
    applied: Option<ApplyAck>,
    rollback: Option<RollbackReceipt>,
    /// 实验旁路记录（生产门绕过明细；配合 EXPERIMENT/NON_ACCEPTABLE 产物）。
    experiment_bypass: Vec<String>,
}

impl FitWiring {
    /// 构造（结构校验失败返回人读错误；不触碰设备/不写盘）。
    pub fn new(design: FitWiringDesign) -> Result<Self, String> {
        design.validate()?;
        Ok(FitWiring {
            design,
            ack_source: Box::new(OfflineSyntheticAck),
            host_apply: None,
            prebuilt_plan: None,
            peq: None,
            peq_artifacts: None,
            phase: None,
            phase_artifacts: None,
            crossover_evidence: None,
            compiled: None,
            session: None,
            prepared: None,
            applied: None,
            rollback: None,
            experiment_bypass: Vec::new(),
        })
    }

    /// 实验模式（来自设计输入的 compile 配置；默认 OFF）。
    pub fn experiment_mode(&self) -> ExperimentMode {
        self.design.compile.experiment
    }

    /// 实验旁路记录（证据）。
    pub fn experiment_bypass(&self) -> &[String] {
        &self.experiment_bypass
    }

    /// 注入 all-ack 来源（真实 RT host / 负向用例）。
    pub fn with_ack_source(mut self, ack_source: Box<dyn ApplyAckSource>) -> Self {
        self.ack_source = ack_source;
        self
    }

    /// 注入真实 host plan-arena 应用配置（T67 补齐：apply 用真实 khaen-rt-host 进程）。
    ///
    /// 注入后 Apply 段不再消费 `ApplyAckSource` 的离线模型：plan 字节写盘 -> host
    /// plan-arena 加载/校验/跑 block -> host 报告 note_block_all_ack 汇总 -> T66
    /// `note_block_ack`。host 失败按 T66 rollback 处理，不写 applied version。
    pub fn with_host_apply(mut self, config: crate::host_apply::HostApplyConfig) -> Self {
        self.host_apply = Some(config);
        self
    }

    /// 使用调用方提供的已编译 plan（离线 artifact/坏 plan 负向路径）；Prepared 不再用 Compile 产物。
    pub fn with_prebuilt_plan(mut self, plan: CompiledPlan) -> Self {
        self.prebuilt_plan = Some(plan);
        self
    }

    /// 输入（只读）。
    pub fn design(&self) -> &FitWiringDesign {
        &self.design
    }

    /// T64 输出（FitP 之后）。
    pub fn peq_output(&self) -> Option<&PeqFitOutput> {
        self.peq.as_ref()
    }

    /// T65 输出（FitP 之后）。
    pub fn phase_output(&self) -> Option<&PhaseFitOutput> {
        self.phase.as_ref()
    }

    /// T66 编译产物（Compile 之后）。
    pub fn compiled_plan(&self) -> Option<&CompiledPlan> {
        self.compiled.as_ref()
    }

    /// prepare 回执（Prepared 之后）。
    pub fn prepared_plan(&self) -> Option<&PreparedPlan> {
        self.prepared.as_ref()
    }

    /// apply ack（Apply 成功之后；未 all-ack 恒 None）。
    pub fn applied_ack(&self) -> Option<&ApplyAck> {
        self.applied.as_ref()
    }

    /// rollback 回执（apply 的 ack 未达/rollback 之后）。
    pub fn rollback_receipt(&self) -> Option<&RollbackReceipt> {
        self.rollback.as_ref()
    }

    /// max p_fir 长度（T66 L_P 预算诊断；crossover 组合核长度计入）。
    pub fn max_p_fir_len(&self) -> Option<usize> {
        let crossover_len = self.design.crossover_required_taps().unwrap_or(0);
        self.peq.as_ref().map(|peq| {
            peq.target_firs
                .iter()
                .map(|fir| fir.coeffs.len())
                .max()
                .unwrap_or(1)
                .max(1)
                .max(crossover_len)
        })
    }

    /// 接线证据 JSON（CLI/测试/证据索引消费；显式标注离线/未测口径）。
    pub fn evidence_json(&self) -> String {
        let plan = self.compiled.as_ref().or(self.prebuilt_plan.as_ref());
        let mut fields = vec![
            ("schema", json_text(WIRING_SCHEMA)),
            ("design_source", json_text(self.design.design_source.clone())),
            ("measured", JsonValue::Bool(self.design.measured)),
            ("simulated", JsonValue::Bool(!self.design.measured)),
            (
                "board_run",
                json_text(if self.design.measured {
                    "measured_input".to_string()
                } else {
                    "NOT_RUN".to_string()
                }),
            ),
            (
                "fit",
                match (self.peq.as_ref(), self.phase.as_ref()) {
                    (Some(peq), Some(phase)) => json_object(vec![
                        (
                            "peq_report_sha256",
                            json_opt_text(
                                self.peq_artifacts.as_ref().map(|a| a.report_sha256.clone()),
                            ),
                        ),
                        (
                            "phase_report_sha256",
                            json_opt_text(
                                self.phase_artifacts
                                    .as_ref()
                                    .map(|a| a.report_sha256.clone()),
                            ),
                        ),
                        ("plan_sha256", json_text(peq.report.plan_sha256.clone())),
                        (
                            "phase_measurement_sha256",
                            json_text(phase.report.measurement_sha256.clone()),
                        ),
                        (
                            "peq_checks",
                            json_object(vec![
                                (
                                    "all_rule_2a_pass",
                                    JsonValue::Bool(peq.report.checks.all_rule_2a_pass),
                                ),
                                (
                                    "all_rule_2b_pass",
                                    JsonValue::Bool(peq.report.checks.all_rule_2b_pass),
                                ),
                                (
                                    "all_headroom_pass",
                                    JsonValue::Bool(peq.report.checks.all_headroom_pass),
                                ),
                                (
                                    "all_boost_limits_pass",
                                    JsonValue::Bool(peq.report.checks.all_boost_limits_pass),
                                ),
                            ]),
                        ),
                        (
                            "phase_checks",
                            json_object(vec![
                                ("all_pass", JsonValue::Bool(phase.report.checks.all_pass)),
                                (
                                    "fir_direction_ok",
                                    JsonValue::Bool(phase.report.checks.fir_direction_ok),
                                ),
                                ("min_zero", JsonValue::Bool(phase.report.checks.min_zero)),
                                (
                                    "reference_is_latest",
                                    JsonValue::Bool(phase.report.checks.reference_is_latest),
                                ),
                            ]),
                        ),
                    ]),
                    _ => JsonValue::Null,
                },
            ),
            (
                "compile",
                match plan {
                    Some(plan) => {
                        let mut fields = vec![
                            ("graph_id", json_text(plan.graph_id_hex())),
                            ("plan_sha256", json_text(plan.plan_sha256())),
                            ("plan_bytes", JsonValue::UInt(plan.plan_bytes.len() as u64)),
                            (
                                "chosen_form",
                                json_text(plan.report.facts.chosen_form.to_string()),
                            ),
                            ("encoding", json_text(plan.encoding.as_str().to_string())),
                            (
                                "max_p_fir_len",
                                match self.max_p_fir_len() {
                                    Some(len) => JsonValue::UInt(len as u64),
                                    None => JsonValue::Null,
                                },
                            ),
                        ];
                        // plan ABI v2 per-role IIR（仅在有 T64 PEQ 段时出现，无 PEQ 汇总不变）。
                        if !plan.iir_entries.is_empty() {
                            fields.push(("plan_abi", JsonValue::UInt(plan.header.abi_version as u64)));
                            fields.push((
                                "iir_roles",
                                JsonValue::UInt(plan.iir_entries.len() as u64),
                            ));
                            fields.push((
                                "iir_sections",
                                JsonValue::UInt(
                                    plan.iir_entries
                                        .iter()
                                        .map(|entry| entry.section_count as u64)
                                        .sum(),
                                ),
                            ));
                            if let Some(iir) = plan.report.iir.as_ref() {
                                fields.push(("iir_table_sha256", json_text(iir.table_sha256.clone())));
                            }
                        }
                        json_object(fields)
                    }
                    None => JsonValue::Null,
                },
            ),
            (
                "prepared",
                match self.prepared.as_ref() {
                    Some(prepared) => json_object(vec![
                        ("plan_version", JsonValue::UInt(prepared.plan_version as u64)),
                        (
                            "graph_id",
                            json_text(crate::compile_apply::hex32(&prepared.graph_id)),
                        ),
                        ("ack_mask", JsonValue::UInt(prepared.ack_mask as u64)),
                        (
                            "expected_workers",
                            JsonValue::UInt(prepared.expected_workers as u64),
                        ),
                        ("path_count", JsonValue::UInt(prepared.path_count as u64)),
                        ("diag_count", JsonValue::UInt(prepared.diag_count as u64)),
                        ("arena_bytes", JsonValue::UInt(prepared.arena_bytes)),
                        ("admission_note", json_text(prepared.admission_note.clone())),
                    ]),
                    None => JsonValue::Null,
                },
            ),
            (
                "apply",
                match self.applied.as_ref() {
                    Some(ack) => json_object(vec![
                        ("applied", JsonValue::Bool(ack.applied)),
                        ("plan_version", JsonValue::UInt(ack.plan_version as u64)),
                        ("graph_id", json_text(ack.graph_id_hex.clone())),
                        ("ack_mask", JsonValue::UInt(ack.ack_mask as u64)),
                        ("t3_required", JsonValue::Bool(ack.t3_required)),
                        ("t3_confirmed", JsonValue::Bool(false)),
                        (
                            "overall_status",
                            json_text("APPLIED_NOT_CONFIRMED".to_string()),
                        ),
                    ]),
                    None => JsonValue::Null,
                },
            ),
            (
                "experiment",
                if self.design.compile.experiment.is_active() {
                    json_object(vec![
                        (
                            "verdict",
                            json_text(EXPERIMENT_VERDICT.to_string()),
                        ),
                        (
                            "acceptability",
                            json_text(EXPERIMENT_ACCEPTABILITY.to_string()),
                        ),
                        ("applied", JsonValue::Bool(false)),
                        ("certifiable", JsonValue::Bool(false)),
                        (
                            "scope",
                            json_text(self.design.compile.experiment.scope().to_string()),
                        ),
                        (
                            "single_point",
                            JsonValue::Bool(self.design.compile.experiment.single_point),
                        ),
                        (
                            "multipoint_pilot",
                            JsonValue::Bool(self.design.compile.experiment.multipoint_pilot),
                        ),
                        (
                            "allow_incomplete_artifact",
                            JsonValue::Bool(
                                self.design.compile.experiment.allow_incomplete_artifact,
                            ),
                        ),
                        (
                            "gates_bypassed",
                            JsonValue::Array(
                                self.experiment_bypass
                                    .iter()
                                    .map(|note| json_text(note.clone()))
                                    .collect(),
                            ),
                        ),
                        (
                            "note",
                            json_text(format!(
                                "实验旁路：FitP/T65 生产门被显式绕过；plan 为 EXPERIMENT/NON_ACCEPTABLE，不得写成 READY/PASS；scope={}",
                                self.design.compile.experiment.scope()
                            )),
                        ),
                    ])
                } else {
                    JsonValue::Null
                },
            ),
            (
                "rollback",
                match self.rollback.as_ref() {
                    Some(receipt) => json_object(vec![
                        ("mode", json_text(receipt.mode.to_string())),
                        ("rolled_back", JsonValue::Bool(receipt.rolled_back)),
                        ("safety_mute", JsonValue::Bool(receipt.safety_mute)),
                        ("reason", json_text(receipt.reason.to_string())),
                    ]),
                    None => JsonValue::Null,
                },
            ),
            (
                "notes",
                json_text(
                    "apply ack 为 T66 NONRT 离线/宿主模型；T3 应用后实测与独立 holdout 属 T67/T68，本接线不写 CONFIRMED/PASS。"
                        .to_string(),
                ),
            ),
        ];
        // crossover 接线证据：无 crossover 时不出现该键（历史 JSON 字节不变）。
        if let Some(wiring) = self.design.crossover.as_ref() {
            let evidence_sha256 = self
                .crossover_evidence
                .as_ref()
                .map(CrossoverEvidence::evidence_sha256);
            let diag_rows = plan.map(|plan| {
                plan.diags
                    .iter()
                    .filter(|diag| diag.flags & crate::compile_apply::DIAG_FLAG_CROSSOVER != 0)
                    .count() as u64
            });
            let mut crossover_fields = vec![
                (
                    "schema",
                    json_text(crate::crossover_post::CROSSOVER_WIRING_SCHEMA),
                ),
                ("summary", wiring.summary_json()),
                (
                    "design_report_sha256",
                    json_text(wiring.design_report_sha256.clone()),
                ),
                (
                    "required_l_p",
                    JsonValue::UInt(self.design.crossover_required_taps().unwrap_or(0) as u64),
                ),
                (
                    "zero_phase_delay",
                    JsonValue::Bool(self.design.zero_phase_delay),
                ),
                ("evidence_sha256", json_opt_text(evidence_sha256)),
                (
                    "plan_diag_crossover_rows",
                    match diag_rows {
                        Some(rows) => JsonValue::UInt(rows),
                        None => JsonValue::Null,
                    },
                ),
            ];
            if let Some(evidence) = self.crossover_evidence.as_ref() {
                crossover_fields.push((
                    "evidence",
                    json_object(vec![
                        ("kind", json_text(evidence.kind.clone())),
                        ("taps", JsonValue::UInt(evidence.taps as u64)),
                        ("fc_hz", JsonValue::Number(evidence.fc_hz)),
                        ("normative_pass", JsonValue::Bool(evidence.normative_pass)),
                        (
                            "merge_flat_deviation_db",
                            JsonValue::Number(evidence.merge_flat_deviation_db),
                        ),
                        (
                            "roles",
                            JsonValue::Array(
                                evidence
                                    .roles
                                    .iter()
                                    .map(|record| {
                                        json_object(vec![
                                            ("role_id", json_text(record.role_id.clone())),
                                            ("branch", json_text(record.branch.clone())),
                                            (
                                                "composed_taps",
                                                JsonValue::UInt(record.composed_taps as u64),
                                            ),
                                            (
                                                "p_row_gain_db",
                                                JsonValue::Number(record.p_row_gain_db),
                                            ),
                                            (
                                                "p_row_max_positive_db",
                                                JsonValue::Number(record.p_row_max_positive_db),
                                            ),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ),
                    ]),
                ));
            }
            fields.push(("crossover", json_object(crossover_fields)));
        }
        let document = json_object(fields);
        match json_encode(&document) {
            Ok(text) => text,
            Err(error) => {
                format!("{{\"schema\":\"{WIRING_SCHEMA}\",\"error\":\"json encode: {error}\"}}")
            }
        }
    }

    // -- stage implementations ------------------------------------------------

    fn stage_fitp(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        ctx.report_progress(0.05);
        let (peq, peq_artifacts) = {
            let design = &self.design;
            let mut request = PeqFitRequest::new(&design.plan, &design.target, &design.curves)
                .with_options(design.peq_options.clone())
                .with_created_unix(design.created_unix);
            if !design.channel_bands.is_empty() {
                request = request.with_channel_bands(&design.channel_bands);
            }
            if let Some(quality) = design.design_quality.as_ref() {
                request = request.with_design_quality(quality);
            }
            let peq = fit_peq(&request).map_err(|error| {
                StageFailure::failed(error.code, format!("T64 fit_peq: {error}"))
            })?;
            let artifacts = peq_artifacts_of(&peq);
            (peq, artifacts)
        };
        // 证据纪律：先落盘本次真实 fit 产物（design-report/modal-peq/target-fir），
        // 再判设计侧门；门失败时证据仍可审计（失败不等于不出证据）。门失败仍以稳定
        // 错误码中止，不进入 compile。
        ctx.cancel_token().check()?;
        ctx.report_progress(0.35);
        peq_artifacts.write_stage_artifacts(ctx)?;
        // 生产语义：门失败一律中止（fail-closed）。实验模式（显式 flag）下不阻断，但每次
        // 绕过都写进 experiment_bypass => EXPERIMENT/NON_ACCEPTABLE 证据，不写成 PASS。
        let experiment = self.design.compile.experiment;
        if !peq.report.checks.all_rule_2a_pass {
            if !experiment.bypasses_production_gates() {
                return Err(wiring_fail(
                    E_WIRING_FIT_GATE,
                    "T64 rule_2a 未全过（设计侧修正门；不得进入 compile；                 本次真实 fit 报告已落盘 fit/design-report.json 供审计）",
                ));
            }
            self.experiment_bypass.push(format!(
                "T64 rule_2a FAIL（all_rule_2a_pass=false）— 实验模式不阻断 compile；fit 报告已落盘 fit/design-report.json"
            ));
        }
        if !peq.report.checks.all_rule_2b_pass {
            self.experiment_bypass.push(
                "T64 rule_2b FAIL（all_rule_2b_pass=false；生产未单独设门，如实记录）".to_string(),
            );
        }
        if !peq.report.checks.all_headroom_pass || !peq.report.checks.all_boost_limits_pass {
            if !experiment.bypasses_production_gates() {
                return Err(wiring_fail(
                    E_WIRING_FIT_GATE,
                    format!(
                        "T64 D7 门未过（headroom_pass={} boost_limits_pass={}）",
                        peq.report.checks.all_headroom_pass,
                        peq.report.checks.all_boost_limits_pass
                    ),
                ));
            }
            self.experiment_bypass.push(format!(
                "T64 D7 headroom/boost 门未过（headroom_pass={} boost_limits_pass={}）— 实验模式不阻断 compile",
                peq.report.checks.all_headroom_pass, peq.report.checks.all_boost_limits_pass
            ));
        }
        self.peq = Some(peq);
        self.peq_artifacts = Some(peq_artifacts);
        ctx.report_progress(0.45);
        ctx.cancel_token().check()?;
        let (phase, phase_artifacts) = {
            let design = &self.design;
            let mut request = PhaseFitRequest::new(&design.plan, &design.phase_responses)
                .with_options(design.phase_options.clone())
                .with_created_unix(design.created_unix);
            if !design.phase_channel_bands.is_empty() {
                request = request.with_channel_bands(&design.phase_channel_bands);
            }
            if !design.phase_channel_indices.is_empty() {
                request = request.with_channel_indices(&design.phase_channel_indices);
            }
            if let Some(quality) = design.design_quality.as_ref() {
                request = request.with_design_quality(quality);
            }
            let phase = fit_phase(&request).map_err(|error| {
                StageFailure::failed(error.code(), format!("T65 fit_phase: {error}"))
            })?;
            let artifacts = phase.artifacts.clone();
            (phase, artifacts)
        };
        if !phase.report.checks.all_pass {
            if !experiment.bypasses_production_gates() {
                return Err(wiring_fail(
                    E_WIRING_PHASE_GATE,
                    format!(
                        "T65 checks 未全过（integer={} min_zero={} ref_latest={} frac={} polarity={} fir_design={} fir_direction={}）",
                        phase.report.checks.integer_delay_contract,
                        phase.report.checks.min_zero,
                        phase.report.checks.reference_is_latest,
                        phase.report.checks.fractional_residual_within_half_sample,
                        phase.report.checks.polarity_known,
                        phase.report.checks.fir_design_ok,
                        phase.report.checks.fir_direction_ok,
                    ),
                ));
            }
            self.experiment_bypass.push(format!(
                "T65 checks 未全过（integer={} min_zero={} ref_latest={} frac={} polarity={} fir_design={} fir_direction={}）— 实验模式不阻断 compile",
                phase.report.checks.integer_delay_contract,
                phase.report.checks.min_zero,
                phase.report.checks.reference_is_latest,
                phase.report.checks.fractional_residual_within_half_sample,
                phase.report.checks.polarity_known,
                phase.report.checks.fir_design_ok,
                phase.report.checks.fir_direction_ok,
            ));
        }
        ctx.cancel_token().check()?;
        ctx.report_progress(0.75);
        phase_artifacts.write_stage_artifacts(ctx)?;
        self.phase = Some(phase);
        self.phase_artifacts = Some(phase_artifacts);
        let summary = self.evidence_json();
        ctx.write_artifact(
            ArtifactKind::Other("fit_wiring_summary".to_string()),
            FITP_SUMMARY_ARTIFACT_PATH,
            summary.as_bytes(),
            WIRING_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    fn stage_solve_g(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let m = self.design.mapping.dense_to_dac.len();
        if self.design.g_paths.len() != m * m {
            return Err(wiring_fail(
                E_WIRING_SOLVE_G,
                format!(
                    "G 输入 {} 条 != dense M*M {}（M={m}）；T50/T54 求解产物必须按 dense 全覆盖",
                    self.design.g_paths.len(),
                    m * m
                ),
            ));
        }
        let mut diag = 0usize;
        let mut offdiag = 0usize;
        for path in &self.design.g_paths {
            if path.diag_baseline {
                diag += 1;
            } else {
                offdiag += 1;
            }
        }
        let document = json_object(vec![
            ("schema", json_text(WIRING_SCHEMA)),
            ("stage", json_text("solve_g".to_string())),
            ("design_source", json_text(self.design.design_source.clone())),
            ("measured", JsonValue::Bool(self.design.measured)),
            ("simulated", JsonValue::Bool(!self.design.measured)),
            (
                "g_source",
                json_text(
                    "design_input(t54/t50 upstream); 本 delegate 不做 5x5 求解，只校验/接线 T54 每 path 系数"
                        .to_string(),
                ),
            ),
            ("dense_participants", JsonValue::UInt(m as u64)),
            (
                "path_count",
                JsonValue::UInt(self.design.g_paths.len() as u64),
            ),
            ("diag_paths", JsonValue::UInt(diag as u64)),
            ("offdiag_paths", JsonValue::UInt(offdiag as u64)),
            ("l_g", JsonValue::UInt(self.design.compile.l_g as u64)),
            (
                "low_band_d",
                JsonValue::UInt(self.design.compile.low_band_d as u64),
            ),
        ]);
        let summary = encode_json(document).map_err(|message| {
            wiring_fail(E_WIRING_SOLVE_G, format!("solve_g summary json: {message}"))
        })?;
        ctx.write_artifact(
            ArtifactKind::Other("g_source".to_string()),
            SOLVE_G_ARTIFACT_PATH,
            summary.as_bytes(),
            WIRING_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    fn stage_compile(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let peq = self.peq.as_ref().ok_or_else(resume_pending)?;
        let peq_artifacts = self.peq_artifacts.as_ref().ok_or_else(resume_pending)?;
        let phase = self.phase.as_ref().ok_or_else(resume_pending)?;
        let phase_artifacts = self.phase_artifacts.as_ref().ok_or_else(resume_pending)?;
        let report_bytes = peq_artifacts
            .entries
            .iter()
            .find(|entry| entry.path == crate::peq_fit::FIT_REPORT_ARTIFACT_PATH)
            .map(|entry| entry.bytes.clone())
            .ok_or_else(|| {
                wiring_fail(
                    E_WIRING_FIT_GATE,
                    "缺少 T64 报告 artifact 字节（F-35 D7 绑定必需）",
                )
            })?;
        let headroom = build_headroom(&self.design, peq, &report_bytes)?;
        let phase_evidence = build_phase_evidence(&self.design, phase, phase_artifacts)?;
        let roles = build_roles(&self.design, peq, phase)?;
        // crossover 证据（P_post 含分频段时；D7 绑定基准 = 卷积后 P 行预测）。
        let crossover_evidence = build_crossover_evidence(&self.design, &roles)?;
        let plan = {
            let design = &self.design;
            let request = CompileApplyRequest::new(
                design.plan.sample_rate_hz,
                design.compile.block_size,
                design.compile.n_in,
                design.compile.container_channels,
                design.compile.low_band_d,
                design.compile.l_g,
                design.compile.l_p,
                &design.mapping,
                &phase_evidence,
                &headroom,
                &design.artifact,
                &design.g_paths,
                &roles,
                design.compile.encoding,
                design.compile.options,
            );
            let request = if design.compile.experiment.is_active() {
                request
                    .with_experiment(design.compile.experiment)
                    .with_experiment_notes(self.experiment_bypass.clone())
            } else {
                request
            };
            let request = match crossover_evidence.as_ref() {
                Some(evidence) => request.with_crossover(evidence),
                None => request,
            };
            compile_plan(&request).map_err(ca_failure)?
        };
        ctx.cancel_token().check()?;
        ctx.report_progress(0.7);
        plan.artifacts().write_stage_artifacts(ctx)?;
        // 实验模式：额外落 EXPERIMENT envelope（生产路径 None，不产任何新 artifact）。
        if let Some(envelope) = plan.experiment_artifact() {
            ctx.write_artifact(
                envelope.kind,
                &envelope.path,
                &envelope.bytes,
                EXPERIMENT_SCHEMA,
            )?;
        }
        self.crossover_evidence = crossover_evidence;
        self.compiled = Some(plan);
        let summary = self.evidence_json();
        ctx.write_artifact(
            ArtifactKind::Other("fit_wiring_summary".to_string()),
            COMPILE_SUMMARY_ARTIFACT_PATH,
            summary.as_bytes(),
            WIRING_SCHEMA,
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    fn stage_prepared(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let plan = self
            .prebuilt_plan
            .as_ref()
            .or(self.compiled.as_ref())
            .ok_or_else(resume_pending)?
            .clone();
        let env = self.design.admission;
        let mut session = ApplySession::new(plan.header.sample_rate, plan.header.block_size);
        let prepared = session
            .prepare_with(&plan, &env, &mut MockMemory::new())
            .map_err(ca_failure)?;
        let document = json_object(vec![
            ("schema", json_text(WIRING_SCHEMA)),
            ("stage", json_text("prepared".to_string())),
            ("design_source", json_text(self.design.design_source.clone())),
            ("simulated", JsonValue::Bool(!self.design.measured)),
            ("plan_version", JsonValue::UInt(prepared.plan_version as u64)),
            (
                "graph_id",
                json_text(crate::compile_apply::hex32(&prepared.graph_id)),
            ),
            (
                "expected_workers",
                JsonValue::UInt(prepared.expected_workers as u64),
            ),
            ("ack_mask", JsonValue::UInt(prepared.ack_mask as u64)),
            ("path_count", JsonValue::UInt(prepared.path_count as u64)),
            ("diag_count", JsonValue::UInt(prepared.diag_count as u64)),
            ("arena_bytes", JsonValue::UInt(prepared.arena_bytes)),
            ("graph_bytes", JsonValue::UInt(prepared.graph_bytes)),
            (
                "total_budget_bytes",
                JsonValue::UInt(prepared.total_budget_bytes),
            ),
            ("planned_version", json_text(prepared.planned_version.clone())),
            ("admission_note", json_text(prepared.admission_note.clone())),
            ("state", json_text(session.state().to_string())),
            (
                "note",
                json_text(
                    "T66 NONRT prepare/admission 模型（T18 MockMemory + T20 PlanRegistry）；真实 RT host 接线属 T26/T67 窗口"
                        .to_string(),
                ),
            ),
        ]);
        let summary = encode_json(document).map_err(|message| {
            wiring_fail(E_WIRING_DESIGN, format!("prepared summary json: {message}"))
        })?;
        ctx.write_artifact(
            ArtifactKind::Other("prepared_plan".to_string()),
            PREPARED_ARTIFACT_PATH,
            summary.as_bytes(),
            WIRING_SCHEMA,
        )?;
        self.session = Some(session);
        self.prepared = Some(prepared);
        ctx.report_progress(1.0);
        Ok(())
    }

    fn stage_apply(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let expected_workers = self
            .prepared
            .as_ref()
            .map(|prepared| prepared.expected_workers)
            .ok_or_else(resume_pending)?;
        // 1) T66 commit：prepared -> staged -> armed -> block 边界 swap（此时仍未 applied）。
        let pending_version = {
            let session = self.session.as_mut().ok_or_else(resume_pending)?;
            session.commit().map_err(ca_failure)?.pending_version
        };
        // 2) ack 来源：
        //    - 配置了 host apply：真实 khaen-rt-host plan-arena 进程加载同一 plan 字节并跑
        //      block，报告里的 swap（note_block_all_ack，synthetic=false）映射为 KhaenBlockAckV1；
        //    - 未配置：沿用 T66 ApplyAckSource seam（离线模型，证据标 simulated）。
        let host_config = self.host_apply.clone();
        let mut host_evidence: Option<String> = None;
        let block = match host_config {
            Some(config) => {
                let plan = self
                    .compiled
                    .as_ref()
                    .or(self.prebuilt_plan.as_ref())
                    .ok_or_else(resume_pending)?
                    .clone();
                let last_good_bytes = self
                    .session
                    .as_ref()
                    .and_then(|session| session.last_good())
                    .map(|last_good| last_good.plan_bytes.clone());
                let bundle = match crate::host_apply::write_plan_bundle(
                    &plan,
                    &config,
                    last_good_bytes.as_deref(),
                ) {
                    Ok(bundle) => bundle,
                    Err(message) => {
                        return Err(self.fail_with_rollback(
                            ctx,
                            RollbackReason::CommitFailed,
                            &format!("host plan bundle 写入失败：{message}"),
                        ));
                    }
                };
                let outcome = match crate::host_apply::run_host_apply(&bundle, &config) {
                    Ok(outcome) => outcome,
                    Err(message) => {
                        return Err(self.fail_with_rollback(
                            ctx,
                            RollbackReason::CommitFailed,
                            &format!("host plan-arena 应用失败：{message}"),
                        ));
                    }
                };
                if outcome.plan_version != pending_version {
                    return Err(self.fail_with_rollback(
                        ctx,
                        RollbackReason::AckTimeout,
                        &format!(
                            "host 报告的 plan_version={} 与 T66 pending={} 不一致；不得视为 applied（host 输出目录 {}）",
                            outcome.plan_version,
                            pending_version,
                            config.work_dir.display()
                        ),
                    ));
                }
                let block = match outcome.all_ack_block(expected_workers) {
                    Ok(block) => block,
                    Err(message) => {
                        return Err(self.fail_with_rollback(
                            ctx,
                            RollbackReason::AckTimeout,
                            &format!("host all-ack 映射失败：{message}"),
                        ));
                    }
                };
                host_evidence = Some(outcome.evidence_json());
                Some(block)
            }
            None => {
                let session = self.session.as_mut().ok_or_else(resume_pending)?;
                self.ack_source
                    .all_ack_block(session, pending_version, expected_workers)
            }
        };
        let Some(block) = block else {
            return Err(self.fail_with_rollback(
                ctx,
                RollbackReason::AckTimeout,
                "apply ack 未达（all_ack_block=None）；按 T66 rollback 回到 last-good/SafetyMute",
            ));
        };
        // 3) 喂 ack：只有 all-ack 使 retire_ready=1 才可能 applied。
        let ack = {
            let session = self.session.as_mut().ok_or_else(resume_pending)?;
            match session.note_block_ack(&block).map_err(ca_failure)? {
                Some(ack) => ack,
                None => {
                    return Err(self.fail_with_rollback(
                        ctx,
                        RollbackReason::AckTimeout,
                        "all-ack 未使 retire_ready=1（T20 swap 语义）；不得视为 applied",
                    ));
                }
            }
        };
        if !ack.applied {
            return Err(self.fail_with_rollback(
                ctx,
                RollbackReason::AckTimeout,
                "apply ack.applied=false；不得 set_applied_version",
            ));
        }
        ctx.cancel_token().check()?;
        // 4) ack 完成才登记 artifact + 设置 applied version（T60 纪律）。
        let (ack_artifact, last_good_version) = {
            let session = self.session.as_ref().ok_or_else(resume_pending)?;
            let ack_artifact = session.apply_ack_artifact().ok_or_else(|| {
                wiring_fail(E_WIRING_STAGE, "apply ack artifact 缺失（内部状态不一致）")
            })?;
            let last_good_version = session.last_good().map(|last_good| {
                format!(
                    "pv-{}:{}",
                    last_good.plan_version,
                    crate::compile_apply::hex32(&last_good.graph_id)
                )
            });
            (ack_artifact, last_good_version)
        };
        ctx.write_artifact(
            ack_artifact.kind,
            &ack_artifact.path,
            &ack_artifact.bytes,
            "t66-apply-v1",
        )?;
        if let Some(evidence) = host_evidence.as_ref() {
            ctx.write_artifact(
                ArtifactKind::Other("host_apply_evidence".to_string()),
                HOST_APPLY_ARTIFACT_PATH,
                evidence.as_bytes(),
                crate::host_apply::HOST_APPLY_SCHEMA,
            )?;
        }
        let applied_version = format!("pv-{}:{}", ack.plan_version, ack.graph_id_hex);
        ctx.set_applied_version(applied_version);
        if let Some(last_good_version) = last_good_version {
            ctx.set_last_good_version(last_good_version);
        }
        let summary = self.evidence_json();
        ctx.write_artifact(
            ArtifactKind::Other("fit_wiring_summary".to_string()),
            APPLY_SUMMARY_ARTIFACT_PATH,
            summary.as_bytes(),
            WIRING_SCHEMA,
        )?;
        self.applied = Some(ack);
        ctx.report_progress(1.0);
        Ok(())
    }

    fn fail_with_rollback(
        &mut self,
        ctx: &mut StageCtx<'_>,
        reason: RollbackReason,
        message: &str,
    ) -> StageFailure {
        let receipt = match self
            .session
            .as_mut()
            .map(|session| session.rollback_with(reason, &mut MockMemory::new()))
        {
            Some(Ok(receipt)) => Some(receipt),
            Some(Err(error)) => {
                return wiring_fail(
                    crate::compile_apply::E_CA_ROLLBACK,
                    format!("{message}；T66 rollback 亦失败：{error}"),
                );
            }
            None => None,
        };
        if let Some(receipt) = receipt.as_ref() {
            let document = json_object(vec![
                ("schema", json_text(WIRING_SCHEMA)),
                ("stage", json_text("apply".to_string())),
                ("reason", json_text(receipt.reason.to_string())),
                ("mode", json_text(receipt.mode.to_string())),
                ("rolled_back", JsonValue::Bool(receipt.rolled_back)),
                ("safety_mute", JsonValue::Bool(receipt.safety_mute)),
                (
                    "safety_mute_cycles",
                    JsonValue::UInt(receipt.safety_mute_cycles as u64),
                ),
                (
                    "version_epoch_reset",
                    JsonValue::Bool(receipt.version_epoch_reset),
                ),
                (
                    "last_good_version",
                    json_opt_u64(receipt.last_good_version.map(u64::from)),
                ),
                (
                    "last_good_graph_id",
                    json_opt_text(receipt.last_good_graph_id_hex.clone()),
                ),
                (
                    "warnings",
                    JsonValue::Array(
                        receipt
                            .warnings
                            .iter()
                            .map(|warning| json_text(warning.clone()))
                            .collect(),
                    ),
                ),
                ("failure", json_text(message.to_string())),
            ]);
            if let Ok(summary) = encode_json(document) {
                let _ = ctx.write_artifact(
                    ArtifactKind::Other("apply_rollback".to_string()),
                    ROLLBACK_ARTIFACT_PATH,
                    summary.as_bytes(),
                    WIRING_SCHEMA,
                );
            }
            self.rollback = Some(receipt.clone());
            return wiring_fail(
                crate::compile_apply::E_CA_ACK_TIMEOUT,
                format!(
                    "{message}（rollback mode={} safety_mute={}）",
                    receipt.mode, receipt.safety_mute
                ),
            );
        }
        wiring_fail(
            crate::compile_apply::E_CA_ACK_TIMEOUT,
            format!("{message}（无 apply session 可回滚）"),
        )
    }
}

impl StageDelegate for FitWiring {
    fn run(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::FitP => self.stage_fitp(ctx),
            JobStage::SolveG => self.stage_solve_g(ctx),
            JobStage::Compile => self.stage_compile(ctx),
            JobStage::Prepared => self.stage_prepared(ctx),
            JobStage::Apply => self.stage_apply(ctx),
            other => Err(wiring_fail(
                E_WIRING_STAGE,
                format!(
                    "stage {} 不属 fit-wiring delegate（测量段由 StageBackend 负责）",
                    other.as_str()
                ),
            )),
        }
    }
}

fn resume_pending() -> StageFailure {
    wiring_fail(
        E_WIRING_RESUME_PENDING,
        "缺少本 stage 所需的内存上游产物（进程重启后 checkpoint resume 从 in-progress stage 重跑；fit/compile 产物不跨进程保留）：请重跑 job 而不依赖 resume",
    )
}

fn peq_artifacts_of(peq: &PeqFitOutput) -> FitArtifacts {
    peq.artifacts()
}

// ---------------------------------------------------------------------------
// T64/T65 -> T66 输入桥接
// ---------------------------------------------------------------------------

/// T64 PEQ 设计通道 → plan ABI v2 IIR 段（GAP-T66-3）。
///
/// 取 T64 `PeqDesign` 该 role 的 band 顺序（与 T64 报告 `bands[]` 同序）转成 plan 段：
/// a0 必须归一化为 1、系数有限、二阶实系数极点单位圆内。空/无该通道 = 无 PEQ 段。
fn peq_sections_for_role(
    peq: &PeqFitOutput,
    role_id: &str,
) -> Result<Vec<crate::peq_plan::PeqBiquad>, StageFailure> {
    let Some(channel) = peq
        .peq
        .channels
        .iter()
        .find(|channel| channel.name == role_id)
    else {
        return Ok(Vec::new());
    };
    let mut sections = Vec::with_capacity(channel.bands.len());
    for (index, band) in channel.bands.iter().enumerate() {
        if (band.a[0] - 1.0).abs() > 1e-9 {
            return Err(wiring_fail(
                E_WIRING_PEQ_PLAN,
                format!(
                    "role {role_id:?} band[{index}] a0={} 未归一化为 1（plan IIR 只编码 b0,b1,b2,a1,a2）",
                    band.a[0]
                ),
            ));
        }
        let section =
            crate::peq_plan::PeqBiquad::new(band.b[0], band.b[1], band.b[2], band.a[1], band.a[2])
                .map_err(|detail| {
                    wiring_fail(
                        E_WIRING_PEQ_PLAN,
                        format!("role {role_id:?} band[{index}]: {detail}"),
                    )
                })?;
        sections.push(section);
    }
    Ok(sections)
}

fn build_roles(
    design: &FitWiringDesign,
    peq: &PeqFitOutput,
    phase: &PhaseFitOutput,
) -> Result<Vec<RoleCompileInput>, StageFailure> {
    let mut roles = Vec::with_capacity(design.mapping.roles.len());
    for slot in &design.mapping.roles {
        let is_participant = design.mapping.dense_to_dac.contains(&slot.dac);
        let (mut delay_samples, polarity) = if is_participant {
            let op = phase
                .ops
                .iter()
                .find(|op| op.channel == slot.role_id)
                .ok_or_else(|| {
                    wiring_fail(
                        E_WIRING_PHASE_GATE,
                        format!("T65 ops 未覆盖 participant role {:?}", slot.role_id),
                    )
                })?;
            (op.delay_samples, op.polarity.sign() as i8)
        } else {
            (0u32, 1i8)
        };
        if design.zero_phase_delay {
            delay_samples = 0;
        }
        let target_fir = peq
            .target_firs
            .iter()
            .find(|fir| fir.name == slot.role_id && !fir.coeffs.is_empty())
            .map(|fir| fir.coeffs.clone())
            .unwrap_or_else(|| vec![1.0]);
        // P_post = crossover ⊛ target（规范段序）；crossover 未赋值该输出 = 直通，只承载 target。
        let crossover_kernel = design
            .crossover
            .as_ref()
            .and_then(|wiring| wiring.kernel_for_output(slot.dac));
        let composed =
            compose_role_p_post(crossover_kernel, Some(&target_fir)).map_err(|message| {
                wiring_fail(
                    E_MEASURED_CROSSOVER,
                    format!("role {:?} P_post 段组合失败：{message}", slot.role_id),
                )
            })?;
        let p_fir = composed.kernel;
        let source_channel = if (slot.role_index as u32) < design.compile.n_in as u32 {
            Some(slot.role_index)
        } else {
            None
        };
        let gain_db = design.role_gain_db(&slot.role_id);
        let mut role = RoleCompileInput::new(
            slot.role_index,
            slot.role_id.clone(),
            slot.dac,
            source_channel,
            delay_samples,
            polarity,
            gain_db,
            p_fir,
            0,
        )
        .map_err(|error| wiring_fail(error.code(), format!("role {:?}: {error}", slot.role_id)))?;
        if !composed.segments.is_empty() {
            role = role.with_fir_segments(composed.segments).map_err(|error| {
                wiring_fail(
                    error.code(),
                    format!("role {:?} fir_segments: {error}", slot.role_id),
                )
            })?;
        }
        // plan ABI v2（GAP-T66-3）：T64 报告 PEQ 段 → plan per-role IIR 表（与 P FIR/crossover
        // 共存；H_eff=Hraw·P_fir·P_iir）。空段/无该通道 = 不追加（v1 字节不变）。
        if design.compile.t64_peq_in_plan {
            let sections = peq_sections_for_role(peq, &slot.role_id)?;
            if !sections.is_empty() {
                role = role.with_peq_sections(sections).map_err(|error| {
                    wiring_fail(
                        error.code(),
                        format!("role {:?} peq_sections: {error}", slot.role_id),
                    )
                })?;
            }
        }
        roles.push(role);
    }
    Ok(roles)
}

/// crossover D7 证据：T39 判据/合并平坦 + 每 role 卷积后 P 行预测（D7 绑定基准）。
///
/// 仅在 design.crossover 存在时产出；无 crossover 返回 None（报告/证据均不出现该节）。
fn build_crossover_evidence(
    design: &FitWiringDesign,
    roles: &[RoleCompileInput],
) -> Result<Option<CrossoverEvidence>, StageFailure> {
    let Some(wiring) = design.crossover.as_ref() else {
        return Ok(None);
    };
    let fs = design.plan.sample_rate_hz as f64;
    let segment_taps = wiring.config.branch_taps();
    let mut records = Vec::with_capacity(roles.len());
    for role in roles {
        if !role.has_crossover_segment() {
            continue;
        }
        let branch = wiring.branch_for_output(role.out_channel).ok_or_else(|| {
            wiring_fail(
                E_MEASURED_CROSSOVER,
                format!(
                    "role {:?} 含 crossover 段但输出 {} 无分支赋值（状态不一致）",
                    role.role_id, role.out_channel
                ),
            )
        })?;
        let composed = &role.p_fir;
        let target_taps = if role.has_target_segment() {
            composed
                .len()
                .checked_add(1)
                .and_then(|len| len.checked_sub(segment_taps))
                .ok_or_else(|| {
                    wiring_fail(
                        E_MEASURED_CROSSOVER,
                        format!(
                            "role {:?} 段长度关系非法（无法反解 target taps）",
                            role.role_id
                        ),
                    )
                })?
        } else {
            0
        };
        let record = CrossoverRoleEvidence::new(
            role.role_id.clone(),
            role.out_channel,
            branch.as_str(),
            segment_taps,
            target_taps,
            composed.len(),
            p_row_chain_peak_gain_db(composed, fs),
            p_row_max_positive_db(composed, fs),
            kernel_sha256(composed),
        )
        .map_err(|error| {
            wiring_fail(
                error.code(),
                format!("crossover evidence role {:?}: {error}", role.role_id),
            )
        })?;
        records.push(record);
    }
    if records.is_empty() {
        return Err(wiring_fail(
            E_MEASURED_CROSSOVER,
            "crossover 已配置但没有任何 role 承载 crossover 段（未赋值输出直通；该配置对 plan 无效果，拒绝）",
        ));
    }
    CrossoverEvidence::new(
        wiring.design.kind.as_str(),
        wiring.design.method.as_str(),
        design.plan.sample_rate_hz,
        wiring.design.spec.fc_hz,
        segment_taps,
        design.compile.container_channels,
        wiring.design_report_sha256.clone(),
        wiring.normative_pass(),
        wiring.failed_criteria(),
        wiring.design.merge.flat_max_deviation_db,
        wiring.design.merge.flat_limit_db,
        wiring.design.merge.relative_delay_samples,
        wiring.design.lowpass.max_abs_h,
        wiring.design.highpass.max_abs_h,
        wiring.design.lowpass.sum_abs_h,
        wiring.design.highpass.sum_abs_h,
        records,
    )
    .map(Some)
    .map_err(|error| wiring_fail(error.code(), format!("crossover evidence: {error}")))
}

/// 相位证据桥接（T65 op -> T66 phase evidence；role 表逐条对齐）。
fn build_phase_evidence(
    design: &FitWiringDesign,
    phase: &PhaseFitOutput,
    artifacts: &PhaseFitArtifacts,
) -> Result<PhaseEvidence, StageFailure> {
    // 逐 mapping role 组装 ops：participant 用 T65 实测 op；非 participant 用 identity
    //（T66 要求 phase.channels 与 role 表逐条一致；identity 不参与 MIMO 估计，delay=0/极性 normal）。
    let mut ops: Vec<PhaseChannelOp> = Vec::with_capacity(design.mapping.roles.len());
    for slot in &design.mapping.roles {
        let measured = phase
            .ops
            .iter()
            .find(|op| op.channel == slot.role_id)
            .cloned();
        match measured {
            Some(mut op) => {
                if op.index.is_none() {
                    op.index = Some(slot.dac as u32);
                }
                if design.zero_phase_delay {
                    op.delay_samples = 0;
                    op.delay_ms = 0.0;
                }
                ops.push(op);
            }
            None => ops.push(PhaseChannelOp {
                channel: slot.role_id.clone(),
                index: Some(slot.dac as u32),
                delay_samples: 0,
                delay_ms: 0.0,
                polarity: Polarity::Normal,
            }),
        }
    }
    let ops_sha256 = artifacts
        .entries
        .iter()
        .find(|entry| entry.path == crate::phase_fit::PHASE_OPS_ARTIFACT_PATH)
        .map(|entry| entry.sha256.clone())
        .unwrap_or_else(|| sha256_hex(phase.ops_json().as_bytes()));
    let report_sha256 = artifacts
        .entries
        .iter()
        .find(|entry| entry.path == crate::phase_fit::PHASE_FIT_ARTIFACT_PATH)
        .map(|entry| entry.sha256.clone())
        .unwrap_or_else(|| sha256_hex(phase.report_json().as_bytes()));
    let residual_max = phase
        .report
        .channels
        .iter()
        .map(|channel| channel.execution.delta_samples.abs())
        .fold(0.0_f64, f64::max);
    if phase.report.reference_channel.trim().is_empty() {
        return Err(wiring_fail(
            E_WIRING_PHASE_GATE,
            "T65 报告 reference_channel 为空",
        ));
    }
    Ok(PhaseEvidence::from_ops(
        &ops,
        &phase.report.checks,
        ops_sha256,
        report_sha256,
        phase.report.reference_channel.clone(),
        residual_max,
    ))
}

fn build_headroom(
    design: &FitWiringDesign,
    peq: &PeqFitOutput,
    report_bytes: &[u8],
) -> Result<HeadroomEvidence, StageFailure> {
    let mut records = Vec::with_capacity(design.mapping.roles.len());
    for slot in &design.mapping.roles {
        let channel = peq
            .report
            .channels
            .iter()
            .find(|channel| channel.name == slot.role_id)
            .ok_or_else(|| {
                wiring_fail(
                    E_WIRING_FIT_GATE,
                    format!(
                        "T64 报告缺 role {:?} 的 channel 记录（F-35 D7 证据必须逐 role）",
                        slot.role_id
                    ),
                )
            })?;
        let record = RoleBoostRecord::new(
            slot.role_id.clone(),
            channel.single_filter_max_boost_db,
            channel.filter_positive_sum_db,
            channel.predicted_peak_dbfs,
            channel.headroom_db,
            channel.boost_limits_pass && channel.headroom_pass,
        )
        .map_err(|error| {
            wiring_fail(
                error.code(),
                format!("headroom role {:?}: {error}", slot.role_id),
            )
        })?;
        records.push(record);
    }
    let report_sha256 = sha256_hex(report_bytes);
    Ok(
        HeadroomEvidence::new(COMPILE_PEQ_FIT_SCHEMA, report_sha256, records)
            .with_t64_report_bytes(report_bytes.to_vec()),
    )
}

// ---------------------------------------------------------------------------
// JSON helpers（证据摘要；确定性字段顺序）
// ---------------------------------------------------------------------------

fn json_text(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn json_opt_text(value: Option<String>) -> JsonValue {
    match value {
        Some(value) => JsonValue::String(value),
        None => JsonValue::Null,
    }
}

fn json_opt_u64(value: Option<u64>) -> JsonValue {
    match value {
        Some(value) => JsonValue::UInt(value),
        None => JsonValue::Null,
    }
}

fn encode_json(document: JsonValue) -> Result<String, String> {
    json_encode(&document).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// 实验：measured 设计 -> P_post 目标曲线 FIR -> factorized plan（目标 3/4 离线路径）
// ---------------------------------------------------------------------------

/// 实验路径参数非法（taps 越界 / 未在 experiment.single_point 下调用）。
pub const E_WIRING_TARGET_FIR_EXPERIMENT: &str = "E_WIRING_TARGET_FIR_EXPERIMENT";
/// T64 目标 FIR 抽头数下限（plan P 行最小可辨识长度）。
pub const TARGET_FIR_TAPS_MIN: usize = 8;
/// T64 目标 FIR 抽头数上限（与 CLI --target-fir-taps 一致；extended taps 不在此实验内）。
pub const TARGET_FIR_TAPS_MAX: usize = 4096;

/// 实验生效结果（写证据/日志用；不含任何测量结论或验收结论）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetFirExperimentOutcome {
    /// 目标 FIR 抽头数（= compile L_P）。
    pub taps: u32,
    /// 是否把 role 整数延迟归零（F-16 双扣规避）。
    pub zero_phase_delay: bool,
    /// 强制 plan 形态（真实 rt-host plan-arena 目前只支持 factorized）。
    pub force_form: FusionForm,
}

/// 启用 P_post 目标曲线 FIR 所需的最小联动（不改变 experiment 标记，不选 plan 形态）。
///
/// T64 目标 FIR 是线性相位 FIR：GD_p=(taps-1)/2 已包含在系数里，因此必须同时：
/// 1) 把 T65 role 整数延迟归零（否则 T66 F-16 双扣 -> E_CA_DOUBLE_COUNT）；
/// 2) 把 compile L_P 提到 taps（否则 role P FIR 超过 L_P -> E_CA_TAPS）。
///
/// 生产路径不得调用；measured 真实测量入口见
/// [apply_measured_target_fir_experiment]（额外要求 experiment.single_point 并强制
/// factorized）。证据：evidence/calib-chain-20261121/measured-pfir-20261122/。
pub fn enable_target_fir_p_post(design: &mut FitWiringDesign, taps: usize) -> Result<(), String> {
    if !(TARGET_FIR_TAPS_MIN..=TARGET_FIR_TAPS_MAX).contains(&taps) {
        return Err(format!(
            "{E_WIRING_TARGET_FIR_EXPERIMENT}: target FIR taps={taps} 越界（{}..={}）",
            TARGET_FIR_TAPS_MIN, TARGET_FIR_TAPS_MAX
        ));
    }
    design.peq_options.target_fir.enabled = true;
    design.peq_options.target_fir.taps = taps;
    design.zero_phase_delay = true;
    // L_P 只增不减：crossover 已配置时组合核 = L_xo + taps - 1（见 sync_crossover_p_budget）。
    design.compile.l_p = design.compile.l_p.max(taps as u32);
    design.sync_crossover_p_budget()?;
    Ok(())
}

/// EXPERIMENT：measured 设计启用 P_post 目标曲线 FIR，并把 T66 输入准备成真实
/// plan-arena 可消费的形态（force factorized；P FIR 作为 diag 行下发）。
///
/// 真实执行证据（2026-11-22，backend=fake，非真实声卡）：
/// `rt-stack/evidence/calib-chain-20261121/measured-pfir-20261122/`
/// plan 4552 B / diag_count=2 / p_taps=512 / host_status=ok / blocks=8 / acks=4 /
/// all_ack=8 / retire_ready=true；job=complete，apply-ack=APPLIED_NOT_CONFIRMED。
/// 产物一律 EXPERIMENT / NON_ACCEPTABLE；真实 ALSA apply + T3 measured + holdout
/// 仍需白天出声窗口（本函数不执行任何 apply）。
pub fn apply_measured_target_fir_experiment(
    design: &mut FitWiringDesign,
    taps: usize,
    experiment: ExperimentMode,
) -> Result<TargetFirExperimentOutcome, String> {
    if !experiment.single_point {
        return Err(format!(
            "{E_WIRING_TARGET_FIR_EXPERIMENT}: 必须在 experiment.single_point 下调用（生产路径禁止 target FIR 实验）"
        ));
    }
    enable_target_fir_p_post(design, taps)?;
    // 真实 rt-host plan-arena 只接受 factorized（G path + P diag）；fused 形态加载 NOT_WIRED。
    // 显式 force，避免依赖 T12 成本启发式在数据变化后悄悄切到 fused。
    design.compile.options.force_form = Some(FusionForm::Factorized);
    design.compile.experiment = experiment;
    Ok(TargetFirExperimentOutcome {
        taps: taps as u32,
        zero_phase_delay: true,
        force_form: FusionForm::Factorized,
    })
}

// ---------------------------------------------------------------------------
// 实验：crossover（T39 分频）接入 P_post -> factorized plan（目标 3 缺口）
// ---------------------------------------------------------------------------

/// crossover 进入 P_post 的结果（证据/日志；不含验收结论）。
#[derive(Clone, Debug, PartialEq)]
pub struct CrossoverPPostOutcome {
    /// 每支 taps（T39 设计支撑）。
    pub taps: u32,
    /// 卷积后 P FIR 长度（crossover ⊛ target）。
    pub composed_taps: u32,
    /// 同步后的 compile L_P。
    pub l_p: u32,
    /// 是否把 role 整数延迟归零（P FIR 承载 GD；F-16 单一延迟定义）。
    pub zero_phase_delay: bool,
    /// 显式赋值的输出数。
    pub assignments: usize,
}

/// 把 T39 分频设计接入 P_post（**显式配置**；None 配置 = 路径不进入，行为不变）。
///
/// 联动（与 [enable_target_fir_p_post] 同构，顺序无关）：
/// 1) T39 design_strict 校验（INFEASIBLE -> E_WIRING_MEASURED_CROSSOVER，fail-closed）；
/// 2) 组合核预算：L_P = max(L_P, L_xo + L_target - 1)；超 options.max_taps 拒绝（不截尾）；
/// 3) F-16 单一延迟定义：P FIR 已含 crossover/target 的群延迟，role 整数延迟归零；
/// 4) 布局政策：仅 4.1.2（含 SUB1 五执行器）允许（非 4.1.2 fail-closed）。
pub fn enable_crossover_p_post(
    design: &mut FitWiringDesign,
    config: CrossoverSpecConfig,
) -> Result<CrossoverPPostOutcome, String> {
    if design.crossover.is_some() {
        return Err(format!(
            "{E_MEASURED_CROSSOVER}: crossover 已配置（禁止重复接线/双份分频）"
        ));
    }
    if !crate::crossover_post::layout_accepts_crossover(&design.plan.layout) {
        return Err(format!(
            "{E_MEASURED_CROSSOVER}: crossover 需要 {} 布局（含 SUB1 五执行器）；当前 layout={:?}（非 4.1.2 布局拒绝策略）",
            crate::crossover_post::CROSSOVER_LAYOUT_REQUIRED,
            design.plan.layout
        ));
    }
    // 预算预检（在昂贵 FFT 设计前 fail-closed）：L_xo + L_target - 1 必须 <= max_taps。
    let target_taps = design.target_fir_taps();
    let required = if target_taps == 0 {
        config.branch_taps()
    } else {
        config.branch_taps() + target_taps - 1
    };
    if required > design.compile.options.max_taps as usize {
        return Err(format!(
            "{E_WIRING_CROSSOVER_BUDGET}: crossover 组合核 {required} taps > options.max_taps={}（taps={} target={}；禁止截尾/静默扩档）",
            design.compile.options.max_taps,
            config.branch_taps(),
            target_taps
        ));
    }
    let wiring = CrossoverWiring::build(
        &config,
        design.plan.sample_rate_hz,
        design.compile.container_channels,
    )?;
    let assignments = wiring.assignments.len();
    design.crossover = Some(wiring);
    design.zero_phase_delay = true;
    design.sync_crossover_p_budget()?;
    let composed_taps = design.crossover_required_taps()? as u32;
    design.validate()?;
    Ok(CrossoverPPostOutcome {
        taps: config.taps as u32,
        composed_taps,
        l_p: design.compile.l_p,
        zero_phase_delay: true,
        assignments,
    })
}

/// EXPERIMENT：measured 设计接入 crossover P_post（要求 experiment.single_point；
/// 真实 rt-host plan-arena 只接受 factorized，显式 force）。
///
/// 产物一律 EXPERIMENT/NON_ACCEPTABLE：真实 ALSA apply / T3 measured / holdout
/// 仍需白天出声窗口，本函数**不执行任何 apply**。
pub fn apply_measured_crossover_experiment(
    design: &mut FitWiringDesign,
    config: CrossoverSpecConfig,
    experiment: ExperimentMode,
) -> Result<CrossoverPPostOutcome, String> {
    if !experiment.single_point {
        return Err(format!(
            "{E_MEASURED_CROSSOVER}: 必须在 experiment.single_point 下调用（crossover 进入 plan 目前仅实验路径；生产验收未完成）"
        ));
    }
    let outcome = enable_crossover_p_post(design, config)?;
    design.compile.options.force_form = Some(FusionForm::Factorized);
    design.compile.experiment = experiment;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// 本机离线 dry-run 夹具（synthetic_offline；不得当板端/实测证据）
// ---------------------------------------------------------------------------

/// 离线 dry-run 输入：stereo-2.0、48k/B128、2 participants/2 roles。
///
/// 语义（证据里必须显式标注 synthetic_offline / measured=false）：
/// - T64：flat 目标 + flat 实测曲线（无模态/宽带修正段，band_count=0；目标 FIR 关闭），
///   因此 T66 plan 的 P 为 identity，delay/polarity 由 T65 op 承载（F-16 无双扣）；
/// - T65：1 个 design 测点 + 1 个 validation 测点；FL 到达 2 samples、FR 到达 5 samples
///   （FR=最晚=D0 参考通道），故 D_FL=3、D_FR=0（非零 D_i 与参考方向都被真实执行）；
/// - T54/T57：mapping/artifact/G path 为确定性夹具（hash 由内容决定，不是全零伪 hash）；
/// - apply：T66 NONRT 离线模型（OfflineSyntheticAck）；T3 后实测 NOT_RUN。
pub fn synthetic_design() -> Result<FitWiringDesign, String> {
    use khaen_designer::modal_peq::{log_frequency_grid, CurvePoint};
    use khaen_designer::targets::{builtin_target, BuiltinOptions, BuiltinTarget};
    use khaen_modal_design::timebase::CommonD0;

    use crate::compile_apply::{MappingBinding, RoleDacSlot};
    use crate::positions::{
        MultipointPosition, QualityScope, RepeatPolicy, POSITION_QUALITY_SCHEMA,
    };

    const FS: u32 = 48_000;
    let plan = MultipointPlan::new(
        "fit-wiring-dryrun",
        "stereo-2.0",
        FS,
        vec!["FL".to_string(), "FR".to_string()],
        CommonD0::zero(),
        "aligned_segment",
        vec![
            MultipointPosition::design("D1", 1.0)
                .map_err(map_join)?
                .with_mic_orientation("90deg"),
            MultipointPosition::validation("H1", 1.0)
                .map_err(map_join)?
                .with_mic_orientation("90deg"),
        ],
    )
    .map_err(map_join)?;
    let quality = PositionQualityReport {
        schema: POSITION_QUALITY_SCHEMA,
        plan_sha256: plan.plan_sha256(),
        scope: QualityScope::Design,
        repeat_policy: RepeatPolicy::DesignOff,
        min_repeats: RepeatPolicy::DesignOff.min_repeats(),
        plan_design_total: plan.design_count(),
        plan_validation_total: plan.validation_count(),
        points_total: plan.design_count(),
        points_usable: plan.design_count(),
        continue_allowed: true,
        reasons: Vec::new(),
        points: Vec::new(),
    };
    let target = builtin_target(BuiltinTarget::Flat, &BuiltinOptions::new(FS as f64))
        .map_err(|error| format!("builtin flat target: {error}"))?;
    let grid = log_frequency_grid(20.0, 20_000.0, 1.0 / 48.0);
    let flat: Vec<CurvePoint> = grid
        .iter()
        .map(|freq| CurvePoint::new(*freq, 0.0))
        .collect();
    let mut curves = Vec::new();
    for position in ["D1", "H1"] {
        for channel in ["FL", "FR"] {
            curves.push(PositionCurve::new(position, channel, flat.clone()));
        }
    }
    let mut peq_options = PeqFitOptions::default();
    peq_options.target_fir.enabled = false; // flat 无残差；P=identity，D_i 由 T65 承载
    let mut phase_options = PhaseFitOptions::default();
    phase_options.reference_position = Some("D1".to_string());
    let raw_grid = log_frequency_grid(80.0, 8_000.0, 1.0 / 24.0);
    let mut phase_responses = Vec::new();
    for (channel, tau_samples) in [("FL", 2.0_f64), ("FR", 5.0_f64)] {
        let points: Vec<crate::phase_fit::PhaseFitPoint> = raw_grid
            .iter()
            .map(|freq| {
                let theta = 2.0 * std::f64::consts::PI * freq * tau_samples / FS as f64;
                crate::phase_fit::PhaseFitPoint::new(*freq, theta.cos(), theta.sin())
            })
            .collect();
        let response =
            DePhasedResponse::from_aligned_segment("D1", channel, FS, points, CommonD0::zero())
                .map_err(|error| format!("synthetic phase response: {error}"))?;
        phase_responses.push(response);
    }
    let mapping = MappingBinding::new(
        "stereo-2.0",
        fixture_hash("t57-overlay-source"),
        fixture_hash("t57-binding"),
        fixture_hash("t57-role"),
        fixture_hash("t57-participants"),
        8,
        vec![0, 1],
        vec![
            RoleDacSlot::new(0, "FL", 0, vec![0]).map_err(|error| error.to_string())?,
            RoleDacSlot::new(1, "FR", 1, vec![1]).map_err(|error| error.to_string())?,
        ],
        vec![2, 3, 4, 5, 6, 7],
        true,
        vec!["FL".to_string(), "FR".to_string()],
    )
    .map_err(|error| error.to_string())?;
    let artifact = ArtifactBinding::from_parts(
        fixture_hash("t54-artifact"),
        fixture_hash("t54-manifest"),
        fixture_hash("t54-wav-aggregate"),
        fixture_hash("t54-solution"),
        fixture_hash("t54-design-plant"),
        fixture_hash("t54-target"),
        fixture_hash("t54-baseline"),
        ArtifactStatus::Ready.as_str(),
    );
    let mut g_paths = Vec::with_capacity(4);
    for out_dense in 0..2u16 {
        for in_dense in 0..2u16 {
            let diag = out_dense == in_dense;
            let mut coeffs = vec![0.0_f64; 8];
            if diag {
                coeffs[0] = 1.0;
            } else {
                coeffs[1] = 0.01 * ((out_dense * 2 + in_dense + 1) as f64);
            }
            let path = GPathInput::new(
                out_dense,
                in_dense,
                coeffs,
                diag,
                false,
                fixture_hash(&format!("t54-g-{out_dense}-{in_dense}")),
            )
            .map_err(|error| error.to_string())?
            .with_source(in_dense);
            g_paths.push(path);
        }
    }
    let mut admission = AdmissionEnv::new();
    admission.sample_rate_hz = FS;
    admission.block_size = 128;
    admission.rt_workers = RT_WORKERS;
    admission.rt_privilege_ok = true;
    admission.clock_source_ok = true;
    admission.cpu_set_ok = true;
    Ok(FitWiringDesign {
        plan,
        target,
        curves,
        channel_bands: Vec::new(),
        design_quality: Some(quality),
        peq_options,
        phase_responses,
        phase_channel_bands: Vec::new(),
        phase_channel_indices: vec![
            PhaseChannelIndex::new("FL", 0),
            PhaseChannelIndex::new("FR", 1),
        ],
        phase_options,
        mapping,
        artifact,
        g_paths,
        compile: CompileWiringConfig {
            block_size: 128,
            n_in: 2,
            container_channels: 8,
            low_band_d: 0,
            l_g: 8,
            l_p: 8,
            encoding: DelayPolarityEncoding::RoleMetadata,
            options: CompileApplyOptions::default(),
            experiment: ExperimentMode::OFF,
            t64_peq_in_plan: true,
        },
        crossover: None,
        admission,
        role_gains_db: Vec::new(),
        design_source: "synthetic_offline".to_string(),
        measured: false,
        zero_phase_delay: false,
        created_unix: 1_790_000_000,
    })
}

/// 确定性 64-hex 夹具 hash（内容 SHA-256，不是全零/全同伪 hash）。
fn fixture_hash(label: &str) -> String {
    sha256_hex(format!("khaen-fit-wiring-fixture/v1\n{label}\n").as_bytes())
}

// ---------------------------------------------------------------------------
// 私有测试（include! tests/T67/fit_wiring.rs；cargo 不自动发现 tests/<Txx>/）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod fit_wiring_tests {
    use super::*;

    include!("../tests/T67/fit_wiring.rs");
}

// 私有测试（include! tests/T70；crossover/T39 分频接入 P_post -> T66 plan）
#[cfg(test)]
mod crossover_p_post_tests {
    use super::*;

    include!("../tests/T70/crossover_p_post.rs");
}
