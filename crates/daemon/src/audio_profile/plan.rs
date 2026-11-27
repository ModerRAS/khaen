//! profile -> plan ABI v3 生成（docs/API.md §12.3）。
//!
//! 语义（与 §12.3 逐条对应）：
//! 1. 启用输出按 canonical 角色顺序排成 lane 0..N-1（同角色 DAC 升序）；
//!    content.in_channels=22、out_lanes=N；crossover/matrix/protection 的 lanes=N；
//!    routing.dac_channels=22，routes = N 条 (lane -> dac)；未启用 DAC 不进路由。
//! 2. folds：启用输入通道按角色 fan-out 到同角色所有 lane（fanout unity=1.0 / group=1/sqrt(N)，
//!    再乘该源 gain_db）；缺 C -> C 输入额外 fold 到 L/R lane（downmix.center.gain_db）；
//!    缺 LFE/Sub -> LFE 输入额外 fold 到其它启用 lane（downmix.lfe.gain_db，include_heights）。
//! 3. 复用 khaen-plan-builder 做 fail-closed 组装（结构/ABI 上限/lane 链一致性），
//!    再用 khaen_core::plan 的编码器落盘；方案文件写 <state_dir>/plans/<sha256>.plan
//!    （与 POST /api/plan/upload 的内容寻址命名同一约定，原子写）。
//! 4. 生成后经控制通道 op inspect 做只读准入/图编译预览；不自动 prepare/commit。
//!
//! 当前 daemon 没有滤波器状态来源：crossover / matrix 用 identity，protection 用 profile 的
//! 每路 gain_db。响应里 filter_source="identity" 如实标注。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use khaen_core::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan,
    ProtectionSection, RoutingSection,
};
use khaen_host::EngineConfig;
use khaen_plan_builder::{PlanBuilder, PlanConfig};
use serde_json::{json, Map, Value};

use crate::api_error::ApiError;
use crate::host_link::{Target, WireReply};
use crate::DaemonState;

use super::model::{db_to_gain, lfe_downmix_targets, AudioProfile, Lane, SpeakerRole};
use super::store::CurrentProfile;

/// 方案生成失败（handler 翻成 M1 错误；都是内部/配置问题，不是请求字段错误）。
#[derive(Debug, Clone, PartialEq)]
pub enum PlanGenerateError {
    /// 当前已保存 profile 不可读（文件损坏）。
    ProfileUnreadable(String),
    /// profile 校验不过（不应发生：commit 前已校验，但外部改文件也如实报）。
    InvalidProfile(Value),
    /// PlanBuilder 拒绝（稳定错误码 + 说明）。
    Build {
        /// 稳定错误码（PlanBuildErrorCode::as_str）。
        code: String,
        /// 组阶段。
        stage: String,
        /// 人读说明。
        detail: String,
    },
    /// 编码结果不是合法 ABI v3（防御性自检）。
    Abi(String),
    /// 写方案文件失败。
    Io(String),
}

impl std::fmt::Display for PlanGenerateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProfileUnreadable(reason) => {
                write!(formatter, "saved audio profile is unreadable: {reason}")
            }
            Self::InvalidProfile(validation) => {
                write!(formatter, "audio profile is invalid: {validation}")
            }
            Self::Build {
                code,
                stage,
                detail,
            } => write!(formatter, "plan build failed ({stage}/{code}): {detail}"),
            Self::Abi(reason) => write!(formatter, "generated plan is not valid ABI v3: {reason}"),
            Self::Io(reason) => write!(formatter, "cannot write the plan file: {reason}"),
        }
    }
}

impl std::error::Error for PlanGenerateError {}

/// 生成结果（字节 + 寻址 + 预览）。
#[derive(Debug, Clone)]
pub struct GeneratedPlan {
    /// 方案文件路径（daemon 状态目录的 plans/ 下，内容寻址）。
    pub plan_path: String,
    /// ABI v3 字节的 sha256（与文件名同源）。
    pub sha256: String,
    /// lane 数（= 启用输出数）。
    pub lane_count: u64,
    /// 每条 fold 的预览（source/channel/role/lane/gain；下混 fold 带 downmix 标记）。
    pub folds_preview: Vec<Value>,
    /// 下混应用明细（kind/gain_db/lanes）。
    pub downmix_applied: Vec<Value>,
}

/// 组装结果（未落盘；纯函数输出）。
#[derive(Debug, Clone)]
pub struct GeneratedBody {
    /// ABI v3 编码字节。
    pub bytes: Vec<u8>,
    /// lane 数（= 启用输出数）。
    pub lane_count: usize,
    /// 每条 fold 的预览。
    pub folds_preview: Vec<Value>,
    /// 下混应用明细。
    pub downmix_applied: Vec<Value>,
}

