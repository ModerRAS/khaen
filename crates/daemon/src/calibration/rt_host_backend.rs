//! daemon 侧 rt-host 真实测量 job backend（C3b1 测量 + C3b2a solve）：经 host link 驱动
//! rt-host 的 cal_enter / cal_measure / cal_status / cal_solve / cal_leave，产出真实
//! measurement 证据与单点设计（design.json）。
//!
//! # 为什么是独立 backend
//!
//! 离线 job（OfflineJobBackend）读两段已上传 WAV 做闭环证据，不碰设备；真实测量相反：
//! 扫频由 rt-host 在真实设备上播放、由 UMIK 采集，daemon 只能通过控制通道**驱动**它，
//! 无法自己产生数据。把两者放进同一个 backend 会逼出 "这个 kind 在这个 stage 不做事" 的
//! 静默分支；独立实现让每个 stage 的语义只有一种解释，也便于测试逐 op 断言。
//!
//! # stage 语义（plan = validate -> sweep_capture -> quality_check -> solve_g -> bundle）
//!
//! - validate：目标必须是 rt-host；MicProfile 必须已保存且校准文件可读、sha256 对得上；
//!   job 必须显式授权可闻扫频（P08）；再做一个只读 cal_status 预检（设备缺失/静音/
//!   合成引擎/rt-host 未接线都在进入校准模式前失败，避免 "排一个注定假的 job"）；
//! - sweep_capture：cal_enter（带 MicProfile 的 mic 覆盖 + 可选 channels/level_dbfs）
//!   -> cal_measure（异步 accepted，拿 measure_id/session_id）-> 每 poll_interval（默认
//!   250 ms）轮询 cal_status，每次检查 CancelToken；done 时把真实回执写成 measurement.json；
//!   成功路径**不在这里 leave**（solve_g 需要校准模式下、measurement 成功后的 cal_solve，
//!   leave 延后到 bundle）；失败/取消仍然立刻 cal_leave（C3a 已定：cal_enter/cal_measure/
//!   cal_leave/cal_solve 不重试，connect 失败也只试一次由 host_link 分类）；
//! - quality_check：读 measurement.json，只搬 rt-host 真实给出的 quality/verdict 字段；
//!   rt-host 的 cal_status 当前只暴露测量摘要（measure.result = CalSummary，含 snr_db /
//!   capture_peak_dbfs / xruns / clipped_samples / valid_slots 等），没有闭环质量门对象 ->
//!   quality.json 写 available:false + verdict:null + reason，**绝不写假 PASS**；
//! - solve_g（C3b2a）：cal_solve（校准模式下、measurement 成功后）-> 把回执里的 design 对象
//!   与 verdict/note 逐字段写成 design.json（schema khaen-calibration-rt-host-design/v1，
//!   缺失即 null）；缺 design 对象/与本次 measure_id 不一致 -> 稳定码 E_CAL_SOLVE，不伪造。
//!   设计是 PREDICTED_ONLY：不提交、不改正在跑的输出，本切片也没有 plan compile/apply
//!   （C3b2b）；**design 不是 PASS**；
//! - bundle：cal_leave 幂等（成功路径只在这里 leave 一次；本实例已 leave 过就不再发第二个
//!   op）、写 report.json（真实 job id / measure_id / artifact sha256；design 的 sha/verdict
//!   如实汇总，verdict 仍 null 并说明 plan compile/apply 属 C3b2b）。
//!
//! # leave 时机（C3b2a 的关键约束）
//!
//! rt-host 的 cal_solve 只在“校准模式 + 本会话已有成功测量”时可用，所以成功路径的顺序是
//! validate -> sweep_capture -> quality_check -> solve_g -> bundle，cal_leave 只在 bundle
//! 发一次。任何失败/取消必须**尽早** leave：sweep_capture/quality_check/solve_g 的错误路径
//! 都会先 ensure_leave 再返回原错误（leave 失败只追加 message 并尽力写 cal_leave.json，
//! 不掩盖原错误）；成功路径 leave 失败则 job 以 E_CAL_LEAVE 失败。另外 Drop 兜底：若进过
//! 校准模式、却没有任何 stage 尝试过 leave（例如 job 在 stage 边界被取消），backend 释放时
//! 补一次 cal_leave（只补没尝试过的第一次，绝不重试 C3a 的 leave 语义）。
//!
//! # 诚实性
//!
//! - 不编字段：measurement.json 逐字段拷贝 rt-host cal_status 的 measure 对象与顶层
//!   engine/simulated/real_capture/umik；缺失即 null，不补默认值；
//! - 不假成功：rt-host 业务错误码原样进 job failure.code（例如 mic_declaration_invalid /
//!   device_absent / audio_blocked / format_unsupported），transport 不可用映射成稳定码
//!   E_RT_HOST 且 message 保留 host_link_unavailable 原因；
//! - leave 失败不掩盖原错误：有原错误时把 leave 失败追加到 message（code 保持原错误），
//!   并尽力写 cal_leave.json 证据；成功路径 leave 失败则 job 以 E_CAL_LEAVE 失败。

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use khaen_calibration::job::{
    sha256_hex, ArtifactKind, CalibrationJobBackend, JobId, JobStage, StageCtx, StageFailure,
};

use crate::calibration::mic::{MicProfile, MicProfileStore};
use crate::calibration::service::RtHostJobInputs;
use crate::host_link::{HostLink, Target, WireReply, HOST_LINK_UNAVAILABLE};

/// 真实测量证据 artifact（job artifacts 目录内）。
pub const MEASUREMENT_ARTIFACT: &str = "measurement.json";
/// 质量检查 artifact（available:false 时也写，绝不写假 PASS）。
pub const QUALITY_ARTIFACT: &str = "quality.json";
/// 单点设计 artifact（C3b2a：cal_solve 的真实回执；predicted-only，不是可应用 plan）。
pub const DESIGN_ARTIFACT: &str = "design.json";
/// 结束报告 artifact（真实 sha 汇总；verdict 恒 null）。
pub const REPORT_ARTIFACT: &str = "report.json";
/// leave 失败时的证据 artifact（尽力写；不作为成功证据）。
pub const CAL_LEAVE_ARTIFACT: &str = "cal_leave.json";

/// measurement.json schema。
pub const MEASUREMENT_SCHEMA: &str = "khaen-calibration-rt-host-measurement/v1";
/// quality.json schema。
pub const QUALITY_SCHEMA: &str = "khaen-calibration-rt-host-quality/v1";
/// design.json schema（C3b2a）：job/measure 身份 + cal_solve 回执逐字段拷贝。
pub const DESIGN_SCHEMA: &str = "khaen-calibration-rt-host-design/v1";
/// report.json schema。
pub const REPORT_SCHEMA: &str = "khaen-calibration-rt-host-report/v1";

