//! [NONRT] 图准备：session + 系数 manifest + IR -> 可执行 core 图 / 完整图版本 / identity 保护。
//!
//! 本模块是 file runner 的“prepare”阶段（对齐 T12 core::compile、T09 WorkerDagPlan、T13 IR、T55 manifest）：
//!
//! 1. 加载并校验输入 WAV（`crate::wav`）与每个系数槽（`crate::fir` 的 `tspi-fir/v1`）；
//! 2. 组装 T03 reference 语义图（A：N×L_A；G：M×M×L_G；P：O×L_P）并应用 T13 IR 的 P/phase/D
//!    （`apply_post_to_p`，coeff_embedded 不再加、role_metadata 应用一次）；
//! 3. 用 `core::compile::compile` 做 factorized/fused 成本/长度/预算校验并保留 CompileReport；
//!    4 构造 `core::direct::DirectTopology` 与 `core::workers::WorkerDagPlan`（静态 4/2 worker DAG）；
//! 4. 计算**完整图版本** graph_id（规范化 SHA-256：roles/A/G/P 系数 f32 位、fs/B/D/worker/profile/
//!    ABI/精度/IR 绑定/FIR 文件与 manifest 摘要/protection 策略；**不含** plan_version/created_unix/x_）；
//! 5. identity 保护：binding 失效（IR stale / graph_id 不匹配 / compile 拒绝 / IR 缺失按策略）时，
//!    按 `ProtectionPolicy` 切换到 G=I 但保留 P_post 的 fallback 图（core-v1 §5.5
//!    ProtectionFallback）或安全 mute；绝不半图输出，绝不把失效校准悄悄套用到输出。
//!
//! 说明（与 WBS T14 依赖一致）：T14 的 crate 依赖只有 core + rt-host，因此 T13/T55 通过**数据合同**
//! 对齐（IR 草案 JSON / tspi-fir/v1 manifest）；T12 compile 与 T09 workers 是 core 内的真实接口调用。
//! fused 图只作为设计期产物计算摘要（chosen/reason/fused digest 进元数据），执行形态固定 factorized
//! （fused 的 RT 执行归 T26/T56 hybrid DAG），不做“假合并省算”。

use tspi_dsp_core::compile::{compile, CompileInput, CompileOptions, CompileReport, FusedGraph};
use tspi_dsp_core::direct::{
    DirectCoeffsF32, DirectTopology, DIRECT_MAX_PARTICIPANTS, DIRECT_MAX_ROLES, DIRECT_NO_SOURCE,
};
use tspi_dsp_core::rt::RtStatus;
use tspi_dsp_core::workers::WorkerDagPlan;

use crate::fir::{self, LoadedFir};
use crate::ir::{IrDraft, StaleReason};
use crate::json::Json;
use crate::session::{CoeffSlot, CoefficientKind, ProtectionPolicy, Session, SourceProvider};
use crate::sha256::sha256_hex;
use crate::wav::{self, DecodedAudio};
use crate::{
    RunnerError, RunnerResult, E_FILE_BUDGET, E_FILE_CAPACITY, E_FILE_CHANNELS, E_FILE_GRAPH,
    E_FILE_HASH, E_FILE_IR, E_FILE_IR_STALE, E_FILE_PATH_TABLE, E_FILE_PROTECTION, E_FILE_RATE,
    E_FILE_ROLES, E_FILE_TAPS, E_FILE_VERSION, FALLBACK_MUTE, GRAPH_HASH_DOMAIN,
    TRIGGER_COMPILE_REJECT, TRIGGER_GRAPH_HASH_MISMATCH, TRIGGER_IR_ABSENT, TRIGGER_IR_STALE,
};

/// 实际执行的图种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphKind {
    /// 正常图（A -> G -> P -> scatter）。
    Normal,
    /// identity 保护图：G=I（D=0）但保留 P_post（core-v1 §5.5 fallback）。
    ProtectionIdentityPPost,
    /// 安全 mute（全零输出；无图）。
    ProtectionMute,
}

impl GraphKind {
    /// 稳定字面值（报告用）。
    pub const fn as_str(self) -> &'static str {
        match self {
            GraphKind::Normal => "normal",
            GraphKind::ProtectionIdentityPPost => "identity_p_post",
            GraphKind::ProtectionMute => "mute",
        }
    }
}

/// 可执行的 core 图（factorized A/G/P + 静态 worker DAG）。
#[derive(Clone, Debug)]
pub struct ExecutableGraph {
    /// 拓扑（A_map/scatter/participants/D/支撑）。
    pub topology: DirectTopology,
    /// 静态 worker DAG（T09；含 block/worker/plan_version）。
    pub plan: WorkerDagPlan,
    /// A 系数（N×L_A，row-major）。
    pub a: Vec<f32>,
    /// G 系数（M×M×L_G，row-major；不含 delta_D baseline）。
    pub g: Vec<f32>,
    /// P 系数（O×L_P，row-major）。
    pub p: Vec<f32>,
    /// L_A。
    pub l_a: u32,
    /// L_G。
    pub l_g: u32,
    /// L_P。
    pub l_p: u32,
    /// 完整路径长度 L_F = L_A + L_G + L_P - 2（含 D）。
    pub l_total: u32,
    /// 流式 flush 零长度 = L_F - 1（T03 SPEC §3.3）。
    pub flush_zeros: u32,
    /// 采样率。
    pub sample_rate_hz: u32,
    /// block 大小。
    pub block_size: u16,
    /// worker 数。
    pub expected_workers: u8,
    /// plan 版本。
    pub plan_version: u32,
}