/// 把 profile + 引擎格式组装成 ABI v3 方案字节（纯函数，不写盘）。
pub fn generate(
    profile: &AudioProfile,
    config: &EngineConfig,
) -> Result<GeneratedBody, PlanGenerateError> {
    let lanes = profile.enabled_lanes();
    if lanes.is_empty() {
        return Err(PlanGenerateError::InvalidProfile(
            profile.validate().to_json(),
        ));
    }
    let lanes_by_role = lanes_by_role(&lanes);
    let mut folds: Vec<Fold> = Vec::new();
    let mut folds_preview: Vec<Value> = Vec::new();

    // 1) 已启用输入通道按角色 fan-out（AES67 与声卡输入同角色自动求和）。
    for source in &profile.sources {
        if !source.enabled {
            continue;
        }
        let source_gain = db_to_gain(source.gain_db);
        for channel in &source.channels {
            if channel.role == SpeakerRole::None {
                continue;
            }
            let targets = lanes_by_role
                .get(&channel.role)
                .cloned()
                .unwrap_or_default();
            let fanout_gain = profile.fanout.lane_gain(targets.len());
            for lane in targets {
                let gain = source_gain * fanout_gain;
                folds.push(Fold {
                    src: channel.channel,
                    dst: lane.lane,
                    gain,
                });
                folds_preview.push(json!({
                    "source": source.kind.as_str(),
                    "channel": channel.channel,
                    "role": channel.role.as_str(),
                    "lane": lane.lane,
                    "gain": f64::from(gain),
                }));
            }
        }
    }

    // 2) 缺 C -> C 输入下混到 L/R（每 lane 用 downmix.center.gain_db）。
    let mut downmix_applied: Vec<Value> = Vec::new();
    let center_lanes = lanes_by_role
        .get(&SpeakerRole::C)
        .map(Vec::len)
        .unwrap_or(0);
    if profile.downmix.center.mode == super::model::DownmixMode::Auto && center_lanes == 0 {
        let center_gain = db_to_gain(profile.downmix.center.gain_db);
        let targets = lanes
            .iter()
            .filter(|lane| matches!(lane.role, SpeakerRole::L | SpeakerRole::R))
            .copied()
            .collect::<Vec<_>>();
        let mut folded_lanes: BTreeSet<u16> = BTreeSet::new();
        if !targets.is_empty() {
            for (source, channel) in active_channels(profile, &[SpeakerRole::C]) {
                let gain = source_gain(profile, source) * center_gain;
                for lane in &targets {
                    folds.push(Fold {
                        src: channel.channel,
                        dst: lane.lane,
                        gain,
                    });
                    folds_preview.push(json!({
                        "source": source.as_str(),
                        "channel": channel.channel,
                        "role": channel.role.as_str(),
                        "lane": lane.lane,
                        "gain": f64::from(gain),
                        "downmix": "center",
                    }));
                    folded_lanes.insert(lane.lane);
                }
            }
        }
        if !folded_lanes.is_empty() {
            downmix_applied.push(json!({
                "kind": "center",
                "gain_db": f64::from(profile.downmix.center.gain_db),
                "lanes": folded_lanes.into_iter().collect::<Vec<_>>(),
            }));
        }
    }

    // 3) 缺 LFE/Sub -> LFE 输入分配到其它启用 lane（include_heights 控制天空）。
    let low_lanes = lanes.iter().filter(|lane| lane.role.is_low()).count();
    if profile.downmix.lfe.mode == super::model::DownmixMode::Auto && low_lanes == 0 {
        let lfe_gain = db_to_gain(profile.downmix.lfe.gain_db);
        let targets = lfe_downmix_targets(&lanes, profile.downmix.lfe.include_heights);
        let mut folded_lanes: BTreeSet<u16> = BTreeSet::new();
        if !targets.is_empty() {
            for (source, channel) in active_channels(profile, &[SpeakerRole::Lfe, SpeakerRole::Sub])
            {
                let gain = source_gain(profile, source) * lfe_gain;
                for lane in &targets {
                    folds.push(Fold {
                        src: channel.channel,
                        dst: lane.lane,
                        gain,
                    });
                    folds_preview.push(json!({
                        "source": source.as_str(),
                        "channel": channel.channel,
                        "role": channel.role.as_str(),
                        "lane": lane.lane,
                        "gain": f64::from(gain),
                        "downmix": "lfe",
                    }));
                    folded_lanes.insert(lane.lane);
                }
            }
        }
        if !folded_lanes.is_empty() {
            downmix_applied.push(json!({
                "kind": "lfe",
                "gain_db": f64::from(profile.downmix.lfe.gain_db),
                "lanes": folded_lanes.into_iter().collect::<Vec<_>>(),
            }));
        }
    }

    // 4) 复用 plan-builder：content + identity crossover/matrix + protection gain_db + routing。
    let lane_count = u16::try_from(lanes.len()).map_err(|_| PlanGenerateError::Build {
        code: "too_many_lanes".to_string(),
        stage: "content".to_string(),
        detail: format!("{} lanes do not fit the ABI v3 u16 lane count", lanes.len()),
    })?;
    let routes: Vec<(u16, u16)> = lanes.iter().map(|lane| (lane.lane, lane.dac)).collect();
    let builder = PlanBuilder::new(PlanConfig::new(
        u32::try_from(config.block_frames).unwrap_or(u32::MAX),
        config.sample_rate,
        profile.device_channels,
    ))
    .set_content(ContentSection {
        in_channels: profile.device_channels,
        out_lanes: lane_count,
        folds,
    })
    .set_crossover(CrossoverSection {
        lanes: lane_count,
        filters: (0..lane_count)
            .map(|lane| CrossoverFilter {
                lane,
                coeffs: vec![1.0],
            })
            .collect(),
    })
    .set_matrix(identity_matrix(lane_count))
    .set_protection(ProtectionSection {
        lanes: lane_count,
        entries: lanes
            .iter()
            .map(|lane| khaen_core::plan::ProtectionEntry {
                lane: lane.lane,
                delay: 0,
                polarity: 1,
                gain_db: lane.gain_db,
                peq: Vec::new(),
            })
            .collect(),
    })
    .set_routing(RoutingSection {
        dac_channels: profile.device_channels,
        routes,
    });
    let plan = builder
        .build()
        .map_err(|build_error| PlanGenerateError::Build {
            code: build_error.code().as_str().to_string(),
            stage: build_error.stage().as_str().to_string(),
            detail: build_error.detail().to_string(),
        })?;
    let bytes = plan.encode();
    // 防御性自检：落盘口径必须是可解码的 ABI v3（同 plan_store 的 decode 口径）。
    Plan::decode(&bytes)
        .map_err(|decode_error| PlanGenerateError::Abi(decode_error.to_string()))?;
    Ok(GeneratedBody {
        bytes,
        lane_count: lanes.len(),
        folds_preview,
        downmix_applied,
    })
}