/// 稳定错误码：目标/链路不是可用的 rt-host 控制通道。
pub const E_RT_HOST: &str = "E_RT_HOST";
/// 稳定错误码：MicProfile 缺失/不可用/校准文件不可读或不匹配。
pub const E_MIC_PROFILE: &str = "E_MIC_PROFILE";
/// 稳定错误码：真实测量缺少可闻扫频授权（P08）。
pub const E_AUDIBLE: &str = "E_AUDIBLE";
/// 稳定错误码：rt-host 当前不具备真实测量条件（设备缺失/静音/合成引擎/未接线）。
pub const UNAVAILABLE: &str = "UNAVAILABLE";
/// 稳定错误码：rt-host 的 cal_status 回执畸形（不猜状态）。
pub const E_CAL_STATUS: &str = "E_CAL_STATUS";
/// 稳定错误码：cal_measure 回执缺少 measure_id/session_id。
pub const E_CAL_MEASURE: &str = "E_CAL_MEASURE";
/// 稳定错误码：测量超时（rt-host 一直 measuring）。
pub const E_MEASURE_TIMEOUT: &str = "E_MEASURE_TIMEOUT";
/// 稳定错误码：成功后 cal_leave 失败（job 不能声称设备已交还）。
pub const E_CAL_LEAVE: &str = "E_CAL_LEAVE";
/// 稳定错误码：measurement.json 缺失/不可解析（quality_check/solve_g/bundle 的前置证据）。
pub const E_MEASUREMENT_ARTIFACT: &str = "E_MEASUREMENT_ARTIFACT";
/// 稳定错误码：cal_solve 回执畸形（缺 design 对象/与本次测量错配），不伪造设计。
pub const E_CAL_SOLVE: &str = "E_CAL_SOLVE";
/// 稳定错误码：design.json 缺失/不可解析（bundle 的前置证据）。
pub const E_DESIGN_ARTIFACT: &str = "E_DESIGN_ARTIFACT";
/// 稳定错误码：job 层会话参数非法（channels 重复/越界、level 越界；API 层已校验，这里兜底）。
pub const E_RT_HOST_INPUT: &str = "E_RT_HOST_INPUT";
/// 稳定错误码：rt-host measurement 回执的状态不在已知集合内。
pub const E_RT_HOST_STATUS: &str = "E_RT_HOST_STATUS";

/// cal_solve 的目标曲线（rt-host ALLOWED_TARGETS 的默认值；显式传，不依赖默认）。
pub const SOLVE_TARGET_FLAT: &str = "flat";

/// 真实测量 job 的轮询配置（测试注入短间隔，生产默认 250 ms / 30 min）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtHostJobConfig {
    /// cal_status 轮询间隔。
    pub poll_interval: Duration,
    /// 单次测量总超时（从 cal_measure accepted 开始计时）。
    pub measurement_timeout: Duration,
}

impl Default for RtHostJobConfig {
    fn default() -> Self {
        RtHostJobConfig {
            poll_interval: Duration::from_millis(250),
            measurement_timeout: Duration::from_secs(30 * 60),
        }
    }
}

/// host link 调用失败的两种语义（请求未到达 vs rt-host 明确拒绝）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RtHostCallError {
    /// rt-host 明确拒绝：稳定错误码 + 人读消息。
    Rejected { code: String, message: String },
    /// 控制通道不可用（connect/超时/协议）：请求可能没送达，不能假装成功。
    Unavailable(String),
}

impl RtHostCallError {
    /// 通用 stage 失败映射：业务码原样保留（要求 4）；链路不可用映射成 E_RT_HOST，
    /// message 里保留 host_link_unavailable 稳定码与真实原因。
    fn stage_failure(&self) -> StageFailure {
        match self {
            RtHostCallError::Rejected { code, message } => {
                StageFailure::failed(code.clone(), message.clone())
            }
            RtHostCallError::Unavailable(message) => StageFailure::failed(
                E_RT_HOST,
                format!(
                    "rt-host control channel is unavailable ({HOST_LINK_UNAVAILABLE}): {message}"
                ),
            ),
        }
    }
}

/// cal_status 回执的最小可信视图（只做类型/枚举校验，不改值）。
#[derive(Debug, Clone)]
struct StatusView {
    /// 完整回执（证据/字段拷贝源）。
    raw: Value,
    /// 顶层 mode（playback/calibration；缺失为 None）。
    mode: Option<String>,
    /// 顶层 engine（缺失为 None）。
    engine: Option<String>,
    /// 顶层 simulated（缺失为 None）。
    simulated: Option<bool>,
    /// 顶层 real_capture（缺失为 None）。
    real_capture: Option<bool>,
    /// 顶层 umik 快照（缺失为 None；有值必须是对象）。
    umik: Option<Value>,
    /// measure 对象原始拷贝。
    measure: Value,
    /// measure.state（idle/measuring/done/failed）。
    measure_state: String,
    /// measure.measure_id（null/缺失为 None）。
    measure_id: Option<u64>,
    /// measure.session_id（null/缺失为 None）。
    session_id: Option<String>,
    /// measure.error（缺失/null 为 None）。
    measure_error: Option<Value>,
    /// measure.result（缺失/null 为 None）。
    measure_result: Option<Value>,
    /// measure.progress 原始拷贝（缺失为 None）。
    progress: Option<Value>,
}

/// 解析 cal_status 回执：只接受结构/类型正确的回执，畸形一律报原因（不猜成功）。
fn parse_status(raw: Value) -> Result<StatusView, String> {
    let object = raw
        .as_object()
        .ok_or_else(|| "cal_status reply must be a JSON object".to_string())?;
    let mode = optional_string(object, "mode")?;
    let engine = optional_string(object, "engine")?;
    let simulated = optional_bool(object, "simulated")?;
    let real_capture = optional_bool(object, "real_capture")?;
    let umik = match object.get("umik") {
        None | Some(Value::Null) => None,
        Some(Value::Object(_)) => object.get("umik").cloned(),
        Some(_) => return Err("cal_status field umik must be an object or null".to_string()),
    };
    let measure_value = object
        .get("measure")
        .cloned()
        .ok_or_else(|| "cal_status reply is missing the object field measure".to_string())?;
    let measure = measure_value
        .as_object()
        .ok_or_else(|| "cal_status field measure must be an object".to_string())?;
    let measure_state = measure
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| "cal_status measure.state is missing or not a string".to_string())?
        .to_string();
    if !matches!(
        measure_state.as_str(),
        "idle" | "measuring" | "done" | "failed"
    ) {
        return Err(format!(
            "cal_status measure.state {:?} is not one of idle/measuring/done/failed",
            measure_state
        ));
    }
    let measure_id =
        match measure.get("measure_id") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| {
                "cal_status measure.measure_id must be a u64 or null".to_string()
            })?),
        };
    let session_id = match measure.get("session_id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| {
                    "cal_status measure.session_id must be a string or null".to_string()
                })?
                .to_string(),
        ),
    };
    let measure_error = match measure.get("error") {
        None | Some(Value::Null) => None,
        Some(Value::Object(_)) => measure.get("error").cloned(),
        Some(_) => return Err("cal_status measure.error must be an object or null".to_string()),
    };
    let measure_result = match measure.get("result") {
        None | Some(Value::Null) => None,
        Some(Value::Object(_)) => measure.get("result").cloned(),
        Some(_) => return Err("cal_status measure.result must be an object or null".to_string()),
    };
    let progress = match measure.get("progress") {
        None | Some(Value::Null) => None,
        Some(Value::Object(_)) => measure.get("progress").cloned(),
        Some(_) => return Err("cal_status measure.progress must be an object or null".to_string()),
    };
    Ok(StatusView {
        raw,
        mode,
        engine,
        simulated,
        real_capture,
        umik,
        measure: measure_value,
        measure_state,
        measure_id,
        session_id,
        measure_error,
        measure_result,
        progress,
    })
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("cal_status field {key} must be a string or null")),
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("cal_status field {key} must be a boolean or null")),
    }
}

/// 从 cal_status 的 device 段取一个可选布尔字段（缺失/非对象返回 None，不改判）。
fn device_bool(status: &Value, key: &str) -> Option<bool> {
    status.get("device")?.get(key)?.as_bool()
}