impl ExecutableGraph {
    /// 系数借用视图（[RT] 只读）。
    pub fn coeffs(&self) -> DirectCoeffsF32<'_> {
        DirectCoeffsF32 {
            a_fir: &self.a,
            g: &self.g,
            p: &self.p,
        }
    }

    /// 容器通道数。
    pub fn container_channels(&self) -> usize {
        self.topology.container_channels as usize
    }

    /// 执行 state 样本数（布局）。
    pub fn state_samples(&self) -> usize {
        self.plan.layout.state_samples()
    }
}

/// 一个已加载校验的系数槽。
#[derive(Clone, Debug)]
pub struct LoadedSlot {
    /// 原始槽声明。
    pub slot: CoeffSlot,
    /// 校验后的 manifest + f32 系数。
    pub loaded: LoadedFir,
}

/// 系数槽绑定摘要（元数据）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirSlotBinding {
    /// 槽字面值（a:src / g:row,col / p:role）。
    pub kind: String,
    /// 系数文件。
    pub file: String,
    /// 家族。
    pub family: String,
    /// taps。
    pub taps: u32,
    /// 采样率。
    pub fs: u32,
    /// 声明延迟。
    pub delay_samples: f64,
    /// 系数文件 SHA-256。
    pub file_sha256: String,
    /// manifest 语义摘要 SHA-256。
    pub manifest_sha256: String,
    /// manifest profile。
    pub profile: String,
    /// 预算判定。
    pub fits_budget: bool,
}

/// T12 编译摘要（元数据；如实记录 chosen/reason/长度/成本）。
#[derive(Clone, Debug, PartialEq)]
pub struct CompileSummary {
    /// 选择的形态（factorized/fused）。
    pub chosen: String,
    /// 选择原因。
    pub reason: String,
    /// fused 是否可行。
    pub fused_feasible: bool,
    /// fused 系数是否实际生成。
    pub fused_built: bool,
    /// L_F = L_A + L_G + L_P - 2。
    pub full_path_len: u32,
    /// L_G + L_P - 1。
    pub fused_len_gp: u32,
    /// 非零矩阵路径数。
    pub route_count: u32,
    /// factorized 每采样 MAC。
    pub factorized_macs_per_sample: u64,
    /// fused 每采样 MAC。
    pub fused_macs_per_sample: u64,
    /// 节省 MAC（>0 = fused 更省）。
    pub saved_macs_per_sample: i64,
    /// taps 上限。
    pub taps_limit: u32,
    /// extended 是否准入。
    pub extended_taps_admitted: bool,
}

/// IR 绑定信息（元数据）。
#[derive(Clone, Debug)]
pub struct IrBinding {
    /// 解析后的 IR。
    pub draft: IrDraft,
    /// stale 原因（空 = fresh）。
    pub stale: Vec<StaleReason>,
    /// 是否已应用 P/phase/D（false = identity 保护/未应用）。
    pub applied: bool,
}

/// 保护触发记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectionEngaged {
    /// 触发原因（graph_hash_mismatch/ir_stale/compile_reject/ir_absent）。
    pub reason: String,
    /// fallback 策略（identity_p_post/mute）。
    pub fallback: String,
    /// 实际图种类。
    pub graph_kind: String,
}

/// prepare 结果：可直接交给 `crate::runner` 执行。
#[derive(Clone, Debug)]
pub struct PreparedSession {
    /// session。
    pub session: Session,
    /// 解码后的输入（planar f32；T22 decode 语义）。
    pub input: DecodedAudio,
    /// 输入文件字节 SHA-256。
    pub input_file_sha256: String,
    /// 输入文件字节数。
    pub input_file_bytes: usize,
    /// 完整图版本 graph_id（规范 SHA-256）。
    pub graph_id: String,
    /// fused 图摘要（不可行/未生成时为 None；仅设计期元数据）。
    pub fused_digest: Option<String>,
    /// T12 编译摘要（保护路径可能 None）。
    pub compile: Option<CompileSummary>,
    /// 实际执行的图（mute 时为 None）。
    pub graph: Option<ExecutableGraph>,
    /// 实际图种类。
    pub graph_kind: GraphKind,
    /// 保护触发记录（未触发为 None）。
    pub protection: Option<ProtectionEngaged>,
    /// IR 绑定（无 IR 为 None）。
    pub ir_binding: Option<IrBinding>,
    /// 系数槽绑定。
    pub fir_bindings: Vec<FirSlotBinding>,
    /// 警告（session/manifest/x_ 等）。
    pub warnings: Vec<String>,
}

impl PreparedSession {
    /// 是否处于保护路径（identity_p_post/mute）。
    pub fn is_protected(&self) -> bool {
        self.protection.is_some()
    }