/// 编码 ABI v3 并落盘到 <state_dir>/plans/<sha256>.plan（内容寻址 + 原子写）。
pub fn generate_and_store(
    profile: &AudioProfile,
    config: &EngineConfig,
    plans_dir: &std::path::Path,
) -> Result<GeneratedPlan, PlanGenerateError> {
    let generated = generate(profile, config)?;
    let sha256 = khaen_host::sha256_hex(&generated.bytes);
    std::fs::create_dir_all(plans_dir).map_err(|error| {
        PlanGenerateError::Io(format!("cannot create {}: {error}", plans_dir.display()))
    })?;
    let path = plans_dir.join(format!("{sha256}.plan"));
    crate::plan_store::write_atomic(plans_dir, &path, &generated.bytes)
        .map_err(PlanGenerateError::Io)?;
    Ok(GeneratedPlan {
        plan_path: path.display().to_string(),
        sha256,
        lane_count: generated.lane_count as u64,
        folds_preview: generated.folds_preview,
        downmix_applied: generated.downmix_applied,
    })
}

/// POST /api/audio-profile/plan：用已保存 profile + 当前引擎格式生成方案并跑只读 inspect。
pub(crate) async fn generate_plan_endpoint(State(state): State<Arc<DaemonState>>) -> Response {
    let worker = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || compose_generated_plan(&worker)).await;
    match result {
        Ok(Ok(body)) => (StatusCode::OK, Json(body)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(join_error) => {
            ApiError::internal(format!("plan generation task failed: {join_error}")).into_response()
        }
    }
}