fn device_str<'a>(status: &'a Value, key: &str) -> Option<&'a str> {
    status.get("device")?.get(key)?.as_str()
}

/// 真实测量 job backend：一个 job 一个实例（含本次 leave/measure 状态）。
pub struct RtHostJobBackend {
    link: Arc<HostLink>,
    mic: MicProfileStore,
    inputs: RtHostJobInputs,
    config: RtHostJobConfig,
    /// cal_leave 是否已成功发出（幂等：成功路径只发一次）。
    left: bool,
    /// cal_leave 尝试次数。
    leave_attempts: u32,
    /// 哪个 stage 发起了 cal_leave（报告证据用）。
    leave_issued_by: Option<&'static str>,
    /// 最近一次 leave 失败原因（有原错误时不掩盖原错误，只追加/记录）。
    leave_error: Option<String>,
    /// 是否真的进过校准模式（cal_enter 成功）。Drop 兜底 leave 只看这个。
    entered_calibration: bool,
}

impl RtHostJobBackend {
    /// 新建：注入控制通道、MicProfile 存储、job 会话输入与轮询配置。
    pub fn new(
        link: Arc<HostLink>,
        mic: MicProfileStore,
        inputs: RtHostJobInputs,
        config: RtHostJobConfig,
    ) -> Self {
        RtHostJobBackend {
            link,
            mic,
            inputs,
            config,
            left: false,
            leave_attempts: 0,
            leave_issued_by: None,
            leave_error: None,
            entered_calibration: false,
        }
    }

    /// 发一个 cal op；失败按 rt-host 拒绝 / 链路不可用两类返回。
    fn call(&self, request: Value) -> Result<Value, RtHostCallError> {
        match self.link.call(request) {
            Ok(WireReply::Success(value)) => Ok(value),
            Ok(WireReply::Failure { code, message, .. }) => {
                Err(RtHostCallError::Rejected { code, message })
            }
            Err(error) => Err(RtHostCallError::Unavailable(error.message)),
        }
    }

    /// 只读 cal_status。
    fn cal_status(&self) -> Result<StatusView, RtHostCallError> {
        let raw = self.call(json!({ "op": "cal_status" }))?;
        parse_status(raw).map_err(|problem| RtHostCallError::Rejected {
            code: E_CAL_STATUS.to_string(),
            message: format!("rt-host cal_status reply is malformed: {problem}"),
        })
    }

    /// cal_leave（幂等：成功后不再发；C3a 定了 cal_leave 不重试，失败只记录）。
    fn ensure_leave(&mut self, issued_by: &'static str) -> Result<(), StageFailure> {
        if self.left {
            return Ok(());
        }
        self.leave_attempts = self.leave_attempts.saturating_add(1);
        self.leave_issued_by = Some(issued_by);
        match self.call(json!({ "op": "cal_leave" })) {
            Ok(_) => {
                self.left = true;
                self.leave_error = None;
                Ok(())
            }
            Err(error) => {
                // message 里保留 rt-host 业务码/链路稳定码（不吞错；job 的 code 固定 E_CAL_LEAVE）。
                let detail = match &error {
                    RtHostCallError::Rejected { code, message } => {
                        format!("{code}: {message}")
                    }
                    RtHostCallError::Unavailable(message) => {
                        format!("{HOST_LINK_UNAVAILABLE}: {message}")
                    }
                };
                let message = format!("cal_leave failed: {detail}");
                self.leave_error = Some(message.clone());
                Err(StageFailure::failed(E_CAL_LEAVE, message))
            }
        }
    }

    /// 失败/取消路径的统一退出：先尽力 cal_leave（幂等），再返回原错误。
    ///
    /// C3b2a 把成功路径的 leave 移到 bundle（solve_g 必须在校准模式下、measurement 成功后
    /// 调 cal_solve），所以 sweep_capture/quality_check/solve_g 的错误路径必须自己尽早
    /// leave。leave 失败不掩盖原错误：code/取消语义保持原样，message 追加 leave 失败，
    /// 并尽力写 cal_leave.json 证据（C3b1 语义）。
    fn fail_with_leave(
        &mut self,
        ctx: &mut StageCtx<'_>,
        issued_by: &'static str,
        error: StageFailure,
    ) -> StageFailure {
        match self.ensure_leave(issued_by) {
            Ok(()) => error,
            Err(leave_failure) => {
                let mut annotated = error.clone();
                annotated.message = format!(
                    "{}; additionally cal_leave failed: {}",
                    error.message, leave_failure.message
                );
                let _ = self.write_leave_artifact(ctx);
                annotated
            }
        }
    }

    // -----------------------------------------------------------------
    // validate
    // -----------------------------------------------------------------

    fn validate(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        // 1) 目标必须是 rt-host（process 目标没有校准数据面，不能假装真实测量）。
        if self.link.target() != Target::RtHost {
            return Err(StageFailure::failed(
                E_RT_HOST,
                format!(
                    "host link target is {} but a real measurement job needs the rt-host control channel (target rt-host)",
                    self.link.target().as_str()
                ),
            ));
        }
        // 2) P08：真实测量会出声/采音，必须显式授权。
        if !ctx.request().audible_sweep_authorized {
            return Err(StageFailure::failed(
                E_AUDIBLE,
                "a real measurement plays an audible sweep: the job request must set audible_sweep_authorized=true",
            ));
        }
        // 3) 会话参数兜底校验（API 层已校验；resume/旧索引路径也走这里）。
        self.validate_inputs()?;
        // 4) MicProfile 必须已保存，且校准文件真实存在 + sha256 对得上（rt-host 会再验一遍）。
        let profile = self.load_profile()?;
        self.check_cal_file(&profile)?;
        ctx.report_progress(0.5);
        // 5) 只读 cal_status 预检：设备缺失/静音/合成引擎/未接线都在进入校准模式前失败。
        match self.cal_status() {
            Ok(status) => self.check_measurement_ready(&status),
            Err(error) => Err(self.map_validate_call_error(&error)),
        }?;
        ctx.report_progress(1.0);
        Ok(())
    }

    fn validate_inputs(&self) -> Result<(), StageFailure> {
        if !self.inputs.audible {
            return Err(StageFailure::failed(
                E_AUDIBLE,
                "rt-host job inputs must set audible=true",
            ));
        }
        if let Some(channels) = &self.inputs.channels {
            if channels.is_empty() {
                return Err(StageFailure::failed(
                    E_RT_HOST_INPUT,
                    "inputs.channels must not be empty when provided",
                ));
            }
            if channels.len() > crate::calibration::service::RT_HOST_MAX_CHANNELS {
                return Err(StageFailure::failed(
                    E_RT_HOST_INPUT,
                    format!(
                        "inputs.channels has {} entries; at most {} channels are supported",
                        channels.len(),
                        crate::calibration::service::RT_HOST_MAX_CHANNELS
                    ),
                ));
            }
            let mut seen = Vec::with_capacity(channels.len());
            for channel in channels {
                if seen.contains(channel) {
                    return Err(StageFailure::failed(
                        E_RT_HOST_INPUT,
                        format!("inputs.channels contains duplicate channel {channel}"),
                    ));
                }
                seen.push(*channel);
            }
        }
        if let Some(level) = self.inputs.level_dbfs {
            if !level.is_finite()
                || !(crate::calibration::service::RT_HOST_MIN_LEVEL_DBFS
                    ..=crate::calibration::service::RT_HOST_MAX_LEVEL_DBFS)
                    .contains(&level)
            {
                return Err(StageFailure::failed(
                    E_RT_HOST_INPUT,
                    format!(
                        "inputs.level_dbfs {level} is outside [{}, {}]",
                        crate::calibration::service::RT_HOST_MIN_LEVEL_DBFS,
                        crate::calibration::service::RT_HOST_MAX_LEVEL_DBFS
                    ),
                ));
            }
        }
        Ok(())
    }