    /// IR stale 字面值列表（无 IR 为空）。
    pub fn ir_stale_codes(&self) -> Vec<&'static str> {
        match &self.ir_binding {
            Some(binding) => binding.stale.iter().map(|reason| reason.as_str()).collect(),
            None => Vec::new(),
        }
    }
}

/// 解析 session 文本并 prepare。
pub fn prepare_from_json(
    session_text: &str,
    source: &dyn SourceProvider,
) -> RunnerResult<PreparedSession> {
    let session = crate::session::parse_session(session_text)?;
    prepare(session, source)
}

/// prepare：加载/校验输入与系数、应用 IR、编译校验、图版本、identity 保护。
pub fn prepare(session: Session, source: &dyn SourceProvider) -> RunnerResult<PreparedSession> {
    // 1) 输入 WAV
    let input_bytes = source.load(&session.audio.input_file)?;
    let input_file_sha256 = sha256_hex(&input_bytes);
    if let Some(expected) = session.audio.input_sha256.as_deref() {
        if !input_file_sha256.eq_ignore_ascii_case(expected) {
            return Err(RunnerError::new(
                E_FILE_HASH,
                format!(
                    "input WAV sha256 mismatch (session {}, computed {})",
                    expected, input_file_sha256
                ),
            ));
        }
    }
    let input_file_bytes = input_bytes.len();
    let input = wav::decode_wav(&input_bytes)?;
    if input.info.channels != session.audio.input_channels {
        return Err(RunnerError::new(
            E_FILE_CHANNELS,
            format!(
                "input WAV channels {} != session input_channels {}",
                input.info.channels, session.audio.input_channels
            ),
        ));
    }
    if input.info.sample_rate != session.audio.sample_rate_hz {
        return Err(RunnerError::new(
            E_FILE_RATE,
            format!(
                "input WAV sample_rate {} != session fs {}",
                input.info.sample_rate, session.audio.sample_rate_hz
            ),
        ));
    }

    // 2) 系数槽
    let mut loaded: Vec<LoadedSlot> = Vec::with_capacity(session.coefficients.len());
    for slot in &session.coefficients {
        loaded.push(load_slot(slot, &session, source)?);
    }
    let mut warnings = session.warnings.clone();
    for slot in &loaded {
        for warning in &slot.loaded.manifest.warnings {
            warnings.push(format!(
                "{}: {}",
                slot.loaded.manifest.coefficients.file, warning
            ));
        }
    }

    // 3) 基础 P（不做 IR 变换）与 IR 结构校验
    let base_p_rows = build_p_base(&loaded, session.roles.len())?;
    let (base_p_array, l_p_base) = build_p_array(&base_p_rows);
    if let Some(ir) = session.ir.as_ref() {
        validate_ir_mapping(ir, &session, &base_p_rows)?;
    }
    let stale = session.ir_stale_reasons();
    let fir_bindings = binding_list(&loaded);

    // 4) 保护：IR stale / IR 缺失（按策略）
    if !stale.is_empty() {
        if trigger_enabled(&session.protection, TRIGGER_IR_STALE) {
            return prepare_protected(
                session,
                input,
                input_file_sha256,
                input_file_bytes,
                &loaded,
                &base_p_array,
                l_p_base,
                stale,
                fir_bindings,
                warnings,
                TRIGGER_IR_STALE,
            );
        }
        let codes = stale
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>()
            .join(",");
        return Err(RunnerError::new(
            E_FILE_IR_STALE,
            format!(
                "calibration IR is stale ({}) and protection is not configured",
                codes
            ),
        ));
    }
    if session.ir.is_none() && trigger_enabled(&session.protection, TRIGGER_IR_ABSENT) {
        return prepare_protected(
            session,
            input,
            input_file_sha256,
            input_file_bytes,
            &loaded,
            &base_p_array,
            l_p_base,
            stale,
            fir_bindings,
            warnings,
            TRIGGER_IR_ABSENT,
        );
    }

    // 5) normal 图：P 应用 IR（唯一应用点），组装 A/G/P + 图版本
    let p_rows = match session.ir.as_ref() {
        Some(ir) => {
            let mut rows = Vec::with_capacity(base_p_rows.len());
            for (role, base) in session.roles.iter().zip(base_p_rows.iter()) {
                rows.push(ir.apply_post_to_p(&role.name, base)?);
            }
            rows
        }
        None => base_p_rows.clone(),
    };
    let (p_array, l_p) = build_p_array(&p_rows);
    let (a_array, l_a) = build_a(&loaded, session.audio.input_channels)?;
    let (g_array, l_g) = build_g(&loaded, session.participants.len(), session.d_samples)?;
    if l_a > u16::MAX as u32 || l_g > u16::MAX as u32 || l_p > u16::MAX as u32 {
        return Err(RunnerError::new(
            E_FILE_TAPS,
            "graph support exceeds u16 capacity",
        ));
    }
    let graph_id = graph_hash(
        &session,
        &loaded,
        session.ir.as_ref(),
        &a_array,
        &g_array,
        &p_array,
        l_a,
        l_g,
        l_p,
        &session.protection.fallback,
    );
    if let Some(declared) = session.engine.graph_id.as_deref() {
        if !declared.eq_ignore_ascii_case(&graph_id) {
            if trigger_enabled(&session.protection, TRIGGER_GRAPH_HASH_MISMATCH) {
                return prepare_protected(
                    session,
                    input,
                    input_file_sha256,
                    input_file_bytes,
                    &loaded,
                    &base_p_array,
                    l_p_base,
                    stale,
                    fir_bindings,
                    warnings,
                    TRIGGER_GRAPH_HASH_MISMATCH,
                );
            }
            return Err(RunnerError::new(
                E_FILE_GRAPH,
                format!(
                    "engine.graph_id mismatch (declared {}, computed {})",
                    declared, graph_id
                ),
            ));
        }
    }

    // 6) T12 compile（选择/长度/预算）与 T09 DAG
    match build_normal_graph(&session, &a_array, &g_array, &p_array, l_a, l_g, l_p) {
        Ok((graph, summary, fused_digest)) => {
            let ir_binding = session_ir_binding(&session, stale, true);
            Ok(PreparedSession {
                session,
                input,
                input_file_sha256,
                input_file_bytes,
                graph_id,
                fused_digest,
                compile: Some(summary),
                graph: Some(graph),
                graph_kind: GraphKind::Normal,
                protection: None,
                ir_binding,
                fir_bindings,
                warnings,
            })
        }
        Err(error) => {
            if trigger_enabled(&session.protection, TRIGGER_COMPILE_REJECT) {
                prepare_protected(
                    session,
                    input,
                    input_file_sha256,
                    input_file_bytes,
                    &loaded,
                    &base_p_array,
                    l_p_base,
                    stale,
                    fir_bindings,
                    warnings,
                    TRIGGER_COMPILE_REJECT,
                )
            } else {
                Err(error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 系数加载/组装
// ---------------------------------------------------------------------------

fn load_slot(
    slot: &CoeffSlot,
    session: &Session,
    source: &dyn SourceProvider,
) -> RunnerResult<LoadedSlot> {
    let manifest_json: Json = match (&slot.manifest_inline, &slot.manifest_file) {
        (Some(json), None) => json.clone(),
        (None, Some(file)) => {
            let bytes = source.load(file)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                RunnerError::new(E_FILE_GRAPH, format!("manifest '{}' is not UTF-8", file))
            })?;
            Json::parse(text)?
        }
        _ => {
            return Err(RunnerError::new(
                E_FILE_PATH_TABLE,
                "coefficient slot must have exactly one of manifest/manifest_file",
            ))
        }
    };
    let data = source.load(&slot.file)?;
    let loaded = fir::load_fir_manifest(&manifest_json, &data, &session.engine.profile)?;
    if loaded.manifest.fs != session.audio.sample_rate_hz {
        return Err(RunnerError::new(
            E_FILE_RATE,
            format!(
                "coefficient '{}' fs {} != session fs {}",
                slot.file, loaded.manifest.fs, session.audio.sample_rate_hz
            ),
        ));
    }
    if loaded.manifest.coefficients.file != slot.file {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!(
                "slot file '{}' != manifest coefficients.file '{}'",
                slot.file, loaded.manifest.coefficients.file
            ),
        ));
    }
    Ok(LoadedSlot {
        slot: slot.clone(),
        loaded,
    })
}

fn build_a(loaded: &[LoadedSlot], n_src: u16) -> RunnerResult<(Vec<f32>, u32)> {
    let mut rows: Vec<&[f32]> = Vec::with_capacity(n_src as usize);
    for src in 0..n_src {
        let slot = loaded
            .iter()
            .find(|slot| slot.slot.kind == CoefficientKind::Input { src })
            .ok_or_else(|| {
                RunnerError::new(
                    E_FILE_PATH_TABLE,
                    format!("missing A coefficient for src {}", src),
                )
            })?;
        rows.push(&slot.loaded.coeffs);
    }
    let l_a = rows.iter().map(|row| row.len()).max().unwrap_or(1) as u32;
    if l_a == 0 || l_a > u16::MAX as u32 {
        return Err(RunnerError::new(E_FILE_TAPS, "A support invalid"));
    }
    let mut out = vec![0.0f32; n_src as usize * l_a as usize];
    for (src, row) in rows.iter().enumerate() {
        let base = src * l_a as usize;
        out[base..base + row.len()].copy_from_slice(row);
    }
    Ok((out, l_a))
}

fn build_g(loaded: &[LoadedSlot], m: usize, d_samples: u32) -> RunnerResult<(Vec<f32>, u32)> {
    let mut cells: Vec<Option<&[f32]>> = vec![None; m * m];
    let mut max_len = 0usize;
    for slot in loaded {
        if let CoefficientKind::Matrix { row, col } = slot.slot.kind {
            let index = row as usize * m + col as usize;
            if index < cells.len() {
                cells[index] = Some(&slot.loaded.coeffs);
                max_len = max_len.max(slot.loaded.coeffs.len());
            }
        }
    }
    let min_len = d_samples as usize + 1;
    let l_g = max_len.max(min_len).max(1);
    if l_g > u16::MAX as usize {
        return Err(RunnerError::new(E_FILE_TAPS, "G support exceeds u16"));
    }
    let mut out = vec![0.0f32; m * m * l_g];
    for (cell, row) in cells.iter().enumerate() {
        if let Some(coeffs) = row {
            let base = cell * l_g;
            out[base..base + coeffs.len()].copy_from_slice(coeffs);
        }
    }
    Ok((out, l_g as u32))
}

fn build_p_base(loaded: &[LoadedSlot], n_role: usize) -> RunnerResult<Vec<Vec<f32>>> {
    let mut rows: Vec<Vec<f32>> = Vec::with_capacity(n_role);
    for role in 0..n_role {
        let kind = CoefficientKind::Output { role: role as u16 };
        let slot = loaded
            .iter()
            .find(|slot| slot.slot.kind == kind)
            .ok_or_else(|| {
                RunnerError::new(
                    E_FILE_PATH_TABLE,
                    format!("missing P coefficient for role {}", role),
                )
            })?;
        rows.push(slot.loaded.coeffs.clone());
    }
    Ok(rows)
}

fn build_p_array(rows: &[Vec<f32>]) -> (Vec<f32>, u32) {
    let l_p = rows.iter().map(|row| row.len()).max().unwrap_or(1).max(1);
    let mut out = vec![0.0f32; rows.len() * l_p];
    for (role, row) in rows.iter().enumerate() {
        let base = role * l_p;
        out[base..base + row.len()].copy_from_slice(row);
    }
    (out, l_p as u32)
}

fn identity_a(n_src: u16) -> Vec<f32> {
    vec![1.0f32; n_src as usize]
}

fn identity_g(m: usize) -> Vec<f32> {
    vec![0.0f32; m * m]
}

fn binding_list(loaded: &[LoadedSlot]) -> Vec<FirSlotBinding> {
    loaded
        .iter()
        .map(|slot| FirSlotBinding {
            kind: kind_str(&slot.slot.kind),
            file: slot.loaded.manifest.coefficients.file.clone(),
            family: slot.loaded.manifest.family.clone(),
            taps: slot.loaded.manifest.taps,
            fs: slot.loaded.manifest.fs,
            delay_samples: slot.loaded.manifest.delay_samples,
            file_sha256: slot.loaded.file_sha256.clone(),
            manifest_sha256: slot.loaded.manifest_sha256.clone(),
            profile: slot.loaded.manifest.limits.profile.clone(),
            fits_budget: slot.loaded.manifest.limits.fits_budget,
        })
        .collect()
}

fn kind_str(kind: &CoefficientKind) -> String {
    match kind {
        CoefficientKind::Input { src } => format!("a:{}", src),
        CoefficientKind::Matrix { row, col } => format!("g:{},{}", row, col),
        CoefficientKind::Output { role } => format!("p:{}", role),
    }
}

fn binding_json(bindings: &[FirSlotBinding]) -> Vec<Json> {
    bindings
        .iter()
        .map(|binding| {
            Json::from_entries(vec![
                ("kind", Json::String(binding.kind.clone())),
                ("file", Json::String(binding.file.clone())),
                ("family", Json::String(binding.family.clone())),
                ("taps", Json::Number(binding.taps as f64)),
                ("fs", Json::Number(binding.fs as f64)),
                ("delay_samples", Json::Number(binding.delay_samples)),
                ("file_sha256", Json::String(binding.file_sha256.clone())),
                (
                    "manifest_sha256",
                    Json::String(binding.manifest_sha256.clone()),
                ),
                ("profile", Json::String(binding.profile.clone())),
                ("fits_budget", Json::Bool(binding.fits_budget)),
            ])
        })
        .collect()
}

/// 系数绑定 JSON（runner 报告用）。
pub fn fir_bindings_json(prepared: &PreparedSession) -> Vec<Json> {
    binding_json(&prepared.fir_bindings)
}

// ---------------------------------------------------------------------------
// IR 结构校验与绑定
// ---------------------------------------------------------------------------

fn validate_ir_mapping(
    ir: &IrDraft,
    session: &Session,
    base_p_rows: &[Vec<f32>],
) -> RunnerResult<()> {
    if ir.roles.len() != session.roles.len() {
        return Err(RunnerError::new(
            E_FILE_IR,
            format!(
                "ir roles count {} != session roles count {}",
                ir.roles.len(),
                session.roles.len()
            ),
        ));
    }
    for (ir_role, session_role) in ir.roles.iter().zip(session.roles.iter()) {
        if ir_role.name != session_role.name || ir_role.index != session_role.scatter as u32 {
            return Err(RunnerError::new(
                E_FILE_IR,
                format!(
                    "ir role '{}'/{} != session role '{}'/{} (physical scatter)",
                    ir_role.name, ir_role.index, session_role.name, session_role.scatter
                ),
            ));
        }
    }
    for (role, base) in session.roles.iter().zip(base_p_rows.iter()) {
        let post = ir.post_role(&role.name).ok_or_else(|| {
            RunnerError::new(E_FILE_IR, format!("ir post missing role '{}'", role.name))
        })?;
        match ir.encoding {
            crate::ir::PostEncoding::CoeffEmbedded => {
                if base.len() != post.p_taps as usize {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!(
                            "role '{}' coeff_embedded p_taps {} != coefficient len {}",
                            role.name,
                            post.p_taps,
                            base.len()
                        ),
                    ));
                }
            }
            crate::ir::PostEncoding::RoleMetadata => {
                if post.p_taps != 1 || base.len() != 1 {
                    return Err(RunnerError::new(
                        E_FILE_TAPS,
                        format!(
                            "role '{}' role_metadata requires p_taps=1 and len=1 (p_taps {}, len {})",
                            role.name,
                            post.p_taps,
                            base.len()
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn session_ir_binding(
    session: &Session,
    stale: Vec<StaleReason>,
    applied: bool,
) -> Option<IrBinding> {
    session.ir.clone().map(|draft| IrBinding {
        draft,
        stale,
        applied,
    })
}

fn trigger_enabled(protection: &ProtectionPolicy, trigger: &str) -> bool {
    protection.triggers.iter().any(|entry| entry == trigger)
}

// ---------------------------------------------------------------------------
// 正常图：T12 compile + T09 DAG
// ---------------------------------------------------------------------------

fn build_normal_graph(
    session: &Session,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    l_a: u32,
    l_g: u32,
    l_p: u32,
) -> RunnerResult<(ExecutableGraph, CompileSummary, Option<String>)> {
    let n_src = session.audio.input_channels;
    let n_role = session.roles.len() as u16;
    let participants = participant_indices(session)?;
    let src_of_role = source_roles(session)?;
    let scatter = scatter_channels(session)?;
    let a64: Vec<f64> = a.iter().map(|value| *value as f64).collect();
    let g64: Vec<f64> = g.iter().map(|value| *value as f64).collect();
    let p64: Vec<f64> = p.iter().map(|value| *value as f64).collect();
    let defaults = CompileOptions::default();
    let options = CompileOptions {
        sample_rate_hz: session.audio.sample_rate_hz,
        block_size: session.engine.block_size,
        max_taps: session.engine.max_taps,
        extended_taps_admitted: session.engine.extended_taps_admitted,
        coeff_budget_bytes: defaults.coeff_budget_bytes,
    };
    let input = CompileInput {
        n_src,
        n_role,
        container_channels: session.audio.container_channels,
        d_samples: session.d_samples,
        participants: &participants,
        src_of_role: &src_of_role,
        scatter: &scatter,
        a: &a64,
        l_a,
        g: &g64,
        l_g,
        p: &p64,
        l_p,
    };
    let plans = compile(&input, &options).map_err(|status| rt_map("core::compile", status))?;
    let summary = summary_from(&plans.report);
    let fused_digest = plans.fused.as_ref().map(fused_digest);
    let graph = make_graph(session, a, g, p, l_a, l_g, l_p)?;
    Ok((graph, summary, fused_digest))
}

fn summary_from(report: &CompileReport) -> CompileSummary {
    CompileSummary {
        chosen: match report.chosen {
            tspi_dsp_core::compile::FusionForm::Factorized => "factorized".to_string(),
            tspi_dsp_core::compile::FusionForm::FusedGp => "fused_gp".to_string(),
        },
        reason: selection_reason_str(report.reason).to_string(),
        fused_feasible: report.fused_feasible,
        fused_built: report.fused_built,
        full_path_len: report.full_path_len,
        fused_len_gp: report.fused_len_gp,
        route_count: report.route_count,
        factorized_macs_per_sample: report.factorized.macs_per_sample,
        fused_macs_per_sample: report.fused.macs_per_sample,
        saved_macs_per_sample: report.saved_macs_per_sample,
        taps_limit: report.taps_limit,
        extended_taps_admitted: report.extended_taps_admitted,
    }
}

fn selection_reason_str(reason: tspi_dsp_core::compile::SelectionReason) -> &'static str {
    use tspi_dsp_core::compile::SelectionReason;
    match reason {
        SelectionReason::FusedCheaper => "fused_cheaper",
        SelectionReason::FusedNotCheaper => "fused_not_cheaper",
        SelectionReason::FusedOverTapLimit => "fused_over_tap_limit",
        SelectionReason::FusedOverRouteLimit => "fused_over_route_limit",
        SelectionReason::FusedOverCoeffBudget => "fused_over_coeff_budget",
    }
}

fn fused_digest(fused: &FusedGraph) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    buffer.extend_from_slice(b"TSPI-FILE-RUNNER-FUSED-v1\n");
    for path in &fused.paths {
        let header = format!(
            "path={}|{}|{}|{}|{}\n",
            path.in_index, path.out_index, path.in_role, path.taps, path.diag_baseline
        );
        buffer.extend_from_slice(header.as_bytes());
        for value in &path.coeffs {
            buffer.extend_from_slice(&(*value as f32).to_bits().to_le_bytes());
        }
    }
    sha256_hex(&buffer)
}

fn participant_indices(session: &Session) -> RunnerResult<Vec<u16>> {
    let mut participants = Vec::with_capacity(session.participants.len());
    for name in &session.participants {
        let index = session.role_position(name).ok_or_else(|| {
            RunnerError::new(E_FILE_ROLES, format!("participant '{}' not in roles", name))
        })?;
        participants.push(index);
    }
    Ok(participants)
}

fn source_roles(session: &Session) -> RunnerResult<Vec<u16>> {
    let mut src_of_role = Vec::with_capacity(session.roles.len());
    for role in &session.roles {
        src_of_role.push(role.src.unwrap_or(DIRECT_NO_SOURCE));
    }
    Ok(src_of_role)
}

fn scatter_channels(session: &Session) -> RunnerResult<Vec<u16>> {
    Ok(session.roles.iter().map(|role| role.scatter).collect())
}

fn make_graph(
    session: &Session,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    l_a: u32,
    l_g: u32,
    l_p: u32,
) -> RunnerResult<ExecutableGraph> {
    let n_src = session.audio.input_channels;
    let n_role = session.roles.len() as u16;
    let participants = participant_indices(session)?;
    let src_vec = source_roles(session)?;
    let scatter_vec = scatter_channels(session)?;
    let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
    let mut participants_array = [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS];
    let mut scatter = [0u16; DIRECT_MAX_ROLES];
    for (index, value) in src_vec.iter().enumerate() {
        src_of_role[index] = *value;
    }
    for (index, value) in participants.iter().enumerate() {
        participants_array[index] = *value;
    }
    for (index, value) in scatter_vec.iter().enumerate() {
        scatter[index] = *value;
    }
    let topology = DirectTopology {
        n_src,
        n_role,
        container_channels: session.audio.container_channels,
        d_samples: session.d_samples,
        l_a: l_a as u16,
        l_g: l_g as u16,
        l_p: l_p as u16,
        m: participants.len() as u16,
        src_of_role,
        participants: participants_array,
        scatter,
    };
    let expected_len = n_src as usize * l_a as usize;
    if a.len() != expected_len {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!(
                "A coefficient len {} != n_src*l_a {}",
                a.len(),
                expected_len
            ),
        ));
    }
    let expected_g = participants.len() * participants.len() * l_g as usize;
    if g.len() != expected_g {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!("G coefficient len {} != m*m*l_g {}", g.len(), expected_g),
        ));
    }
    let expected_p = n_role as usize * l_p as usize;
    if p.len() != expected_p {
        return Err(RunnerError::new(
            E_FILE_PATH_TABLE,
            format!("P coefficient len {} != n_role*l_p {}", p.len(), expected_p),
        ));
    }
    topology
        .validate()
        .is_ok()
        .then_some(())
        .ok_or_else(|| rt_map("DirectTopology::validate", topology.validate()))?;
    let plan = WorkerDagPlan::build(
        &topology,
        session.engine.block_size,
        session.engine.expected_workers,
        session.engine.plan_version,
    )
    .map_err(|status| rt_map("WorkerDagPlan::build", status))?;
    let l_total = l_a
        .checked_add(l_g)
        .and_then(|value| value.checked_add(l_p))
        .and_then(|value| value.checked_sub(2))
        .ok_or_else(|| RunnerError::new(E_FILE_TAPS, "L_F overflow"))?;
    Ok(ExecutableGraph {
        topology,
        plan,
        a: a.to_vec(),
        g: g.to_vec(),
        p: p.to_vec(),
        l_a,
        l_g,
        l_p,
        l_total,
        flush_zeros: l_total - 1,
        sample_rate_hz: session.audio.sample_rate_hz,
        block_size: session.engine.block_size,
        expected_workers: session.engine.expected_workers,
        plan_version: session.engine.plan_version,
    })
}

// ---------------------------------------------------------------------------
// identity 保护（G=I 保 P_post / mute）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn prepare_protected(
    session: Session,
    input: DecodedAudio,
    input_file_sha256: String,
    input_file_bytes: usize,
    loaded: &[LoadedSlot],
    base_p_array: &[f32],
    l_p_base: u32,
    stale: Vec<StaleReason>,
    fir_bindings: Vec<FirSlotBinding>,
    warnings: Vec<String>,
    reason: &str,
) -> RunnerResult<PreparedSession> {
    let fallback = session.protection.fallback.clone();
    let a_identity = identity_a(session.audio.input_channels);
    let g_identity = identity_g(session.participants.len());
    // fallback 是 G=I：低带矩阵不存在，公共 baseline 延迟 D 必须归零，
    // 否则 DirectTopology 会因 L_G(=1) <= D 判非法；D 只属于失效的正常图。
    let mut fallback_session = session.clone();
    fallback_session.d_samples = 0;
    let graph_id = graph_hash(
        &fallback_session,
        loaded,
        session.ir.as_ref(),
        &a_identity,
        &g_identity,
        base_p_array,
        1,
        1,
        l_p_base,
        &fallback,
    );
    let (graph, graph_kind) = if fallback == FALLBACK_MUTE {
        (None, GraphKind::ProtectionMute)
    } else {
        let graph = make_graph(
            &fallback_session,
            &a_identity,
            &g_identity,
            base_p_array,
            1,
            1,
            l_p_base,
        )
        .map_err(|error| {
            RunnerError::new(
                E_FILE_PROTECTION,
                format!("identity_p_post fallback graph invalid: {}", error),
            )
        })?;
        (Some(graph), GraphKind::ProtectionIdentityPPost)
    };
    let protection = Some(ProtectionEngaged {
        reason: reason.to_string(),
        fallback: fallback.clone(),
        graph_kind: graph_kind.as_str().to_string(),
    });
    let ir_binding = session_ir_binding(&session, stale, false);
    Ok(PreparedSession {
        session,
        input,
        input_file_sha256,
        input_file_bytes,
        graph_id,
        fused_digest: None,
        compile: None,
        graph,
        graph_kind,
        protection,
        ir_binding,
        fir_bindings,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// 完整图版本（规范 SHA-256；不含 plan_version/created_unix/x_）
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn graph_hash(
    session: &Session,
    loaded: &[LoadedSlot],
    ir: Option<&IrDraft>,
    a: &[f32],
    g: &[f32],
    p: &[f32],
    l_a: u32,
    l_g: u32,
    l_p: u32,
    protection_fallback: &str,
) -> String {
    let mut buffer: Vec<u8> = Vec::new();
    fn line(buffer: &mut Vec<u8>, text: String) {
        buffer.extend_from_slice(text.as_bytes());
        buffer.push(b'\n');
    }
    line(&mut buffer, GRAPH_HASH_DOMAIN.to_string());
    line(
        &mut buffer,
        format!("session_schema={}", crate::SESSION_SCHEMA),
    );
    line(&mut buffer, format!("precision={}", crate::PRECISION_LOCK));
    line(
        &mut buffer,
        format!("abi_version={}", session.engine.abi_version),
    );
    line(&mut buffer, format!("fs={}", session.audio.sample_rate_hz));
    line(
        &mut buffer,
        format!("block_size={}", session.engine.block_size),
    );
    line(
        &mut buffer,
        format!("expected_workers={}", session.engine.expected_workers),
    );
    line(&mut buffer, format!("profile={}", session.engine.profile));
    line(&mut buffer, format!("max_taps={}", session.engine.max_taps));
    line(
        &mut buffer,
        format!(
            "extended_taps_admitted={}",
            session.engine.extended_taps_admitted
        ),
    );
    line(
        &mut buffer,
        format!("container_channels={}", session.audio.container_channels),
    );
    line(
        &mut buffer,
        format!("input_channels={}", session.audio.input_channels),
    );
    line(
        &mut buffer,
        format!("layout_id={}", session.audio.layout_id),
    );
    line(&mut buffer, format!("d_samples={}", session.d_samples));
    for role in &session.roles {
        line(
            &mut buffer,
            format!(
                "role={}|{}|{}|{}",
                role.name,
                role.index,
                role.src
                    .map(|src| src.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                role.scatter
            ),
        );
    }
    line(
        &mut buffer,
        format!("participants={}", session.participants.join(",")),
    );
    line(&mut buffer, format!("supports={},{},{}", l_a, l_g, l_p));
    push_coeffs(&mut buffer, "A", a);
    push_coeffs(&mut buffer, "G", g);
    push_coeffs(&mut buffer, "P", p);
    match ir {
        Some(ir) => line(
            &mut buffer,
            format!(
                "IR={}|{}|{}|{}|{}|{}",
                ir.schema,
                ir.ir_version,
                ir.ir_hash,
                ir.encoding.as_str(),
                ir.layout.as_deref().unwrap_or("-"),
                ir.profile.profile_sha256
            ),
        ),
        None => line(&mut buffer, "IR=none".to_string()),
    }
    for slot in loaded {
        line(
            &mut buffer,
            format!(
                "FIR={}|{}|{}|{}|{}|{}",
                kind_str(&slot.slot.kind),
                slot.slot.file,
                slot.loaded.file_sha256,
                slot.loaded.manifest_sha256,
                slot.loaded.manifest.family,
                slot.loaded.manifest.taps
            ),
        );
    }
    line(&mut buffer, format!("protection={}", protection_fallback));
    sha256_hex(&buffer)
}

fn push_coeffs(buffer: &mut Vec<u8>, label: &str, coeffs: &[f32]) {
    buffer.extend_from_slice(format!("{}={}\n", label, coeffs.len()).as_bytes());
    for value in coeffs {
        buffer.extend_from_slice(&value.to_bits().to_le_bytes());
    }
}

fn rt_map(context: &str, status: RtStatus) -> RunnerError {
    let code = status.code();
    let error_code = if status == RtStatus::ERR_CHANNELS {
        E_FILE_CHANNELS
    } else if status == RtStatus::ERR_ROLES {
        E_FILE_ROLES
    } else if status == RtStatus::ERR_TAPS {
        E_FILE_TAPS
    } else if status == RtStatus::ERR_PATH_TABLE {
        E_FILE_PATH_TABLE
    } else if status == RtStatus::ERR_CAPACITY {
        E_FILE_CAPACITY
    } else if status == RtStatus::ERR_RATE {
        E_FILE_RATE
    } else if status == RtStatus::ERR_BLOCK_SIZE {
        E_FILE_VERSION
    } else if status == RtStatus::ERR_ADMISSION_SCHED {
        E_FILE_BUDGET
    } else {
        E_FILE_GRAPH
    };
    RunnerError::new(
        error_code,
        format!("{} failed with RtStatus {}", context, code),
    )
}