fn compose_generated_plan(state: &DaemonState) -> Result<Value, ApiError> {
    let current = state.audio_profile().current();
    let profile = match &current {
        CurrentProfile::Default { .. } => AudioProfile::default(),
        CurrentProfile::Saved(saved) => saved.profile.clone(),
        CurrentProfile::Unreadable { reason } => {
            return Err(ApiError::internal(format!(
                "the saved audio profile is unreadable: {reason}"
            )))
        }
    };
    let validation = profile.validate();
    if !validation.ok() {
        return Err(ApiError::conflict(
            "profile_invalid",
            "the saved audio profile is structurally invalid: fix and commit it before generating a plan",
        )
        .with_detail("validation", validation.to_json()));
    }
    let config = state.engine_config();
    let generated = generate_and_store(&profile, &config, state.plans().dir())
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    let inspect = inspect_plan(state, &generated.plan_path, &generated.sha256, &mut reasons);
    let mut body = json!({
        "ok": true,
        "plan_path": generated.plan_path,
        "sha256": generated.sha256,
        "lane_count": generated.lane_count,
        "inspect": inspect,
        "folds_preview": generated.folds_preview,
        "downmix_applied": generated.downmix_applied,
        "filter_source": "identity",
        "reasons": super::api::reasons_json(&reasons),
    });
    if let Some(object) = body.as_object_mut() {
        object.insert(
            "filter_note".to_string(),
            json!("the daemon has no filter state source: crossover/matrix are identity and protection carries each lane's profile gain_db"),
        );
    }
    Ok(body)
}

/// 只读 inspect（准入预算 + 图编译预览）；拿不到 null + reason，不编结果。
fn inspect_plan(
    state: &DaemonState,
    plan_path: &str,
    sha256: &str,
    reasons: &mut BTreeMap<String, String>,
) -> Value {
    if state.host_link().target() == Target::Process {
        match state.control().inspect_json(plan_path, Some(sha256)) {
            Ok(body) => body,
            Err(error) => {
                reasons.insert("inspect".to_string(), error.to_json().to_string());
                Value::Null
            }
        }
    } else {
        match state.host_link().call(json!({
            "op": "inspect",
            "plan_path": plan_path,
            "plan_sha256": sha256,
        })) {
            Ok(WireReply::Success(body)) => body,
            Ok(WireReply::Failure { code, message, .. }) => {
                reasons.insert(
                    "inspect".to_string(),
                    format!("rt-host rejected the inspect op ({code}: {message})"),
                );
                Value::Null
            }
            Err(error) => {
                reasons.insert("inspect".to_string(), error.message);
                Value::Null
            }
        }
    }
}

fn identity_matrix(lanes: u16) -> MatrixSection {
    let count = usize::from(lanes);
    let mut coeffs = vec![0.0_f32; count * count];
    for lane in 0..count {
        coeffs[lane * count + lane] = 1.0;
    }
    MatrixSection {
        rows: lanes,
        cols: lanes,
        decimation: 1,
        interpolation: 1,
        taps: 1,
        coeffs,
    }
}

fn lanes_by_role(lanes: &[Lane]) -> BTreeMap<SpeakerRole, Vec<Lane>> {
    let mut by_role: BTreeMap<SpeakerRole, Vec<Lane>> = BTreeMap::new();
    for lane in lanes {
        by_role.entry(lane.role).or_default().push(*lane);
    }
    by_role
}

/// 已启用源里某角色组的通道（来源 + 通道）。
fn active_channels(
    profile: &AudioProfile,
    roles: &[SpeakerRole],
) -> Vec<(super::model::SourceKind, super::model::InputChannel)> {
    profile
        .enabled_inputs()
        .into_iter()
        .filter(|(_, channel)| roles.contains(&channel.role))
        .collect()
}

/// 来源整体增益（线性）。
fn source_gain(profile: &AudioProfile, kind: super::model::SourceKind) -> f32 {
    profile.source_gain(kind)
}

/// 把 Plan 摘要（inspect 之外的诊断）序列化（测试/报告用；不直接对外）。
pub fn plan_summary(plan: &Plan) -> Value {
    let mut map = Map::new();
    map.insert("sample_rate".to_string(), json!(plan.sample_rate));
    map.insert("block_size".to_string(), json!(plan.block_size));
    if let Some(content) = &plan.content {
        map.insert("in_channels".to_string(), json!(content.in_channels));
        map.insert("out_lanes".to_string(), json!(content.out_lanes));
        map.insert("folds".to_string(), json!(content.folds.len()));
    }
    if let Some(routing) = &plan.routing {
        map.insert("dac_channels".to_string(), json!(routing.dac_channels));
        map.insert("routes".to_string(), json!(routing.routes));
    }
    if let Some(protection) = &plan.protection {
        map.insert("lanes".to_string(), json!(protection.lanes));
    }
    Value::Object(map)
}