    fn load_profile(&self) -> Result<MicProfile, StageFailure> {
        let load = self.mic.load();
        match load.profile {
            Some(profile) => Ok(profile),
            None => Err(StageFailure::failed(
                E_MIC_PROFILE,
                format!(
                    "no usable microphone profile is saved ({}); PUT /api/calibration/mic with device/orientation/cal_sha256 first",
                    load.reasons.join("; ")
                ),
            )),
        }
    }

    fn check_cal_file(&self, profile: &MicProfile) -> Result<(), StageFailure> {
        let path = std::path::Path::new(&profile.cal_path);
        let bytes = fs::read(path).map_err(|error| {
            StageFailure::failed(
                E_MIC_PROFILE,
                format!(
                    "saved microphone calibration file {} is unreadable: {error}",
                    profile.cal_path
                ),
            )
        })?;
        if bytes.len() as u64 > MIC_CAL_FILE_LIMIT_BYTES {
            return Err(StageFailure::failed(
                E_MIC_PROFILE,
                format!(
                    "saved microphone calibration file {} is {} bytes (limit {} bytes)",
                    profile.cal_path,
                    bytes.len(),
                    MIC_CAL_FILE_LIMIT_BYTES
                ),
            ));
        }
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(profile.cal_sha256.trim()) {
            return Err(StageFailure::failed(
                E_MIC_PROFILE,
                format!(
                    "saved microphone calibration file {} hashes to {actual}, but the profile declares {}",
                    profile.cal_path, profile.cal_sha256
                ),
            ));
        }
        Ok(())
    }

    fn check_measurement_ready(&self, status: &StatusView) -> Result<(), StageFailure> {
        // 合成引擎 = 没有真实采集：不能记录成真实测量。
        if status.simulated == Some(true) {
            return Err(StageFailure::failed(
                UNAVAILABLE,
                format!(
                    "rt-host calibration engine {:?} is the offline synthetic engine (simulated=true): a real measurement job refuses to record simulated data",
                    status.engine.as_deref().unwrap_or("-")
                ),
            ));
        }
        let has_device = status
            .raw
            .get("device")
            .is_some_and(|value| !value.is_null());
        if !has_device {
            return Err(StageFailure::failed(
                UNAVAILABLE,
                "rt-host has not published a runtime device snapshot yet (device=null): wait for the rt-host block loop before starting a real measurement",
            ));
        }
        if device_bool(&status.raw, "present") == Some(false) {
            return Err(StageFailure::failed(
                UNAVAILABLE,
                format!(
                    "rt-host output device is absent (state={:?}); a real measurement needs a running output device",
                    device_str(&status.raw, "state").unwrap_or("-")
                ),
            ));
        }
        if device_bool(&status.raw, "output_started") == Some(false) {
            return Err(StageFailure::failed(
                UNAVAILABLE,
                "rt-host output device has not started (output_started=false): calibration cannot play a sweep",
            ));
        }
        if device_bool(&status.raw, "muted") == Some(true) {
            return Err(StageFailure::failed(
                UNAVAILABLE,
                "rt-host output is muted/safety-muted (device.muted=true): an audible sweep would be silent, so the measurement is rejected instead of fabricating data",
            ));
        }
        Ok(())
    }

    /// validate 阶段把 cal_status 失败收敛到 4 个稳定码之一（rt-host 原始原因保留在 message）。
    fn map_validate_call_error(&self, error: &RtHostCallError) -> StageFailure {
        match error {
            RtHostCallError::Unavailable(message) => StageFailure::failed(
                UNAVAILABLE,
                format!(
                    "rt-host control channel is unavailable ({HOST_LINK_UNAVAILABLE}): {message}"
                ),
            ),
            RtHostCallError::Rejected { code, message } => match code.as_str() {
                // 校准能力在 rt-host 侧没接线/不认识 cal op：不是链路问题，是目标不具备能力。
                "unsupported" | "not_wired" | "unknown_op" => StageFailure::failed(
                    E_RT_HOST,
                    format!("rt-host rejected the cal_status probe ({code}): {message}"),
                ),
                _ => StageFailure::failed(
                    UNAVAILABLE,
                    format!("rt-host cal_status probe was rejected ({code}): {message}"),
                ),
            },
        }
    }

    // -----------------------------------------------------------------
    // sweep_capture
    // -----------------------------------------------------------------

    fn sweep_capture(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        // 成功路径不在这里 leave：solve_g 必须在校准模式下调用 cal_solve，交还设备推迟到
        // bundle（成功路径只 leave 一次）。失败/取消仍然在这里尽早 leave。
        match self.sweep_capture_measured(ctx) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail_with_leave(ctx, "sweep_capture", error)),
        }
    }

    fn sweep_capture_measured(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let profile = self.load_profile()?;
        // cal_enter：带 MicProfile 的会话 mic 覆盖；可选 channels/level_dbfs 原样转发。
        let mut enter = json!({
            "op": "cal_enter",
            "audible": true,
            "mic": {
                "device": profile.device,
                "serial": profile.serial,
                "cal_path": profile.cal_path,
                "cal_sha256": profile.cal_sha256,
                "orientation": profile.orientation,
            },
        });
        if let Some(object) = enter.as_object_mut() {
            if let Some(channels) = &self.inputs.channels {
                object.insert("channels".to_string(), json!(channels));
            }
            if let Some(level) = self.inputs.level_dbfs {
                object.insert("level_dbfs".to_string(), json!(level));
            }
        }
        let enter_reply = self.call(enter).map_err(|error| error.stage_failure())?;
        if enter_reply.get("mode").and_then(Value::as_str) != Some("calibration") {
            return Err(StageFailure::failed(
                E_CAL_STATUS,
                "rt-host cal_enter reply does not report mode=calibration; refusing to start a measurement in an unknown mode (no fabricated measurement)",
            ));
        }
        // 进入校准模式成功：之后任何没有走到 bundle 的异常退出都由 Drop 兜底 leave。
        self.entered_calibration = true;
        ctx.report_progress(0.1);
        ctx.cancel_token().check()?;
        // cal_measure：异步 accepted。
        let measure = self
            .call(json!({ "op": "cal_measure", "audible": true }))
            .map_err(|error| error.stage_failure())?;
        let measure_id = measure
            .get("measure_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                StageFailure::failed(
                    E_CAL_MEASURE,
                    "rt-host cal_measure reply is missing the u64 field measure_id",
                )
            })?;
        let session_id = measure
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                StageFailure::failed(
                    E_CAL_MEASURE,
                    "rt-host cal_measure reply is missing the non-empty string field session_id",
                )
            })?;
        ctx.report_progress(0.2);
        // 轮询：每 poll_interval 一次 cal_status，每次检查 cancel token；总超时可配。
        let deadline = Instant::now() + self.config.measurement_timeout;
        let status = loop {
            ctx.cancel_token().check()?;
            if Instant::now() >= deadline {
                return Err(StageFailure::failed(
                    E_MEASURE_TIMEOUT,
                    format!(
                        "rt-host measurement {measure_id} ({session_id}) did not finish within {:?}",
                        self.config.measurement_timeout
                    ),
                ));
            }
            std::thread::sleep(self.config.poll_interval);
            ctx.cancel_token().check()?;
            let status = self.cal_status().map_err(|error| error.stage_failure())?;
            if let Some(observed) = status.measure_id {
                if observed != measure_id {
                    return Err(StageFailure::failed(
                        E_CAL_STATUS,
                        format!(
                            "rt-host cal_status reports measure_id {observed}, but this job started measurement {measure_id}"
                        ),
                    ));
                }
            }
            if let Some(observed) = status.session_id.as_deref() {
                if observed != session_id {
                    return Err(StageFailure::failed(
                        E_CAL_STATUS,
                        format!(
                            "rt-host cal_status reports session_id {observed:?}, but this job started {session_id:?}"
                        ),
                    ));
                }
            }
            match status.measure_state.as_str() {
                "measuring" => {
                    self.report_measure_progress(ctx, &status);
                    continue;
                }
                "done" => {
                    if status.measure_id.is_none() || status.session_id.is_none() {
                        return Err(StageFailure::failed(
                            E_CAL_STATUS,
                            "rt-host cal_status reports measure.state=done without measure_id/session_id: refusing to record an unidentifiable measurement",
                        ));
                    }
                    if status.measure_result.is_none() {
                        return Err(StageFailure::failed(
                            E_CAL_STATUS,
                            "rt-host cal_status reports measure.state=done without a measure.result summary: refusing to fabricate a measurement result",
                        ));
                    }
                    break status;
                }
                "failed" => return Err(measure_failed_failure(&status, measure_id)),
                other => {
                    return Err(StageFailure::failed(
                        E_CAL_STATUS,
                        format!(
                            "rt-host cal_status reports measure.state {other:?} while measurement {measure_id} is running"
                        ),
                    ))
                }
            }
        };
        ctx.report_progress(0.9);
        let measurement = measurement_artifact(ctx, &status, &self.inputs);
        let bytes = serde_json::to_vec_pretty(&measurement).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("cannot serialize measurement.json: {error}"),
            )
        })?;
        ctx.write_artifact(
            ArtifactKind::Other("rt_host_measurement".to_string()),
            MEASUREMENT_ARTIFACT,
            &bytes,
            &format!("rt-host-cal-status/measure-{measure_id}"),
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    fn report_measure_progress(&self, ctx: &mut StageCtx<'_>, status: &StatusView) {
        let Some(progress) = status.progress.as_ref() else {
            return;
        };
        let done = progress.get("blocks_done").and_then(Value::as_u64);
        let total = progress.get("total_blocks").and_then(Value::as_u64);
        if let (Some(done), Some(total)) = (done, total) {
            if total > 0 {
                let fraction = 0.2 + 0.7 * (done as f32 / total as f32).clamp(0.0, 1.0);
                ctx.report_progress(fraction);
            }
        }
    }

    /// leave 失败证据（尽力写；本身失败不改变返回值）。
    fn write_leave_artifact(&self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        let evidence = json!({
            "schema": "khaen-calibration-rt-host-cal-leave/v1",
            "job_id": ctx.job_id().to_string(),
            "attempts": self.leave_attempts,
            "ok": false,
            "error": self.leave_error,
            "note": "cal_leave failed after the original stage error; the original failure is preserved in job.error (this file only records the leave attempt)",
        });
        let bytes = serde_json::to_vec_pretty(&evidence).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("cannot serialize cal_leave.json: {error}"),
            )
        })?;
        ctx.write_artifact(
            ArtifactKind::Other("rt_host_cal_leave".to_string()),
            CAL_LEAVE_ARTIFACT,
            &bytes,
            "rt-host-cal-leave/failed",
        )
        .map(|_| ())
    }

    // -----------------------------------------------------------------
    // quality_check
    // -----------------------------------------------------------------

    fn quality_check(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        // 设备仍在校准模式（leave 延后到 bundle）：失败/取消必须在这里尽早 leave。
        match self.quality_check_measured(ctx) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail_with_leave(ctx, "quality_check", error)),
        }
    }

    fn quality_check_measured(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        let dir = ctx.artifacts_dir();
        let measurement_path = dir.join(MEASUREMENT_ARTIFACT);
        let bytes = fs::read(&measurement_path).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!(
                    "cannot read measurement artifact {}: {error}",
                    measurement_path.display()
                ),
            )
        })?;
        let measurement: Value = serde_json::from_slice(&bytes).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!(
                    "measurement artifact {} is not valid JSON: {error}",
                    measurement_path.display()
                ),
            )
        })?;
        ctx.report_progress(0.5);
        let measurement_sha256 = sha256_hex(&bytes);
        // 只搬 rt-host 真实给出的质量字段：当前 cal_status 的 measure.result 是测量摘要
        // （CalSummary），没有闭环质量门对象 => available:false + 原因，绝不写假 PASS。
        let result = measurement.get("result");
        let quality = result
            .and_then(|value| value.get("quality"))
            .filter(|value| !value.is_null())
            .cloned();
        let verdict = result
            .and_then(|value| value.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let available = quality.is_some() || verdict.is_some();
        let reason = if available {
            Value::Null
        } else {
            json!("rt-host cal_status exposes the measurement summary (snr_db / capture_peak_dbfs / xruns / clipped_samples / valid_slots / total_slots) but no closed-loop quality/verdict object for this job; the daemon records available:false and does not fabricate a PASS (a closed-loop holdout quality gate is not wired in C3b2a)")
        };
        let quality_json = json!({
            "schema": QUALITY_SCHEMA,
            "job_id": ctx.job_id().to_string(),
            "measure_id": measurement.get("measure_id").cloned().unwrap_or(Value::Null),
            "session_id": measurement.get("session_id").cloned().unwrap_or(Value::Null),
            "available": available,
            "verdict": verdict,
            "quality": quality,
            "reason": reason,
            "measurement_sha256": measurement_sha256,
            "measurement_summary": result.cloned().unwrap_or(Value::Null),
            "checked_unix": unix_now(),
            "note": "quality.json only copies fields that the rt-host cal_status reply actually reported; null means the rt-host reply did not carry the field",
        });
        let quality_bytes = serde_json::to_vec_pretty(&quality_json).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("cannot serialize quality.json: {error}"),
            )
        })?;
        ctx.write_artifact(
            ArtifactKind::QualityReport,
            QUALITY_ARTIFACT,
            &quality_bytes,
            if available {
                "rt-host-cal-status/quality+running"
            } else {
                "rt-host-cal-status/quality-unavailable"
            },
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // -----------------------------------------------------------------
    // solve_g
    // -----------------------------------------------------------------

    fn solve_g(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        // 设备仍在校准模式（leave 延后到 bundle）：solve 失败/取消必须在这里尽早 leave。
        match self.solve_g_measured(ctx) {
            Ok(()) => Ok(()),
            Err(error) => Err(self.fail_with_leave(ctx, "solve_g", error)),
        }
    }

    fn solve_g_measured(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        // 前置证据：measurement.json（本 job 的 measure_id 是设计结果的身份校验依据）。
        let dir = ctx.artifacts_dir();
        let measurement_bytes =
            read_artifact_bytes(&dir, MEASUREMENT_ARTIFACT, E_MEASUREMENT_ARTIFACT)?;
        let measurement: Value = serde_json::from_slice(&measurement_bytes).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("measurement artifact is not valid JSON: {error}"),
            )
        })?;
        ctx.report_progress(0.1);
        ctx.cancel_token().check()?;
        // cal_solve：只在校准模式下、measurement 成功后才调用（cal_enter 在 sweep_capture，
        // cal_leave 延后到 bundle）。target 显式给 flat（rt-host 的默认值）。
        let reply = self
            .call(json!({ "op": "cal_solve", "target": SOLVE_TARGET_FLAT }))
            .map_err(|error| error.stage_failure())?;
        ctx.report_progress(0.5);
        ctx.cancel_token().check()?;
        let design = design_artifact(ctx.job_id(), &measurement, &reply)?;
        let bytes = serde_json::to_vec_pretty(&design).map_err(|error| {
            StageFailure::failed(
                E_CAL_SOLVE,
                format!("cannot serialize design.json: {error}"),
            )
        })?;
        let measure_id = measurement.get("measure_id").and_then(Value::as_u64);
        ctx.write_artifact(
            ArtifactKind::DesignReport,
            DESIGN_ARTIFACT,
            &bytes,
            &format!("rt-host-cal-solve/measure-{}", measure_id.unwrap_or(0)),
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }

    // -----------------------------------------------------------------
    // bundle
    // -----------------------------------------------------------------

    fn bundle(&mut self, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        ctx.cancel_token().check()?;
        // cal_leave 幂等：正常成功路径这里是唯一一次 leave（solve_g 需要校准模式，见模块
        // 文档）；resume 到 bundle（新实例，left=false）时补一次，保证交还设备；已在别的
        // stage/实例 leave 过则不再发第二个 op（C3a：cal_leave 不重试）。
        let left_here = !self.left;
        self.ensure_leave("bundle")?;
        ctx.report_progress(0.4);
        let dir = ctx.artifacts_dir();
        let measurement_bytes =
            read_artifact_bytes(&dir, MEASUREMENT_ARTIFACT, E_MEASUREMENT_ARTIFACT)?;
        let quality_bytes = read_artifact_bytes(&dir, QUALITY_ARTIFACT, E_MEASUREMENT_ARTIFACT)?;
        // design.json 是 solve_g 的前置产物；bundle 不能在没有设计证据时自称完整。
        let design_bytes = read_artifact_bytes(&dir, DESIGN_ARTIFACT, E_DESIGN_ARTIFACT)?;
        let design: Value = serde_json::from_slice(&design_bytes).map_err(|error| {
            StageFailure::failed(
                E_DESIGN_ARTIFACT,
                format!("design artifact is not valid JSON: {error}"),
            )
        })?;
        let measurement: Value = serde_json::from_slice(&measurement_bytes).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("measurement artifact is not valid JSON: {error}"),
            )
        })?;
        let quality: Value = serde_json::from_slice(&quality_bytes).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("quality artifact is not valid JSON: {error}"),
            )
        })?;
        let measurement_sha256 = sha256_hex(&measurement_bytes);
        let quality_sha256 = sha256_hex(&quality_bytes);
        let design_sha256 = sha256_hex(&design_bytes);
        let design_verdict = design.get("verdict").cloned().unwrap_or(Value::Null);
        let verdict = quality.get("verdict").cloned().unwrap_or(Value::Null);
        let quality_available = quality
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let report = json!({
            "schema": REPORT_SCHEMA,
            "job_id": ctx.job_id().to_string(),
            "kind": "single_point_rt_host",
            "generated_unix": unix_now(),
            "measure_id": measurement.get("measure_id").cloned().unwrap_or(Value::Null),
            "session_id": measurement.get("session_id").cloned().unwrap_or(Value::Null),
            "state": measurement.get("state").cloned().unwrap_or(Value::Null),
            "engine": measurement.get("engine").cloned().unwrap_or(Value::Null),
            "simulated": measurement.get("simulated").cloned().unwrap_or(Value::Null),
            "real_capture": measurement.get("real_capture").cloned().unwrap_or(Value::Null),
            "umik": measurement.get("umik").cloned().unwrap_or(Value::Null),
            "verdict": verdict,
            "quality_available": quality_available,
            "verdict_reason": "this job records a real rt-host measurement and a predicted-only single-point design; a closed-loop quality verdict is not wired (no PASS is fabricated), and the design is never compiled/applied in this slice (plan compile/apply: C3b2b); the daemon reports verdict=null instead of a PASS",
            "design": {
                "available": true,
                "sha256": design_sha256,
                "verdict": design_verdict,
                "measure_id": design.get("measure_id").cloned().unwrap_or(Value::Null),
                "applied": design.get("design").and_then(|value| value.get("applied")).cloned().unwrap_or(Value::Null),
                "note": "design.json copies the rt-host cal_solve reply verbatim; it is a PREDICTED_ONLY FIR design, not an applied or verified correction",
            },
            "artifacts": [
                {
                    "name": MEASUREMENT_ARTIFACT,
                    "sha256": measurement_sha256,
                    "bytes": measurement_bytes.len(),
                },
                {
                    "name": QUALITY_ARTIFACT,
                    "sha256": quality_sha256,
                    "bytes": quality_bytes.len(),
                },
                {
                    "name": DESIGN_ARTIFACT,
                    "sha256": design_sha256,
                    "bytes": design_bytes.len(),
                },
            ],
            "artifacts_manifest": "artifacts.manifest.json (written by the khaen-calibration job manager at the terminal state; it lists every artifact with sha256/bytes)",
            "cal_leave": {
                "ok": true,
                "attempts": self.leave_attempts,
                "issued_by": self.leave_issued_by,
                "issued_in_bundle": left_here,
            },
            "notes": [
                "measurement.json is a field-by-field copy of the rt-host cal_status reply (no daemon-side recomputation)",
                "quality.json reports available:false when the rt-host reply carried no quality/verdict object; no PASS is fabricated",
                "design.json is the verbatim rt-host cal_solve reply (predicted-only FIR design); nothing was applied to the running data plane",
                "the job does not compile or apply a plan in this slice (plan compile/apply: C3b2b), so there is no applicable plan",
            ],
        });
        let report_bytes = serde_json::to_vec_pretty(&report).map_err(|error| {
            StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                format!("cannot serialize report.json: {error}"),
            )
        })?;
        ctx.write_artifact(
            ArtifactKind::Other("rt_host_report".to_string()),
            REPORT_ARTIFACT,
            &report_bytes,
            &format!("rt-host-report/{}", sha256_hex(&report_bytes)),
        )?;
        ctx.report_progress(1.0);
        Ok(())
    }
}

/// UMIK 校准文件上限（与 rt-host MIC_CAL_FILE_MAX_BYTES 同源：1 MiB）。
const MIC_CAL_FILE_LIMIT_BYTES: u64 = 1024 * 1024;

/// rt-host 测量失败 -> StageFailure（取消与真实失败分开；rt-host 稳定码原样保留）。
fn measure_failed_failure(status: &StatusView, measure_id: u64) -> StageFailure {
    let error = status.measure_error.as_ref();
    let cancelled = error
        .and_then(|value| value.get("cancelled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let code = error
        .and_then(|value| value.get("code"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            error
                .and_then(|value| value.get("stage_code"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("measure_failed");
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("rt-host measurement failed without an error message");
    let stage_code = error
        .and_then(|value| value.get("stage_code"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("-");
    if cancelled {
        return StageFailure::cancelled(format!(
            "rt-host measurement {measure_id} was cancelled [{code}]: {message}"
        ));
    }
    StageFailure::failed(
        code,
        format!("rt-host measurement {measure_id} failed [code={code}, stage_code={stage_code}]: {message}"),
    )
}

/// measurement.json：逐字段拷贝真实回执（缺失即 null，不补默认值）。
fn measurement_artifact(
    ctx: &StageCtx<'_>,
    status: &StatusView,
    inputs: &RtHostJobInputs,
) -> Value {
    json!({
        "schema": MEASUREMENT_SCHEMA,
        "job_id": ctx.job_id().to_string(),
        "kind": "single_point_rt_host",
        "captured_unix": unix_now(),
        "measure_id": status.measure_id,
        "session_id": status.session_id,
        "state": status.measure_state,
        "engine": status.engine,
        "simulated": status.simulated,
        "real_capture": status.real_capture,
        "mode": status.mode,
        "umik": status.umik,
        "measure": status.measure,
        "result": status.measure_result,
        "summary": status.measure_result,
        "result_source": "rt-host cal_status measure.result (the rt-host measurement summary); stored under both result and summary keys for callers expecting either name — the daemon does not recompute or fill any field",
        "inputs": {
            "audible": inputs.audible,
            "channels": inputs.channels,
            "level_dbfs": inputs.level_dbfs,
        },
    })
}

/// 读一个前置 artifact；缺失/不可读按调用方给的稳定码失败（不猜内容）。
fn read_artifact_bytes(
    dir: &std::path::Path,
    name: &str,
    code: &str,
) -> Result<Vec<u8>, StageFailure> {
    let path = dir.join(name);
    fs::read(&path).map_err(|error| {
        StageFailure::failed(
            code,
            format!("cannot read artifact {}: {error}", path.display()),
        )
    })
}

/// 从 cal_solve 回执构造 design.json（C3b2a）。
///
/// 只做三件事：身份（job_id/measure_id）+ 逐字段拷贝回执（design/verdict/note，缺失即
/// null，不补值）+ 一致性校验（design 必须是对象、measure_id 必须等于本 job 的测量、
/// design.applied 不得为 true）。任何不满足都返回稳定码 E_CAL_SOLVE，**不伪造设计**。
fn design_artifact(
    job_id: JobId,
    measurement: &Value,
    reply: &Value,
) -> Result<Value, StageFailure> {
    let measure_id = match measurement.get("measure_id") {
        Some(value) if value.is_u64() => value.clone(),
        _ => {
            return Err(StageFailure::failed(
                E_MEASUREMENT_ARTIFACT,
                "measurement artifact has no u64 measure_id: an unidentifiable measurement cannot be solved",
            ))
        }
    };
    let object = reply.as_object().ok_or_else(|| {
        StageFailure::failed(E_CAL_SOLVE, "rt-host cal_solve reply must be a JSON object")
    })?;
    let design = object
        .get("design")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            StageFailure::failed(
                E_CAL_SOLVE,
                format!(
                    "rt-host cal_solve reply is missing the object field design (reply keys: [{}]); refusing to fabricate a design",
                    keys.join(", ")
                ),
            )
        })?;
    if design.get("applied").and_then(Value::as_bool) == Some(true) {
        return Err(StageFailure::failed(
            E_CAL_SOLVE,
            "rt-host cal_solve reply claims design.applied=true: solve must never touch the running data plane, so this reply is not treated as a design",
        ));
    }
    let design_measure_id = design.get("measure_id").cloned().unwrap_or(Value::Null);
    if design_measure_id != measure_id {
        return Err(StageFailure::failed(
            E_CAL_SOLVE,
            format!(
                "rt-host cal_solve design.measure_id {design_measure_id} does not match this job's measured measure_id {measure_id}: refusing to attach a design computed from another measurement"
            ),
        ));
    }
    Ok(json!({
        "schema": DESIGN_SCHEMA,
        "job_id": job_id.to_string(),
        "measure_id": measure_id,
        "design": design,
        "verdict": object.get("verdict").cloned().unwrap_or(Value::Null),
        "note": object.get("note").cloned().unwrap_or(Value::Null),
        "source": "rt-host cal_solve reply (design/verdict/note copied verbatim; the daemon does not recompute or apply the design)",
    }))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 兜底交还设备：backend 释放时若“进过校准模式、还没尝试过 cal_leave”，补一次 leave。
///
/// 正常路径不会触发：成功路径 bundle 已 leave（left=true），失败/取消路径对应 stage 已
/// ensure_leave（leave_attempts>0）。它只覆盖没有 stage 可挂的角落（例如 job 在 stage 边界
/// 被取消，worker 直接终态、不会再调用任何 stage），此时至少让 rt-host 回播放模式。
/// 不重试 C3a 语义：leave_attempts>0 时绝不再发第二个 cal_leave。
impl Drop for RtHostJobBackend {
    fn drop(&mut self) {
        if self.entered_calibration && !self.left && self.leave_attempts == 0 {
            let _ = self.call(json!({ "op": "cal_leave" }));
        }
    }
}

impl CalibrationJobBackend for RtHostJobBackend {
    fn run_stage(&mut self, stage: JobStage, ctx: &mut StageCtx<'_>) -> Result<(), StageFailure> {
        match stage {
            JobStage::Validate => self.validate(ctx),
            JobStage::SweepCapture => self.sweep_capture(ctx),
            JobStage::QualityCheck => self.quality_check(ctx),
            JobStage::SolveG => self.solve_g(ctx),
            JobStage::Bundle => self.bundle(ctx),
            other => Err(StageFailure::failed(
                E_RT_HOST_STATUS,
                format!(
                    "stage {} is not part of the single_point_rt_host plan",
                    other.as_str()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cal_status_parsing_rejects_malformed_replies_instead_of_guessing() {
        // 缺 measure 对象：不猜 idle，直接报原因（SweepCapture 会以 E_CAL_STATUS 失败）。
        let error = parse_status(json!({ "ok": true })).expect_err("missing measure");
        assert!(error.contains("measure"), "{error}");
        // 未知 measure.state：不降级成成功。
        let error =
            parse_status(json!({ "measure": { "state": "exploded" } })).expect_err("bad state");
        assert!(error.contains("measure.state"), "{error}");
        // measure_id 类型错：不转字符串、不默认 0。
        let error = parse_status(json!({ "measure": { "state": "measuring", "measure_id": "1" } }))
            .expect_err("bad measure_id");
        assert!(error.contains("measure_id"), "{error}");
        // measure 不是对象。
        assert!(parse_status(json!({ "measure": [] })).is_err());
        // umik 不是对象。
        assert!(parse_status(json!({ "measure": { "state": "idle" }, "umik": 7 })).is_err());
        // 合法最小回执：缺失字段如实 None（不补值）。
        let status = parse_status(json!({
            "measure": { "state": "idle", "measure_id": null, "session_id": null },
        }))
        .expect("minimal status");
        assert_eq!(status.measure_state, "idle");
        assert_eq!(status.measure_id, None);
        assert_eq!(status.session_id, None);
        assert_eq!(status.engine, None);
        assert_eq!(status.simulated, None);
        assert_eq!(status.umik, None);
        assert_eq!(status.measure_result, None);
    }

    #[test]
    fn measure_failures_preserve_rt_host_codes_and_cancellation() {
        let status = parse_status(json!({
            "measure": {
                "state": "failed",
                "measure_id": 7,
                "session_id": "cal-000007-pass01",
                "error": {
                    "code": "measure_failed",
                    "stage_code": "E_QUALITY_SNR",
                    "message": "snr too low",
                    "cancelled": false,
                },
            },
        }))
        .expect("failed status");
        let failure = measure_failed_failure(&status, 7);
        assert!(!failure.cancelled);
        assert_eq!(failure.code, "measure_failed");
        assert!(
            failure.message.contains("E_QUALITY_SNR"),
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("snr too low"),
            "{}",
            failure.message
        );

        // rt-host 的 CANCELLED 失败（cal_leave 取消测量）必须映射成 job cancelled。
        let status = parse_status(json!({
            "measure": {
                "state": "failed",
                "measure_id": 8,
                "session_id": "cal-000008-pass01",
                "error": {
                    "code": "CANCELLED",
                    "stage_code": "CANCELLED",
                    "message": "calibration measurement cancelled (cal_leave/shutdown)",
                    "cancelled": true,
                },
            },
        }))
        .expect("cancelled status");
        let failure = measure_failed_failure(&status, 8);
        assert!(failure.cancelled);
        assert_eq!(failure.code, "CANCELLED");

        // 没有 error 字段的 failed：如实报 measure_failed（不编具体 stage 码）。
        let status = parse_status(json!({
            "measure": { "state": "failed", "measure_id": 9, "session_id": "s" },
        }))
        .expect("failed without error");
        let failure = measure_failed_failure(&status, 9);
        assert_eq!(failure.code, "measure_failed");
        assert!(!failure.cancelled);
    }

    fn solve_reply_for(measure_id: u64) -> Value {
        json!({
            "ok": true,
            "applied": false,
            "verdict": "PREDICTED_ONLY",
            "design": {
                "schema": "khaen-rt-host.cal-design/v1",
                "ok": true,
                "applied": false,
                "verdict": "PREDICTED_ONLY",
                "measure_id": measure_id,
                "mode": "single_point_fir",
                "target": "flat",
                "admission": { "accepted": true },
                "graph_compile": "ok",
            },
            "note": "design artifacts were written next to the measurement; nothing was committed to the running data plane",
        })
    }

    #[test]
    fn design_artifact_copies_the_solve_reply_and_rejects_malformed_designs() {
        let measurement = json!({ "measure_id": 7, "session_id": "cal-000007-pass01" });
        let reply = solve_reply_for(7);
        let design = design_artifact(JobId(3), &measurement, &reply).expect("valid solve reply");
        // 身份是 daemon 写的 job/measure，回执内容逐字段拷贝（不重算）。
        assert_eq!(design["schema"], json!(DESIGN_SCHEMA));
        assert_eq!(design["job_id"], json!("job-0003"));
        assert_eq!(design["measure_id"], json!(7));
        assert_eq!(design["design"], reply["design"]);
        assert_eq!(design["verdict"], json!("PREDICTED_ONLY"));
        assert_eq!(design["note"], reply["note"]);

        // 缺 design 对象 / design 不是对象 / 回执不是对象：E_CAL_SOLVE，不伪造。
        for malformed in [
            json!({ "ok": true, "verdict": "PREDICTED_ONLY" }),
            json!({ "ok": true, "design": [] }),
            json!([1, 2, 3]),
        ] {
            let error = design_artifact(JobId(3), &measurement, &malformed)
                .expect_err("malformed solve reply must be rejected");
            assert_eq!(error.code, E_CAL_SOLVE, "{malformed}");
            assert!(!error.cancelled);
        }

        // 设计属于另一次测量：拒绝（不让别次测量的设计冒充本 job 的证据）。
        let error = design_artifact(JobId(3), &measurement, &solve_reply_for(8))
            .expect_err("measure_id mismatch must be rejected");
        assert_eq!(error.code, E_CAL_SOLVE);

        // 设计声称已被应用：拒绝（solve 不得改运行数据面）。
        let mut applied = solve_reply_for(7);
        applied["design"]["applied"] = json!(true);
        let error = design_artifact(JobId(3), &measurement, &applied)
            .expect_err("applied design must be rejected");
        assert_eq!(error.code, E_CAL_SOLVE);

        // 回执缺 verdict/note：如实 null（不补值）。
        let minimal = json!({ "ok": true, "design": { "measure_id": 7 } });
        let design = design_artifact(JobId(3), &measurement, &minimal).expect("minimal design");
        assert_eq!(design["verdict"], Value::Null);
        assert_eq!(design["note"], Value::Null);

        // measurement 没有可识别的 measure_id：前置证据问题（E_MEASUREMENT_ARTIFACT）。
        for measurement in [json!({}), json!({ "measure_id": null })] {
            let error = design_artifact(JobId(3), &measurement, &reply)
                .expect_err("unidentifiable measurement must be rejected");
            assert_eq!(error.code, E_MEASUREMENT_ARTIFACT, "{measurement}");
        }
    }

    /// 记录 cal op 的假传输（Drop 兜底 leave 的单测用）。
    #[derive(Debug, Default)]
    struct OpSpy {
        calls: std::sync::Mutex<Vec<Value>>,
    }

    impl crate::host_link::Transport for OpSpy {
        fn exchange(
            &self,
            request: &Value,
            _timeout: std::time::Duration,
        ) -> Result<WireReply, crate::host_link::TransportError> {
            self.calls.lock().expect("op spy").push(request.clone());
            Ok(WireReply::Success(
                json!({ "ok": true, "left": true, "mode": "playback" }),
            ))
        }
    }

    fn backend_for_drop_test(transport: Arc<OpSpy>) -> RtHostJobBackend {
        let dir = std::env::temp_dir().join(format!(
            "khaen-rt-host-drop-{}-{:?}",
            std::process::id(),
            transport.as_ref() as *const OpSpy
        ));
        let _ = std::fs::create_dir_all(&dir);
        let link = HostLink::with_transport(
            crate::host_link::HostLinkConfig::rt_host("/tmp/khaen-test-rt-host.sock"),
            transport as Arc<dyn crate::host_link::Transport>,
        );
        RtHostJobBackend::new(
            link,
            MicProfileStore::new(&dir),
            RtHostJobInputs {
                audible: true,
                channels: None,
                level_dbfs: None,
            },
            RtHostJobConfig {
                poll_interval: Duration::from_millis(1),
                measurement_timeout: Duration::from_secs(1),
            },
        )
    }

    #[test]
    fn drop_leaves_calibration_only_when_no_stage_could_do_it() {
        // 进过校准模式、但在 stage 边界被取消（没有任何 stage 尝试过 leave）：Drop 兜底一次。
        let transport = Arc::new(OpSpy::default());
        let mut backend = backend_for_drop_test(Arc::clone(&transport));
        backend.entered_calibration = true;
        drop(backend);
        let calls = transport.calls.lock().expect("op spy").clone();
        assert_eq!(calls.len(), 1, "drop must leave exactly once: {calls:?}");
        assert_eq!(calls[0]["op"], json!("cal_leave"));

        // 已经尝试过 leave（哪怕失败）：不再发第二个（不重试 C3a 的 leave 语义）。
        let transport = Arc::new(OpSpy::default());
        let mut backend = backend_for_drop_test(Arc::clone(&transport));
        backend.entered_calibration = true;
        backend.leave_attempts = 1;
        drop(backend);
        assert!(transport.calls.lock().expect("op spy").is_empty());

        // 没进过校准模式（validate 失败）：不发 leave。
        let transport = Arc::new(OpSpy::default());
        drop(backend_for_drop_test(Arc::clone(&transport)));
        assert!(transport.calls.lock().expect("op spy").is_empty());
    }
}
