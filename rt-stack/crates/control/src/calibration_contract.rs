//! 校准 UI/REST 的唯一 JSON 契约（settings / lanes / apply-clear / host pending 字段）。
//!
//! 本模块是 daemon（tspi-rt-daemon）与页面（/calibration 向导 + 控制台校准卡）之间
//! 唯一允许定义校准请求/响应形状的地方。host 侧字段还没最终落地时，这里给出字段名、
//! 类型和 pending 语义；REST 层对未实现的 host 字段返回 **501 pending_host_field**，
//! 不伪造 applied / lanes / ack。
//!
//! schema 版本：
//! - GET/POST /api/calibration/settings -> tspi-cal-settings/v1
//! - GET /api/calibration/status         -> tspi-cal-status/v1
//! - GET /api/calibration/jobs[/{id}]    -> tspi-cal-job/v1（新增 progress_text / lanes）
//! - POST /api/calibration/apply|clear   -> tspi-cal-apply/v1
//! - job.lanes                           -> tspi-cal-lanes/v1
//!
//! 注意：本文件只做 JSON 形状/校验，不打开声卡、不接触 host、不执行测量。

use super::*;

/// 校准设置 schema。
pub const CAL_SETTINGS_SCHEMA: &str = "tspi-cal-settings/v1";
/// 校准总状态 schema。
pub const CAL_STATUS_SCHEMA: &str = "tspi-cal-status/v1";
/// 每 lane 结果 schema。
pub const CAL_LANES_SCHEMA: &str = "tspi-cal-lanes/v1";
/// apply/clear 回执 schema。
pub const CAL_APPLY_SCHEMA: &str = "tspi-cal-apply/v1";

/// 麦克风输入类型：USB 数字麦 / 声卡模拟输入。
pub const MIC_KINDS: [&str; 2] = ["usb", "analog_48v"];
/// 允许的麦克风朝向（度）。
pub const ORIENTATIONS_DEG: [u16; 2] = [0, 90];
/// 目标曲线模式。
pub const TARGET_MODES: [&str; 2] = ["flat", "tilt10"];
/// level 校准 stage 名。
pub const STAGE_LEVEL: &str = "level";
/// delay 校准 stage 名。
pub const STAGE_DELAY: &str = "delay";
/// 4.1.2 七角色（lane 顺序与 MOTU 输出/图一致）。
pub const CAL_LANES: [&str; 7] = ["FL", "FR", "SL", "SR", "HFL", "HFR", "SUB1"];
/// 默认参考 lane（level 相对电平和 delay 相对延迟都以 FL 为基准）。
pub const DEFAULT_REFERENCE_LANE: &str = "FL";
/// level 校准默认目标读数（dBFS）。
pub const DEFAULT_LEVEL_TARGET_DBFS: f64 = -12.0;
/// apply/clear 允许的 target。
pub const APPLY_TARGETS: [&str; 5] = ["level", "delay", "bass", "fir", "modal"];

/// bass management stage 名。
pub const STAGE_BASS: &str = "bass";
/// 每 lane 全频 FIR 校正 stage 名。
pub const STAGE_FIR: &str = "fir";
/// 每 lane modal EQ stage 名。
pub const STAGE_MODAL: &str = "modal";
/// 一键运行链序：crossover(bass) → 全频 FIR → modal EQ → level → delay → emperor/A-B(measure)。
pub const RUN_STAGE_ORDER: [&str; 6] = [STAGE_BASS, STAGE_FIR, STAGE_MODAL, STAGE_LEVEL, STAGE_DELAY, "measure"];
/// 一键运行 dry-run schema。
pub const CAL_RUN_SCHEMA: &str = "tspi-cal-run/v1";
/// 允许的 FIR 相位模式。
pub const FIR_PHASES: [&str; 2] = ["mixed", "linear"];
/// 允许的 FIR 目标曲线。
pub const FIR_TARGET_CURVES: [&str; 3] = ["flat", "tilt10", "custom"];
/// FIR taps 上限（WBS/emperor-focus 抽头预算口径）。
pub const FIR_TAPS_MAX: u32 = 24000;
/// 允许的低音炮/主箱斜率。
pub const SLOPES: [&str; 8] = ["LR2", "LR4", "LR6", "LR8", "BW2", "BW4", "BW6", "BW8"];
/// 分频点范围（Hz）。
pub const CROSSOVER_HZ_MIN: f64 = 40.0;
pub const CROSSOVER_HZ_MAX: f64 = 150.0;

/// host 端尚未最终落地、由本契约登记的字段名。REST 对依赖这些字段的请求返回 501。
pub const PENDING_HOST_FIELDS: [&str; 11] = [
    "mic.device",
    "mic.channel",
    "mic.phantom_48v",
    "mic.gain_db",
    "cal_file",
    "level.lanes",
    "delay.lanes",
    "bass.crossover",
    "bass.guardrail",
    "fir.lanes",
    "modal.lanes",
];

/// 501 pending_host_field（host 字段未落地；不伪造成功）。
pub fn pending_host_field_error(message: impl Into<String>) -> RestError {
    RestError::new(501, CODE_PENDING_HOST_FIELD, message)
}

/// pending host 字段数组（JSON）。
pub fn pending_host_fields_json() -> JsonValue {
    JsonValue::arr(
        PENDING_HOST_FIELDS
            .iter()
            .map(|name| JsonValue::str(*name))
            .collect(),
    )
}

/// 校验提示（结构化给页面显示；不泄露内部路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalValidationMessage {
    /// 稳定码（页面可判定）。
    pub code: &'static str,
    /// 人读说明。
    pub message: String,
}

/// 校准设置的校验结果（绝对/相对标签 + 提示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalSettingsValidation {
    /// 是否可用于绝对读数。
    pub ok: bool,
    /// 绝对/相对人读标签。
    pub label: String,
    /// 警告列表。
    pub warnings: Vec<CalValidationMessage>,
}

impl CalSettingsValidation {
    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("ok", JsonValue::Bool(self.ok)),
            ("label", JsonValue::str(self.label.as_str())),
            (
                "warnings",
                JsonValue::arr(
                    self.warnings
                        .iter()
                        .map(|warning| {
                            JsonValue::obj(vec![
                                ("code", JsonValue::str(warning.code)),
                                ("message", JsonValue::str(warning.message.as_str())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}

/// 麦克风输入设置。
#[derive(Debug, Clone, PartialEq)]
pub struct MicSettings {
    /// usb | analog_48v。
    pub kind: String,
    /// ALSA 设备名（例如 hw:U18dB,0 / hw:UltraLitemk5,0）。
    pub device: String,
    /// 采集通道号（0 起）。
    pub channel: u32,
    /// 48V 幻象电源（仅 analog_48v）。
    pub phantom_48v: bool,
    /// 增益 dB（USB 麦为数字增益，模拟为声卡前置增益）。
    pub gain_db: f64,
}

impl Default for MicSettings {
    fn default() -> Self {
        MicSettings {
            kind: "usb".to_string(),
            device: "hw:U18dB,0".to_string(),
            channel: 0,
            phantom_48v: false,
            gain_db: 18.0,
        }
    }
}

impl MicSettings {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "settings.mic must be a JSON object",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration settings mic");
        let kind = fields
            .take_opt_str("kind")?
            .unwrap_or_else(|| "usb".to_string());
        let device = fields
            .take_opt_str("device")?
            .unwrap_or_else(|| MicSettings::default().device);
        let channel = fields.take_opt_u32("channel")?.unwrap_or(0);
        let phantom_48v = fields.take_opt_bool("phantom_48v")?.unwrap_or(false);
        let gain_db = fields.take_opt_f64("gain_db")?.unwrap_or(18.0);
        fields.finish()?;
        Ok(MicSettings {
            kind,
            device,
            channel,
            phantom_48v,
            gain_db,
        })
    }

    fn validate(&self) -> Result<(), RestError> {
        if !MIC_KINDS.contains(&self.kind.as_str()) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("mic.kind must be one of: {}", MIC_KINDS.join(",")),
            ));
        }
        if self.device.trim().is_empty() || self.device.len() > 128 {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "mic.device must be 1..=128 characters",
            ));
        }
        if self
            .device
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte < 0x20)
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "mic.device must not contain whitespace or control characters",
            ));
        }
        if self.channel > 31 {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "mic.channel must be 0..=31",
            ));
        }
        if !(-20.0..=60.0).contains(&self.gain_db) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "mic.gain_db must be -20..=60 dB",
            ));
        }
        if self.kind == "usb" && self.phantom_48v {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "mic.phantom_48v is only valid for kind=analog_48v",
            ));
        }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("kind", JsonValue::str(self.kind.as_str())),
            ("device", JsonValue::str(self.device.as_str())),
            ("channel", JsonValue::from(self.channel)),
            ("phantom_48v", JsonValue::Bool(self.phantom_48v)),
            ("gain_db", JsonValue::from(self.gain_db)),
        ])
    }
}

/// 可选校准文件引用（绝对读数的依据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalFileRef {
    /// host 可读的绝对路径（可选）。
    pub path: Option<String>,
    /// 浏览器选择的文件名（可选）。
    pub name: Option<String>,
    /// 文件 SHA-256（64 hex，小写；可选但决定绝对/相对）。
    pub sha256: Option<String>,
    /// path | browser。
    pub source: String,
}

impl CalFileRef {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "cal_file must be a JSON object or null",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration settings cal_file");
        let path = fields.take_opt_str("path")?;
        let name = fields.take_opt_str("name")?;
        let sha256 = fields.take_opt_str("sha256")?;
        let source = fields
            .take_opt_str("source")?
            .unwrap_or_else(|| "path".to_string());
        fields.finish()?;
        Ok(CalFileRef {
            path,
            name,
            sha256,
            source,
        })
    }

    fn validate(&self) -> Result<(), RestError> {
        if self.path.as_deref().unwrap_or("").trim().is_empty()
            && self.name.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "cal_file requires path or name",
            ));
        }
        if !matches!(self.source.as_str(), "path" | "browser") {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "cal_file.source must be path or browser",
            ));
        }
        for (field, value) in [("path", &self.path), ("name", &self.name)] {
            if let Some(value) = value {
                if value.len() > 512 {
                    return Err(RestError::bad_request(
                        CODE_INVALID_FIELD,
                        format!("cal_file.{field} must be at most 512 characters"),
                    ));
                }
            }
        }
        if let Some(sha) = &self.sha256 {
            if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "cal_file.sha256 must be 64 hex characters",
                ));
            }
        }
        Ok(())
    }

    fn display_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.path.clone())
            .unwrap_or_else(|| "calibration file".to_string())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("path", opt_string_json(&self.path)),
            ("name", opt_string_json(&self.name)),
            (
                "sha256",
                match &self.sha256 {
                    Some(sha) => JsonValue::str(sha.to_ascii_lowercase()),
                    None => JsonValue::Null,
                },
            ),
            ("source", JsonValue::str(self.source.as_str())),
        ])
    }
}

fn opt_string_json(value: &Option<String>) -> JsonValue {
    match value {
        Some(text) => JsonValue::str(text.as_str()),
        None => JsonValue::Null,
    }
}

/// 单只低音炮配置（多炮 B 方案：SUB1 + SUB-L/SUB-R 都参与 LFE/全局低音）。
#[derive(Debug, Clone, PartialEq)]
pub struct SubConfig {
    /// 炮 id（SUB1 / SUB_L / SUB_R）。
    pub id: String,
    /// 输出去向（MOTU 输出通道/角色；host 映射未落地 -> pending）。
    pub output: String,
    /// 高通 Hz（保护）。
    pub hp_hz: f64,
    /// 低通 Hz。
    pub lp_hz: f64,
    /// 炮增益 dB（相对，-12..=+6）。
    pub gain_db: f64,
    /// 炮延迟样本（0..=96000）。
    pub delay_samples: i64,
    /// 极性 +1 / -1。
    pub polarity: i32,
    /// modal notch 列表（最多 4 个）。
    pub modal_notch: Vec<ModalNotch>,
}

/// 单炮 modal notch（PEQ）。
#[derive(Debug, Clone, PartialEq)]
pub struct ModalNotch {
    /// 频率 Hz。
    pub freq_hz: f64,
    /// 衰减 dB（-24..=0）。
    pub gain_db: f64,
    /// Q（0.1..=20）。
    pub q: f64,
}

/// 低频路由矩阵（输入低频 -> 每炮权重）。
#[derive(Debug, Clone, PartialEq)]
pub struct SubRouting {
    /// all_to_all：LFE 与 L/R 低切按权重分给全部炮；stereo_bias：带左右偏置。
    pub mode: String,
    /// LFE -> 每炮权重（长度 = subs.len()）。
    pub lfe_weights: Vec<f64>,
    /// L -> 每炮权重。
    pub left_weights: Vec<f64>,
    /// R -> 每炮权重。
    pub right_weights: Vec<f64>,
    /// 立体声偏置 -1..=1（0=对称；>0 偏 L 炮，<0 偏 R 炮）。
    pub stereo_bias: f64,
}

impl Default for SubRouting {
    fn default() -> Self {
        SubRouting {
            mode: "all_to_all".to_string(),
            lfe_weights: vec![0.34, 0.33, 0.33],
            left_weights: vec![0.5, 0.25, 0.25],
            right_weights: vec![0.25, 0.25, 0.5],
            stereo_bias: 0.0,
        }
    }
}

impl SubConfig {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs entries must be objects")),
        };
        let mut fields = Fields::new(entries, "calibration settings bass.subs");
        let id = fields.take_str("id")?;
        let output = fields.take_str("output")?;
        let hp_hz = fields.take_opt_f64("hp_hz")?.unwrap_or(20.0);
        let lp_hz = fields.take_opt_f64("lp_hz")?.unwrap_or(120.0);
        let gain_db = fields.take_opt_f64("gain_db")?.unwrap_or(0.0);
        let delay_samples = fields.take_opt_f64("delay_samples")?.unwrap_or(0.0) as i64;
        let polarity = fields.take_opt_f64("polarity")?.unwrap_or(1.0) as i32;
        let mut modal_notch = Vec::new();
        if let Some(JsonValue::Arr(items)) = fields.take_opt("modal_notch") {
            for item in items { modal_notch.push(ModalNotch::from_json(&item)?); }
        }
        fields.finish()?;
        Ok(SubConfig { id, output, hp_hz, lp_hz, gain_db, delay_samples, polarity, modal_notch })
    }

    fn validate(&self) -> Result<(), RestError> {
        if self.id.trim().is_empty() || self.id.len() > 24 {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].id must be 1..=24 characters"));
        }
        if self.output.trim().is_empty() || self.output.len() > 32 {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].output must be 1..=32 characters"));
        }
        if !(16.0..=60.0).contains(&self.hp_hz) || !(40.0..=150.0).contains(&self.lp_hz) || self.hp_hz >= self.lp_hz {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].hp_hz/lp_hz must satisfy 16<=hp<lp<=150"));
        }
        if !(-12.0..=6.0).contains(&self.gain_db) {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].gain_db must be -12..=6 dB"));
        }
        if !(0..=96000).contains(&self.delay_samples) {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].delay_samples must be 0..=96000"));
        }
        if self.polarity != 1 && self.polarity != -1 {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].polarity must be +1 or -1"));
        }
        if self.modal_notch.len() > 4 {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].modal_notch supports at most 4 notches"));
        }
        for notch in &self.modal_notch { notch.validate()?; }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("id", JsonValue::str(self.id.as_str())),
            ("output", JsonValue::str(self.output.as_str())),
            ("hp_hz", JsonValue::from(self.hp_hz)),
            ("lp_hz", JsonValue::from(self.lp_hz)),
            ("gain_db", JsonValue::from(self.gain_db)),
            ("delay_samples", JsonValue::from(self.delay_samples)),
            ("polarity", JsonValue::from(i64::from(self.polarity))),
            ("modal_notch", JsonValue::arr(self.modal_notch.iter().map(ModalNotch::to_json).collect())),
        ])
    }
}

impl ModalNotch {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs[].modal_notch entries must be objects")),
        };
        let mut fields = Fields::new(entries, "calibration settings bass.subs.modal_notch");
        let freq_hz = fields.take_opt_f64("freq_hz")?.ok_or_else(|| RestError::bad_request(CODE_MISSING_FIELD, "missing modal_notch.freq_hz"))?;
        let gain_db = fields.take_opt_f64("gain_db")?.unwrap_or(-6.0);
        let q = fields.take_opt_f64("q")?.unwrap_or(4.0);
        fields.finish()?;
        Ok(ModalNotch { freq_hz, gain_db, q })
    }

    fn validate(&self) -> Result<(), RestError> {
        if !(20.0..=150.0).contains(&self.freq_hz) || !(-24.0..=0.0).contains(&self.gain_db) || !(0.1..=20.0).contains(&self.q) {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "modal_notch must satisfy 20<=freq<=150, -24<=gain<=0, 0.1<=q<=20"));
        }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("freq_hz", JsonValue::from(self.freq_hz)),
            ("gain_db", JsonValue::from(self.gain_db)),
            ("q", JsonValue::from(self.q)),
        ])
    }
}

/// 线性相位低音炮分频 / bass management 设置（支持多炮 B 方案）。
#[derive(Debug, Clone, PartialEq)]
pub struct BassSettings {
    /// 是否启用 bass management。
    pub enabled: bool,
    /// 分频点 Hz（40..=150）。
    pub crossover_hz: f64,
    /// 主箱斜率（LR2/LR4/LR6/LR8/BW2/BW4/BW6/BW8）。
    pub main_slope: String,
    /// 低音炮斜率。
    pub sub_slope: String,
    /// 线性相位开关（true=线性相位 FIR，注意 pre-ringing）。
    pub linear_phase: bool,
    /// 单炮模式下的兼容增益（多炮时以 subs[].gain_db 为准）。
    pub sub_gain_db: f64,
    /// 多炮策略：single | multi_b。
    pub strategy: String,
    /// 低音炮列表（1..=4）。
    pub subs: Vec<SubConfig>,
    /// 低频路由矩阵（权重长度必须等于 subs.len()）。
    pub routing: SubRouting,
    /// 预测峰值 dB（页面/契约重算；host 未落地前不标 applied）。
    pub predicted_peak_db: Option<f64>,
    /// 全局 pre_gain dB（重算；防止数字削波）。
    pub pre_gain_db: Option<f64>,
}

impl Default for BassSettings {
    fn default() -> Self {
        BassSettings {
            enabled: false,
            crossover_hz: 80.0,
            main_slope: "LR4".to_string(),
            sub_slope: "LR4".to_string(),
            linear_phase: true,
            sub_gain_db: 0.0,
            strategy: "multi_b".to_string(),
            subs: vec![
                SubConfig { id: "SUB1".to_string(), output: "7".to_string(), hp_hz: 20.0, lp_hz: 120.0, gain_db: 0.0, delay_samples: 0, polarity: 1, modal_notch: Vec::new() },
                SubConfig { id: "SUB_L".to_string(), output: "8".to_string(), hp_hz: 20.0, lp_hz: 120.0, gain_db: 0.0, delay_samples: 0, polarity: 1, modal_notch: Vec::new() },
                SubConfig { id: "SUB_R".to_string(), output: "9".to_string(), hp_hz: 20.0, lp_hz: 120.0, gain_db: 0.0, delay_samples: 0, polarity: 1, modal_notch: Vec::new() },
            ],
            routing: SubRouting::default(),
            predicted_peak_db: Some(0.0),
            pre_gain_db: Some(0.0),
        }
    }
}

impl BassSettings {
    /// 重算预测峰值与全局 pre_gain（输入 LFE/L/R 按权重分给每只炮；取最大输入）。
    pub fn recompute_pre_gain(&mut self) {
        let input_rows = [&self.routing.lfe_weights, &self.routing.left_weights, &self.routing.right_weights];
        let mut peak = 0.0f64;
        for row in input_rows {
            let mut sum = 0.0f64;
            for (index, sub) in self.subs.iter().enumerate() {
                let weight = row.get(index).copied().unwrap_or(0.0).abs();
                sum += weight * 10f64.powf(sub.gain_db / 20.0);
            }
            if sum > 0.0 {
                peak = peak.max(20.0 * sum.log10());
            }
        }
        self.predicted_peak_db = Some(peak);
        self.pre_gain_db = Some(if peak > 0.0 { -peak.clamp(0.0, 20.0) } else { 0.0 });
    }
}

impl BassSettings {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "settings.bass must be a JSON object",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration settings bass");
        let enabled = fields.take_opt_bool("enabled")?.unwrap_or(false);
        let crossover_hz = fields.take_opt_f64("crossover_hz")?.unwrap_or(80.0);
        let main_slope = fields
            .take_opt_str("main_slope")?
            .unwrap_or_else(|| "LR4".to_string());
        let sub_slope = fields
            .take_opt_str("sub_slope")?
            .unwrap_or_else(|| "LR4".to_string());
        let linear_phase = fields.take_opt_bool("linear_phase")?.unwrap_or(true);
        let sub_gain_db = fields.take_opt_f64("sub_gain_db")?.unwrap_or(0.0);
        let strategy = fields.take_opt_str("strategy")?.unwrap_or_else(|| "multi_b".to_string());
        let mut subs = Vec::new();
        if let Some(JsonValue::Arr(items)) = fields.take_opt("subs") {
            for item in items { subs.push(SubConfig::from_json(&item)?); }
        }
        let routing = match fields.take_opt("routing") {
            None | Some(JsonValue::Null) => SubRouting::default(),
            Some(value) => {
                let entries = match value {
                    JsonValue::Obj(entries) => entries,
                    _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.routing must be an object")),
                };
                let mut fields = Fields::new(entries, "calibration settings bass.routing");
                let mode = fields.take_opt_str("mode")?.unwrap_or_else(|| "all_to_all".to_string());
                let lfe_weights = parse_weight_row(fields.take_opt("lfe_weights"))?;
                let left_weights = parse_weight_row(fields.take_opt("left_weights"))?;
                let right_weights = parse_weight_row(fields.take_opt("right_weights"))?;
                let stereo_bias = fields.take_opt_f64("stereo_bias")?.unwrap_or(0.0);
                fields.finish()?;
                SubRouting { mode, lfe_weights, left_weights, right_weights, stereo_bias }
            }
        };
        let predicted_peak_db = fields.take_opt_f64("predicted_peak_db")?;
        let pre_gain_db = fields.take_opt_f64("pre_gain_db")?;
        fields.finish()?;
        let mut settings = BassSettings {
            enabled,
            crossover_hz,
            main_slope,
            sub_slope,
            linear_phase,
            sub_gain_db,
            strategy,
            subs,
            routing,
            predicted_peak_db,
            pre_gain_db,
        };
        if settings.subs.is_empty() {
            settings.subs = BassSettings::default().subs;
        }
        if settings.routing.lfe_weights.is_empty() {
            settings.routing = BassSettings::default().routing;
        }
        settings.recompute_pre_gain();
        Ok(settings)
    }

    fn validate(&self) -> Result<(), RestError> {
        if !(CROSSOVER_HZ_MIN..=CROSSOVER_HZ_MAX).contains(&self.crossover_hz) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("bass.crossover_hz must be {CROSSOVER_HZ_MIN}..={CROSSOVER_HZ_MAX} Hz"),
            ));
        }
        for (field, slope) in [("main_slope", &self.main_slope), ("sub_slope", &self.sub_slope)] {
            if !SLOPES.contains(&slope.as_str()) {
                return Err(RestError::bad_request(
                    CODE_UNSUPPORTED_VALUE,
                    format!("bass.{field} must be one of: {}", SLOPES.join(",")),
                ));
            }
        }
        if !(-12.0..=6.0).contains(&self.sub_gain_db) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "bass.sub_gain_db must be -12..=6 dB",
            ));
        }
        if self.enabled {
            if !matches!(self.strategy.as_str(), "single" | "multi_b") {
                return Err(RestError::bad_request(CODE_UNSUPPORTED_VALUE, "bass.strategy must be single or multi_b"));
            }
            if self.subs.is_empty() || self.subs.len() > 4 {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.subs must have 1..=4 subwoofers"));
            }
            let mut ids = Vec::new();
            for sub in &self.subs {
                sub.validate()?;
                if ids.contains(&sub.id.as_str()) {
                    return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("duplicate sub id '{}'", sub.id)));
                }
                ids.push(sub.id.as_str());
            }
            if !matches!(self.routing.mode.as_str(), "all_to_all" | "stereo_bias") {
                return Err(RestError::bad_request(CODE_UNSUPPORTED_VALUE, "bass.routing.mode must be all_to_all or stereo_bias"));
            }
            if !(-1.0..=1.0).contains(&self.routing.stereo_bias) {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, "bass.routing.stereo_bias must be -1..=1"));
            }
            for (name, row) in [("lfe_weights", &self.routing.lfe_weights), ("left_weights", &self.routing.left_weights), ("right_weights", &self.routing.right_weights)] {
                if row.len() != self.subs.len() {
                    return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("bass.routing.{name} length must equal subs count")));
                }
                if row.iter().any(|value| !value.is_finite() || !(0.0..=1.5).contains(value)) {
                    return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("bass.routing.{name} values must be 0..=1.5")));
                }
                if row.iter().sum::<f64>() <= 0.0 {
                    return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("bass.routing.{name} must have positive sum")));
                }
            }
        }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("enabled", JsonValue::Bool(self.enabled)),
            ("crossover_hz", JsonValue::from(self.crossover_hz)),
            ("main_slope", JsonValue::str(self.main_slope.as_str())),
            ("sub_slope", JsonValue::str(self.sub_slope.as_str())),
            ("linear_phase", JsonValue::Bool(self.linear_phase)),
            ("sub_gain_db", JsonValue::from(self.sub_gain_db)),
            ("strategy", JsonValue::str(self.strategy.as_str())),
            ("subs", JsonValue::arr(self.subs.iter().map(SubConfig::to_json).collect())),
            ("subs_count", JsonValue::from(self.subs.len())),
            ("routing", JsonValue::obj(vec![
                ("mode", JsonValue::str(self.routing.mode.as_str())),
                ("lfe_weights", JsonValue::arr(self.routing.lfe_weights.iter().map(|v| JsonValue::from(*v)).collect())),
                ("left_weights", JsonValue::arr(self.routing.left_weights.iter().map(|v| JsonValue::from(*v)).collect())),
                ("right_weights", JsonValue::arr(self.routing.right_weights.iter().map(|v| JsonValue::from(*v)).collect())),
                ("stereo_bias", JsonValue::from(self.routing.stereo_bias)),
            ])),
            ("predicted_peak_db", opt_f64_json(self.predicted_peak_db)),
            ("pre_gain_db", opt_f64_json(self.pre_gain_db)),
        ])
    }
}

/// 每 lane 全频 FIR 校正设置。
#[derive(Debug, Clone, PartialEq)]
pub struct FirSettings {
    /// 是否启用。
    pub enabled: bool,
    /// flat | tilt10 | custom。
    pub target_curve: String,
    /// custom 目标曲线点（hz, dB；<= 64 点；target_curve=custom 时必填）。
    pub custom_curve: Vec<(f64, f64)>,
    /// taps（512..=24000）。
    pub taps: u32,
    /// mixed | linear。
    pub phase: String,
}

impl Default for FirSettings {
    fn default() -> Self {
        FirSettings {
            enabled: true,
            target_curve: "flat".to_string(),
            custom_curve: Vec::new(),
            taps: 4096,
            phase: "mixed".to_string(),
        }
    }
}

impl FirSettings {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "settings.fir must be a JSON object")),
        };
        let mut fields = Fields::new(entries, "calibration settings fir");
        let enabled = fields.take_opt_bool("enabled")?.unwrap_or(true);
        let target_curve = fields.take_opt_str("target_curve")?.unwrap_or_else(|| "flat".to_string());
        let mut custom_curve = Vec::new();
        if let Some(JsonValue::Arr(items)) = fields.take_opt("custom_curve") {
            for item in items {
                let entries = match item {
                    JsonValue::Obj(entries) => entries,
                    _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "fir.custom_curve entries must be objects")),
                };
                let mut point = Fields::new(entries, "calibration settings fir.custom_curve");
                let hz = point.take_opt_f64("hz")?.ok_or_else(|| RestError::bad_request(CODE_MISSING_FIELD, "missing custom_curve.hz"))?;
                let db = point.take_opt_f64("db")?.ok_or_else(|| RestError::bad_request(CODE_MISSING_FIELD, "missing custom_curve.db"))?;
                point.finish()?;
                custom_curve.push((hz, db));
            }
        }
        let taps = fields.take_opt_u32("taps")?.unwrap_or(4096);
        let phase = fields.take_opt_str("phase")?.unwrap_or_else(|| "mixed".to_string());
        fields.finish()?;
        Ok(FirSettings { enabled, target_curve, custom_curve, taps, phase })
    }

    fn validate(&self) -> Result<(), RestError> {
        if !FIR_TARGET_CURVES.contains(&self.target_curve.as_str()) {
            return Err(RestError::bad_request(CODE_UNSUPPORTED_VALUE, format!("fir.target_curve must be one of: {}", FIR_TARGET_CURVES.join(","))));
        }
        if self.target_curve == "custom" {
            if self.custom_curve.len() < 2 || self.custom_curve.len() > 64 {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, "fir.custom_curve requires 2..=64 points"));
            }
            if self.custom_curve.iter().any(|(hz, db)| !hz.is_finite() || !db.is_finite() || *hz < 20.0 || *hz > 20000.0) {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, "fir.custom_curve points must be finite 20..=20000 Hz"));
            }
        }
        if !(512..=FIR_TAPS_MAX).contains(&self.taps) {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("fir.taps must be 512..={FIR_TAPS_MAX}")));
        }
        if !FIR_PHASES.contains(&self.phase.as_str()) {
            return Err(RestError::bad_request(CODE_UNSUPPORTED_VALUE, format!("fir.phase must be one of: {}", FIR_PHASES.join(","))));
        }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("enabled", JsonValue::Bool(self.enabled)),
            ("target_curve", JsonValue::str(self.target_curve.as_str())),
            ("custom_curve", JsonValue::arr(self.custom_curve.iter().map(|(hz, db)| JsonValue::obj(vec![("hz", JsonValue::from(*hz)), ("db", JsonValue::from(*db))])).collect())),
            ("taps", JsonValue::from(self.taps)),
            ("phase", JsonValue::str(self.phase.as_str())),
        ])
    }
}

/// 每 lane modal EQ 设置。
#[derive(Debug, Clone, PartialEq)]
pub struct ModalEqSettings {
    /// 是否启用。
    pub enabled: bool,
    /// 自动检测模态。
    pub auto_detect: bool,
    /// 每 lane 最大 notch 数（1..=8）。
    pub max_notches: u32,
    /// 关闭 modal EQ 的 lane（结果表里的每 lane 开关）。
    pub disabled_lanes: Vec<String>,
}

impl Default for ModalEqSettings {
    fn default() -> Self {
        ModalEqSettings { enabled: true, auto_detect: true, max_notches: 4, disabled_lanes: Vec::new() }
    }
}

impl ModalEqSettings {
    fn from_json(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "settings.modal must be a JSON object")),
        };
        let mut fields = Fields::new(entries, "calibration settings modal");
        let enabled = fields.take_opt_bool("enabled")?.unwrap_or(true);
        let auto_detect = fields.take_opt_bool("auto_detect")?.unwrap_or(true);
        let max_notches = fields.take_opt_u32("max_notches")?.unwrap_or(4);
        let mut disabled_lanes = Vec::new();
        if let Some(JsonValue::Arr(items)) = fields.take_opt("disabled_lanes") {
            for item in items {
                match item {
                    JsonValue::Str(lane) => disabled_lanes.push(lane),
                    _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "modal.disabled_lanes entries must be strings")),
                }
            }
        }
        fields.finish()?;
        Ok(ModalEqSettings { enabled, auto_detect, max_notches, disabled_lanes })
    }

    fn validate(&self) -> Result<(), RestError> {
        if !(1..=8).contains(&self.max_notches) {
            return Err(RestError::bad_request(CODE_INVALID_FIELD, "modal.max_notches must be 1..=8"));
        }
        let mut seen = Vec::new();
        for lane in &self.disabled_lanes {
            if !CAL_LANES.contains(&lane.as_str()) {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("unknown modal disabled lane '{lane}'")));
            }
            if seen.contains(&lane.as_str()) {
                return Err(RestError::bad_request(CODE_INVALID_FIELD, format!("duplicate modal disabled lane '{lane}'")));
            }
            seen.push(lane.as_str());
        }
        Ok(())
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("enabled", JsonValue::Bool(self.enabled)),
            ("auto_detect", JsonValue::Bool(self.auto_detect)),
            ("max_notches", JsonValue::from(self.max_notches)),
            ("disabled_lanes", JsonValue::arr(self.disabled_lanes.iter().map(|lane| JsonValue::str(lane.as_str())).collect())),
        ])
    }
}

/// 校准设置（mic + 可选校准文件 + 朝向 + target mode + bass + FIR + modal）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationSettings {
    /// 麦克风。
    pub mic: MicSettings,
    /// 可选校准文件。
    pub cal_file: Option<CalFileRef>,
    /// 0 或 90 度。
    pub orientation_deg: u16,
    /// flat | tilt10。
    pub target_mode: String,
    /// bass management。
    pub bass: BassSettings,
    /// 每 lane 全频 FIR 校正。
    pub fir: FirSettings,
    /// 每 lane modal EQ。
    pub modal: ModalEqSettings,
}

impl Default for CalibrationSettings {
    fn default() -> Self {
        CalibrationSettings {
            mic: MicSettings::default(),
            cal_file: None,
            orientation_deg: 90,
            target_mode: "flat".to_string(),
            bass: BassSettings::default(),
            fir: FirSettings::default(),
            modal: ModalEqSettings::default(),
        }
    }
}

impl CalibrationSettings {
    /// 从严格 JSON 对象解析并校验（未知字段 400；类型/范围 400）。
    pub fn parse(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_JSON,
                    "calibration settings body must be a JSON object",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration settings");
        let mic = MicSettings::from_json(&fields.take_required("mic")?)?;
        let cal_file = match fields.take_opt("cal_file") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(CalFileRef::from_json(&value)?),
        };
        let orientation_deg = fields
            .take_opt_u16("orientation_deg")?
            .unwrap_or(90);
        let target_mode = fields
            .take_opt_str("target_mode")?
            .unwrap_or_else(|| "flat".to_string());
        let bass = match fields.take_opt("bass") {
            None | Some(JsonValue::Null) => BassSettings::default(),
            Some(value) => BassSettings::from_json(&value)?,
        };
        let fir = match fields.take_opt("fir") {
            None | Some(JsonValue::Null) => FirSettings::default(),
            Some(value) => FirSettings::from_json(&value)?,
        };
        let modal = match fields.take_opt("modal") {
            None | Some(JsonValue::Null) => ModalEqSettings::default(),
            Some(value) => ModalEqSettings::from_json(&value)?,
        };
        fields.finish()?;
        let settings = CalibrationSettings {
            mic,
            cal_file,
            orientation_deg,
            target_mode,
            bass,
            fir,
            modal,
        };
        settings.validate()?;
        Ok(settings)
    }

    /// 校验（字段合法性与互斥关系）。
    pub fn validate(&self) -> Result<(), RestError> {
        self.mic.validate()?;
        if !ORIENTATIONS_DEG.contains(&self.orientation_deg) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                "orientation_deg must be 0 or 90",
            ));
        }
        if !TARGET_MODES.contains(&self.target_mode.as_str()) {
            return Err(RestError::bad_request(
                CODE_UNSUPPORTED_VALUE,
                format!("target_mode must be one of: {}", TARGET_MODES.join(",")),
            ));
        }
        if let Some(cal_file) = &self.cal_file {
            cal_file.validate()?;
        }
        self.bass.validate()?;
        self.fir.validate()?;
        self.modal.validate()?;
        Ok(())
    }

    /// 绝对/相对口径：有带 SHA-256 的校准文件 = 可标绝对读数。
    pub fn calibration_state(&self) -> &'static str {
        match &self.cal_file {
            Some(cal_file) if cal_file.sha256.is_some() => "CALIBRATED_ABSOLUTE",
            _ => "RELATIVE_UNCALIBRATED",
        }
    }

    /// 校验结果（页面显示绝对/相对标签与警告）。
    pub fn validation(&self) -> CalSettingsValidation {
        let mut warnings = Vec::new();
        let mut absolute = false;
        match &self.cal_file {
            None => warnings.push(CalValidationMessage {
                code: "UNCALIBRATED",
                message: "未提供校准文件；电平读数按相对值处理（不标绝对 dB SPL）".to_string(),
            }),
            Some(cal_file) => {
                if cal_file.sha256.is_none() {
                    warnings.push(CalValidationMessage {
                        code: "CAL_FILE_SHA_MISSING",
                        message: "校准文件缺少 SHA-256；无法证明文件未变，按相对值处理".to_string(),
                    });
                } else {
                    absolute = true;
                    warnings.push(CalValidationMessage {
                        code: "CAL_FILE_SHA256",
                        message: format!(
                            "校准文件 SHA-256 已登记：{}",
                            short_sha(cal_file.sha256.as_deref().unwrap_or(""))
                        ),
                    });
                }
                if cal_file.source == "browser" && cal_file.path.is_none() {
                    warnings.push(CalValidationMessage {
                        code: "CAL_FILE_UPLOAD_PENDING",
                        message: "浏览器选择的校准文件尚未落到 host 可读路径（pending host field）；当前只登记 name+sha256".to_string(),
                    });
                }
            }
        }
        if self.mic.kind == "analog_48v" && !self.mic.phantom_48v {
            warnings.push(CalValidationMessage {
                code: "MIC_PHANTOM_OFF",
                message: "模拟输入未开 48V；电容麦（含 MA1）可能无声，请确认麦克风类型".to_string(),
            });
        }
        if self.bass.enabled && self.bass.linear_phase {
            warnings.push(CalValidationMessage {
                code: "BASS_LINEAR_PHASE_PRERINGING",
                message: "线性相位分频会引入 pre-ringing/群延迟；低频瞬态要求高时请试 LR4 最小相位或缩短 FIR".to_string(),
            });
        }
        if self.fir.enabled && self.fir.phase == "linear" {
            warnings.push(CalValidationMessage {
                code: "FIR_LINEAR_PHASE_PRERINGING",
                message: "全频线性相位 FIR 会增加群延迟与 pre-ringing；taps 越大越明显（当前 {}）".to_string(),
            });
        }
        if self.fir.enabled && self.fir.taps > 16384 {
            warnings.push(CalValidationMessage {
                code: "FIR_TAPS_CPU",
                message: "全频 FIR taps > 16384；注意分区卷积 CPU 与 RT 预算（host 字段未落地）".to_string(),
            });
        }
        CalSettingsValidation {
            ok: absolute,
            label: if absolute {
                format!(
                    "ABSOLUTE：{} 校准文件已加载（sha256 {}）",
                    self.cal_file
                        .as_ref()
                        .map(CalFileRef::display_name)
                        .unwrap_or_default(),
                    short_sha(
                        self.cal_file
                            .as_ref()
                            .and_then(|file| file.sha256.as_deref())
                            .unwrap_or("")
                    )
                )
            } else {
                "RELATIVE / UNCALIBRATED：未提供带 SHA-256 的校准文件，电平读数为相对值".to_string()
            },
            warnings,
        }
    }

    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("mic", self.mic.to_json()),
            (
                "cal_file",
                match &self.cal_file {
                    Some(cal_file) => cal_file.to_json(),
                    None => JsonValue::Null,
                },
            ),
            ("orientation_deg", JsonValue::from(self.orientation_deg)),
            ("target_mode", JsonValue::str(self.target_mode.as_str())),
            ("bass", self.bass.to_json()),
            ("fir", self.fir.to_json()),
            ("modal", self.modal.to_json()),
        ])
    }
}

fn short_sha(sha: &str) -> String {
    if sha.len() > 16 {
        format!("{}...", &sha[..16])
    } else if sha.is_empty() {
        "-".to_string()
    } else {
        sha.to_string()
    }
}

/// lane 位置人读名（与 4.1.2 布局一致）。
pub fn lane_position(lane: &str) -> &'static str {
    match lane {
        "FL" => "front_left",
        "FR" => "front_right",
        "SL" => "side_left",
        "SR" => "side_right",
        "HFL" => "height_front_left",
        "HFR" => "height_front_right",
        "SUB1" => "subwoofer",
        _ => "unknown",
    }
}

/// 单 lane 校准结果（level 与 delay 共用；未使用字段为 null）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalLaneResult {
    /// lane 名（CAL_LANES）。
    pub lane: String,
    /// pending | ramping | locked | failed。
    pub state: String,
    /// level：到达的目标读数 dBFS。
    pub achieved_dbfs: Option<f64>,
    /// level：该 lane 实测 SNR。
    pub snr_db: Option<f64>,
    /// level：锁存增益（相对基准的 trim）。
    pub locked_gain_db: Option<f64>,
    /// level：爬坡步数。
    pub ramp_steps: Option<u32>,
    /// delay：onset 样本。
    pub onset_samples: Option<i64>,
    /// delay：相对参考 lane 的样本数。
    pub relative_samples: Option<i64>,
    /// delay：要施加的 delay 样本数（>=0）。
    pub delay_samples: Option<i64>,
    /// delay：onset | lfe_xcorr_40_150。
    pub method: Option<String>,
    /// delay：互相关置信度 0..1。
    pub confidence: Option<f64>,
    /// 失败原因（无内部路径）。
    pub failure: Option<String>,
    /// fir：目标曲线。
    pub target_curve: Option<String>,
    /// fir：taps。
    pub taps: Option<u32>,
    /// fir：mixed | linear。
    pub phase: Option<String>,
    /// fir：期望幅频误差 dB。
    pub expected_mag_error_db: Option<f64>,
    /// fir/modal：群延迟 ms。
    pub group_delay_ms: Option<f64>,
    /// fir：预测峰值 dBFS。
    pub peak_dbfs: Option<f64>,
    /// modal：检测到的模态 notch。
    pub notches: Vec<ModalNotch>,
}

impl CalLaneResult {
    /// 构造一个已锁定的 level lane（测试/模拟 port 用）。
    pub fn level_locked(lane: &str, achieved_dbfs: f64, snr_db: f64, locked_gain_db: f64) -> Self {
        CalLaneResult {
            lane: lane.to_string(),
            state: "locked".to_string(),
            achieved_dbfs: Some(achieved_dbfs),
            snr_db: Some(snr_db),
            locked_gain_db: Some(locked_gain_db),
            ramp_steps: Some(12),
            onset_samples: None,
            relative_samples: None,
            delay_samples: None,
            method: Some("closed_loop_ramp".to_string()),
            confidence: None,
            failure: None,
            target_curve: None,
            taps: None,
            phase: None,
            expected_mag_error_db: None,
            group_delay_ms: None,
            peak_dbfs: None,
            notches: Vec::new(),
        }
    }

    /// 构造一个已锁定的 delay lane（测试/模拟 port 用）。
    pub fn delay_locked(lane: &str, onset_samples: i64, relative_samples: i64) -> Self {
        CalLaneResult {
            lane: lane.to_string(),
            state: "locked".to_string(),
            achieved_dbfs: None,
            snr_db: None,
            locked_gain_db: None,
            ramp_steps: None,
            onset_samples: Some(onset_samples),
            relative_samples: Some(relative_samples),
            delay_samples: Some(relative_samples.max(0)),
            method: Some(if lane == "SUB1" {
                "lfe_xcorr_40_150".to_string()
            } else {
                "double_gate".to_string()
            }),
            confidence: Some(0.98),
            failure: None,
            target_curve: None,
            taps: None,
            phase: None,
            expected_mag_error_db: None,
            group_delay_ms: None,
            peak_dbfs: None,
            notches: Vec::new(),
        }
    }

    /// 构造一个失败 lane。
    pub fn failed(lane: &str, failure: &str) -> Self {
        CalLaneResult {
            lane: lane.to_string(),
            state: "failed".to_string(),
            achieved_dbfs: None,
            snr_db: None,
            locked_gain_db: None,
            ramp_steps: None,
            onset_samples: None,
            relative_samples: None,
            delay_samples: None,
            method: None,
            confidence: None,
            failure: Some(failure.to_string()),
            target_curve: None,
            taps: None,
            phase: None,
            expected_mag_error_db: None,
            group_delay_ms: None,
            peak_dbfs: None,
            notches: Vec::new(),
        }
    }

    /// 构造一个已锁定的全频 FIR lane（测试/模拟 port 用）。
    pub fn fir_locked(
        lane: &str,
        target_curve: &str,
        taps: u32,
        phase: &str,
        expected_mag_error_db: f64,
        group_delay_ms: f64,
        peak_dbfs: f64,
    ) -> Self {
        CalLaneResult {
            lane: lane.to_string(),
            state: "locked".to_string(),
            achieved_dbfs: None,
            snr_db: None,
            locked_gain_db: None,
            ramp_steps: None,
            onset_samples: None,
            relative_samples: None,
            delay_samples: None,
            method: Some("fullband_fir".to_string()),
            confidence: None,
            failure: None,
            target_curve: Some(target_curve.to_string()),
            taps: Some(taps),
            phase: Some(phase.to_string()),
            expected_mag_error_db: Some(expected_mag_error_db),
            group_delay_ms: Some(group_delay_ms),
            peak_dbfs: Some(peak_dbfs),
            notches: Vec::new(),
        }
    }

    /// 构造一个已锁定的 modal EQ lane（测试/模拟 port 用）。
    pub fn modal_locked(lane: &str, notches: Vec<ModalNotch>) -> Self {
        CalLaneResult {
            lane: lane.to_string(),
            state: "locked".to_string(),
            achieved_dbfs: None,
            snr_db: None,
            locked_gain_db: None,
            ramp_steps: None,
            onset_samples: None,
            relative_samples: None,
            delay_samples: None,
            method: Some("modal_eq".to_string()),
            confidence: None,
            failure: None,
            target_curve: None,
            taps: None,
            phase: None,
            expected_mag_error_db: None,
            group_delay_ms: None,
            peak_dbfs: None,
            notches,
        }
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("lane", JsonValue::str(self.lane.as_str())),
            ("position", JsonValue::str(lane_position(&self.lane))),
            ("state", JsonValue::str(self.state.as_str())),
            ("achieved_dbfs", opt_f64_json(self.achieved_dbfs)),
            ("snr_db", opt_f64_json(self.snr_db)),
            ("locked_gain_db", opt_f64_json(self.locked_gain_db)),
            (
                "ramp_steps",
                match self.ramp_steps {
                    Some(steps) => JsonValue::from(steps),
                    None => JsonValue::Null,
                },
            ),
            (
                "onset_samples",
                match self.onset_samples {
                    Some(samples) => JsonValue::from(samples),
                    None => JsonValue::Null,
                },
            ),
            (
                "relative_samples",
                match self.relative_samples {
                    Some(samples) => JsonValue::from(samples),
                    None => JsonValue::Null,
                },
            ),
            (
                "delay_samples",
                match self.delay_samples {
                    Some(samples) => JsonValue::from(samples),
                    None => JsonValue::Null,
                },
            ),
            ("method", opt_string_json(&self.method)),
            ("confidence", opt_f64_json(self.confidence)),
            ("failure", opt_string_json(&self.failure)),
            ("target_curve", opt_string_json(&self.target_curve)),
            ("taps", match self.taps { Some(taps) => JsonValue::from(taps), None => JsonValue::Null }),
            ("phase", opt_string_json(&self.phase)),
            ("expected_mag_error_db", opt_f64_json(self.expected_mag_error_db)),
            ("group_delay_ms", opt_f64_json(self.group_delay_ms)),
            ("peak_dbfs", opt_f64_json(self.peak_dbfs)),
            ("notches", JsonValue::arr(self.notches.iter().map(ModalNotch::to_json).collect())),
        ])
    }
}

fn opt_f64_json(value: Option<f64>) -> JsonValue {
    match value {
        Some(number) if number.is_finite() => JsonValue::from(number),
        _ => JsonValue::Null,
    }
}

/// bass management 护栏检查项。
#[derive(Debug, Clone, PartialEq)]
pub struct CalGuardCheck {
    /// 稳定码（例如 E_CROSSOVER_ABOVE_FHI / E_GROUP_DELAY / E_PRERINGING）。
    pub code: String,
    /// 是否通过。
    pub passed: bool,
    /// 人读说明。
    pub detail: String,
}

/// 单炮结果（峰值/pre_gain/延迟/极性/notch）。
#[derive(Debug, Clone, PartialEq)]
pub struct SubResult {
    /// 炮 id。
    pub id: String,
    /// 预测/实测峰值 dBFS。
    pub peak_dbfs: f64,
    /// pre_gain dB。
    pub pre_gain_db: f64,
    /// 延迟样本。
    pub delay_samples: i64,
    /// 极性。
    pub polarity: i32,
    /// modal notch 数量。
    pub notch_count: usize,
    /// 护栏是否通过。
    pub guardrail_passed: bool,
}

impl SubResult {
    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("id", JsonValue::str(self.id.as_str())),
            ("peak_dbfs", JsonValue::from(self.peak_dbfs)),
            ("pre_gain_db", JsonValue::from(self.pre_gain_db)),
            ("delay_samples", JsonValue::from(self.delay_samples)),
            ("polarity", JsonValue::from(i64::from(self.polarity))),
            ("notch_count", JsonValue::from(self.notch_count)),
            ("guardrail_passed", JsonValue::Bool(self.guardrail_passed)),
        ])
    }
}

/// bass management 结果（频响摘要 / 群延迟 / 护栏 / 多炮）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalBassResults {
    /// 是否启用。
    pub enabled: bool,
    /// 分频点 Hz。
    pub crossover_hz: f64,
    /// 主箱斜率。
    pub main_slope: String,
    /// 低音炮斜率。
    pub sub_slope: String,
    /// 线性相位。
    pub linear_phase: bool,
    /// 低音炮增益 dB。
    pub sub_gain_db: f64,
    /// 护栏是否全部通过。
    pub guardrail_passed: bool,
    /// 护栏检查项。
    pub checks: Vec<CalGuardCheck>,
    /// 分频点附近群延迟 ms（可选）。
    pub group_delay_ms: Option<f64>,
    /// 线性相位 pre-ringing ms（可选）。
    pub pre_ringing_ms: Option<f64>,
    /// 频响摘要（hz, dB；<= 64 点）。
    pub freq_response: Vec<(f64, f64)>,
    /// 多炮结果（single 时一只）。
    pub subs: Vec<SubResult>,
    /// 全局 pre_gain dB。
    pub pre_gain_db: Option<f64>,
}

impl CalBassResults {
    /// 护栏全部通过的构造（模拟 port/测试用）。
    pub fn passed(settings: &BassSettings) -> Self {
        CalBassResults {
            enabled: settings.enabled,
            crossover_hz: settings.crossover_hz,
            main_slope: settings.main_slope.clone(),
            sub_slope: settings.sub_slope.clone(),
            linear_phase: settings.linear_phase,
            sub_gain_db: settings.sub_gain_db,
            guardrail_passed: true,
            checks: vec![
                CalGuardCheck {
                    code: "CROSSOVER_BELOW_FHI".to_string(),
                    passed: true,
                    detail: format!(
                        "crossover {:.0} Hz <= f_hi 150 Hz；LFE 40–150 Hz 互相关对齐可用",
                        settings.crossover_hz
                    ),
                },
                CalGuardCheck {
                    code: "LINEAR_PHASE_PRERINGING".to_string(),
                    passed: true,
                    detail: "pre-ringing 与群延迟在护栏内".to_string(),
                },
            ],
            group_delay_ms: Some(4.2),
            pre_ringing_ms: settings.linear_phase.then_some(2.1),
            freq_response: vec![
                (40.0, 0.2),
                (80.0, 0.0),
                (120.0, -0.3),
                (150.0, -0.6),
            ],
            subs: settings
                .subs
                .iter()
                .map(|sub| SubResult {
                    id: sub.id.clone(),
                    peak_dbfs: 0.0,
                    pre_gain_db: settings.pre_gain_db.unwrap_or(0.0),
                    delay_samples: sub.delay_samples,
                    polarity: sub.polarity,
                    notch_count: sub.modal_notch.len(),
                    guardrail_passed: true,
                })
                .collect(),
            pre_gain_db: settings.pre_gain_db,
        }
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("enabled", JsonValue::Bool(self.enabled)),
            ("crossover_hz", JsonValue::from(self.crossover_hz)),
            ("main_slope", JsonValue::str(self.main_slope.as_str())),
            ("sub_slope", JsonValue::str(self.sub_slope.as_str())),
            ("linear_phase", JsonValue::Bool(self.linear_phase)),
            ("sub_gain_db", JsonValue::from(self.sub_gain_db)),
            ("guardrail_passed", JsonValue::Bool(self.guardrail_passed)),
            (
                "checks",
                JsonValue::arr(
                    self.checks
                        .iter()
                        .map(|check| {
                            JsonValue::obj(vec![
                                ("code", JsonValue::str(check.code.as_str())),
                                ("passed", JsonValue::Bool(check.passed)),
                                ("detail", JsonValue::str(check.detail.as_str())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("group_delay_ms", opt_f64_json(self.group_delay_ms)),
            ("pre_ringing_ms", opt_f64_json(self.pre_ringing_ms)),
            (
                "freq_response",
                JsonValue::arr(
                    self.freq_response
                        .iter()
                        .map(|(hz, db)| {
                            JsonValue::obj(vec![
                                ("hz", JsonValue::from(*hz)),
                                ("db", JsonValue::from(*db)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("subs", JsonValue::arr(self.subs.iter().map(SubResult::to_json).collect())),
            ("pre_gain_db", opt_f64_json(self.pre_gain_db)),
        ])
    }
}

/// 一次 level/delay/bass job 的全部结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CalLaneResults {
    /// level | delay | bass。
    pub stage: String,
    /// 参考 lane（level 相对电平 / delay 相对延迟）。
    pub reference_lane: Option<String>,
    /// level 目标读数 dBFS。
    pub target_dbfs: Option<f64>,
    /// 每 lane 结果。
    pub lanes: Vec<CalLaneResult>,
    /// bass management 结果（stage=bass）。
    pub bass: Option<CalBassResults>,
}

impl CalLaneResults {
    /// 已锁定 lane 数。
    pub fn locked_count(&self) -> usize {
        self.lanes
            .iter()
            .filter(|lane| lane.state == "locked")
            .count()
    }

    /// 是否全部锁定。
    pub fn all_locked(&self) -> bool {
        !self.lanes.is_empty()
            && self.lanes.iter().all(|lane| lane.state == "locked")
    }

    /// 失败 lane 数。
    pub fn failed_count(&self) -> usize {
        self.lanes
            .iter()
            .filter(|lane| lane.state == "failed")
            .count()
    }

    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            ("schema", JsonValue::str(CAL_LANES_SCHEMA)),
            ("stage", JsonValue::str(self.stage.as_str())),
            ("reference_lane", opt_string_json(&self.reference_lane)),
            ("target_dbfs", opt_f64_json(self.target_dbfs)),
            ("locked", JsonValue::from(self.locked_count())),
            ("total", JsonValue::from(self.lanes.len())),
            (
                "lanes",
                JsonValue::arr(self.lanes.iter().map(CalLaneResult::to_json).collect()),
            ),
            (
                "bass",
                match &self.bass {
                    Some(bass) => bass.to_json(),
                    None => JsonValue::Null,
                },
            ),
        ])
    }
}

/// 人读进度文案（页面 progress_text；不替代机器可读 state/stage）。
pub fn progress_text(
    state: &str,
    stage: Option<&str>,
    progress: f64,
    lanes: Option<&CalLaneResults>,
) -> String {
    let pct = (progress.clamp(0.0, 1.0) * 100.0).round() as u64;
    let label = match stage {
        Some(STAGE_LEVEL) => Some("Level 对齐"),
        Some(STAGE_DELAY) => Some("Delay 对齐"),
        Some(STAGE_BASS) => Some("Bass 管理"),
        Some(STAGE_FIR) => Some("全频 FIR"),
        Some(STAGE_MODAL) => Some("Modal EQ"),
        _ => None,
    };
    if stage == Some(STAGE_BASS) {
        if let Some(bass) = lanes.and_then(|lanes| lanes.bass.as_ref()) {
            if state == "complete" && bass.guardrail_passed {
                return format!(
                    "Bass 管理完成：{:.0} Hz {} / {}，线性相位={}，护栏通过，群延迟 {} ms",
                    bass.crossover_hz,
                    bass.main_slope,
                    bass.sub_slope,
                    if bass.linear_phase { "是" } else { "否" },
                    bass.group_delay_ms
                        .map(|value| format!("{value:.1}"))
                        .unwrap_or_else(|| "-".to_string())
                );
            }
            let failed = bass
                .checks
                .iter()
                .find(|check| !check.passed)
                .map(|check| format!("{} {}", check.code, check.detail))
                .unwrap_or_else(|| "护栏未通过".to_string());
            return format!("Bass 管理：护栏未通过（{failed}，{pct}%）");
        }
        return format!("Bass 管理：计算中（{pct}%）");
    }
    if let (Some(label), Some(lanes)) = (label, lanes) {
        if lanes.failed_count() > 0 {
            let first = lanes
                .lanes
                .iter()
                .find(|lane| lane.state == "failed")
                .map(|lane| {
                    format!(
                        "{}（{}）",
                        lane.lane,
                        lane.failure.as_deref().unwrap_or("failure")
                    )
                })
                .unwrap_or_else(|| "-".to_string());
            return format!(
                "{label}：{} 个 lane 失败，首个 {first}；共 {}/{} 已锁定",
                lanes.failed_count(),
                lanes.locked_count(),
                lanes.lanes.len()
            );
        }
        if state == "complete" && lanes.all_locked() {
            return format!(
                "{label}完成：{}/{} lane 已锁定{}",
                lanes.locked_count(),
                lanes.lanes.len(),
                if stage == Some(STAGE_LEVEL) {
                    "，可应用电平 trim"
                } else {
                    "，可应用延迟"
                }
            );
        }
        let current = lanes
            .lanes
            .iter()
            .find(|lane| lane.state == "ramping" || lane.state == "pending")
            .map(|lane| lane.lane.as_str())
            .unwrap_or("-");
        if lanes.locked_count() > 0 {
            return format!(
                "{label}：{}/{} lane 已锁定（{pct}%），当前 {current}",
                lanes.locked_count(),
                lanes.lanes.len()
            );
        }
        return format!("{label}：预热/爬坡中（{pct}%），当前 {current}");
    }
    let state_text = match state {
        "queued" => "排队中",
        "preflight" => "预检",
        "sweep_capture" => "扫频采集中",
        "quality_check" => "质量门检查",
        "fit_p" => "拟合传递函数",
        "solve_g" => "求解 G",
        "compile" => "编译 plan",
        "prepared" => "待应用",
        "apply" => "应用中",
        "remeasure" => "复测",
        "independent_holdout" => "独立 holdout",
        "complete" => "完成",
        "failed" => "失败",
        "cancelled" => "已取消",
        "rolled_back" => "已回滚",
        _ => "未知状态",
    };
    match state {
        "complete" | "failed" | "cancelled" | "rolled_back" => state_text.to_string(),
        _ => format!(
            "{}{}（{pct}%）",
            label.map(|label| format!("{label}：")).unwrap_or_default(),
            state_text
        ),
    }
}

/// level trim（apply 用）。
#[derive(Debug, Clone, PartialEq)]
pub struct LevelTrim {
    /// lane 名。
    pub lane: String,
    /// 锁存增益 dB（相对基准）。
    pub gain_db: f64,
}

/// delay（apply 用）。
#[derive(Debug, Clone, PartialEq)]
pub struct DelayTrim {
    /// lane 名。
    pub lane: String,
    /// 要施加的延迟样本数（>=0）。
    pub delay_samples: i64,
}

/// apply 请求（level trims / delays）。
#[derive(Debug, Clone, PartialEq)]
pub struct CalApplyRequest {
    /// ["level","delay"] 子集。
    pub targets: Vec<String>,
    /// 电平 trim。
    pub level_trims: Vec<LevelTrim>,
    /// 延迟。
    pub delays: Vec<DelayTrim>,
    /// 来源 job（可选）。
    pub job_id: Option<String>,
    /// bass 配置（apply target=bass 时携带；缺省用已保存设置）。
    pub bass: Option<BassSettings>,
    /// FIR 配置（apply target=fir）。
    pub fir: Option<FirSettings>,
    /// modal EQ 配置（apply target=modal）。
    pub modal: Option<ModalEqSettings>,
}

impl CalApplyRequest {
    /// 从 JSON 解析（apply body；未知字段 400）。
    pub fn parse(value: &JsonValue) -> Result<Self, RestError> {
        let entries = match value {
            JsonValue::Obj(entries) => entries.clone(),
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_JSON,
                    "calibration apply body must be a JSON object",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration apply");
        let targets = match fields.take_opt("targets") {
            None | Some(JsonValue::Null) => APPLY_TARGETS
                .iter()
                .map(|target| target.to_string())
                .collect(),
            Some(JsonValue::Arr(items)) => {
                let mut targets = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        JsonValue::Str(target) => {
                            if !APPLY_TARGETS.contains(&target.as_str()) {
                                return Err(RestError::bad_request(
                                    CODE_UNSUPPORTED_VALUE,
                                    format!(
                                        "apply target must be one of: {}",
                                        APPLY_TARGETS.join(",")
                                    ),
                                ));
                            }
                            targets.push(target);
                        }
                        _ => {
                            return Err(RestError::bad_request(
                                CODE_INVALID_FIELD,
                                "targets entries must be strings",
                            ))
                        }
                    }
                }
                targets
            }
            Some(_) => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "targets must be an array of strings",
                ))
            }
        };
        let level_trims = parse_level_trims(fields.take_opt("level_trims"))?;
        let delays = parse_delays(fields.take_opt("delays"))?;
        let job_id = fields.take_opt_str("job_id")?;
        let bass = match fields.take_opt("bass") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(BassSettings::from_json(&value)?),
        };
        let fir = match fields.take_opt("fir") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(FirSettings::from_json(&value)?),
        };
        let modal = match fields.take_opt("modal") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(ModalEqSettings::from_json(&value)?),
        };
        fields.finish()?;
        Ok(CalApplyRequest {
            targets,
            level_trims,
            delays,
            job_id,
            bass,
            fir,
            modal,
        })
    }

    /// 是否包含 target。
    pub fn includes(&self, target: &str) -> bool {
        self.targets.iter().any(|item| item == target)
    }
}

fn parse_weight_row(value: Option<JsonValue>) -> Result<Vec<f64>, RestError> {
    match value {
        None | Some(JsonValue::Null) => Ok(Vec::new()),
        Some(JsonValue::Arr(items)) => {
            let mut weights = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    JsonValue::Num(number) if number.is_finite() => weights.push(number),
                    _ => return Err(RestError::bad_request(CODE_INVALID_FIELD, "routing weights must be finite numbers")),
                }
            }
            Ok(weights)
        }
        Some(_) => Err(RestError::bad_request(CODE_INVALID_FIELD, "routing weights must be arrays")),
    }
}

fn parse_level_trims(value: Option<JsonValue>) -> Result<Vec<LevelTrim>, RestError> {
    let items = match value {
        None | Some(JsonValue::Null) => return Ok(Vec::new()),
        Some(JsonValue::Arr(items)) => items,
        Some(_) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "level_trims must be an array",
            ))
        }
    };
    let mut trims = Vec::with_capacity(items.len());
    for item in items {
        let entries = match item {
            JsonValue::Obj(entries) => entries,
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "level_trims entries must be objects",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration apply level_trims");
        let lane = fields.take_str("lane")?;
        let gain_db = fields
            .take_opt_f64("gain_db")?
            .ok_or_else(|| RestError::bad_request(CODE_MISSING_FIELD, "missing gain_db"))?;
        fields.finish()?;
        if !CAL_LANES.contains(&lane.as_str()) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("unknown lane '{lane}'"),
            ));
        }
        if !(-60.0..=12.0).contains(&gain_db) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "level trim gain_db must be -60..=12 dB",
            ));
        }
        trims.push(LevelTrim { lane, gain_db });
    }
    Ok(trims)
}

fn parse_delays(value: Option<JsonValue>) -> Result<Vec<DelayTrim>, RestError> {
    let items = match value {
        None | Some(JsonValue::Null) => return Ok(Vec::new()),
        Some(JsonValue::Arr(items)) => items,
        Some(_) => {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "delays must be an array",
            ))
        }
    };
    let mut delays = Vec::with_capacity(items.len());
    for item in items {
        let entries = match item {
            JsonValue::Obj(entries) => entries,
            _ => {
                return Err(RestError::bad_request(
                    CODE_INVALID_FIELD,
                    "delays entries must be objects",
                ))
            }
        };
        let mut fields = Fields::new(entries, "calibration apply delays");
        let lane = fields.take_str("lane")?;
        let delay_samples = fields
            .take_opt_f64("delay_samples")?
            .ok_or_else(|| RestError::bad_request(CODE_MISSING_FIELD, "missing delay_samples"))?;
        fields.finish()?;
        if !CAL_LANES.contains(&lane.as_str()) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                format!("unknown lane '{lane}'"),
            ));
        }
        if delay_samples.fract() != 0.0 || !(0.0..=96000.0).contains(&delay_samples) {
            return Err(RestError::bad_request(
                CODE_INVALID_FIELD,
                "delay_samples must be an integer 0..=96000",
            ));
        }
        delays.push(DelayTrim {
            lane,
            delay_samples: delay_samples as i64,
        });
    }
    Ok(delays)
}

/// 已应用/已清除的校准状态（daemon 内存态；host 未落地时不标 applied_to_audio）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalAppliedState {
    /// 已应用电平 trim。
    pub level_trims: Vec<LevelTrim>,
    /// 已应用延迟。
    pub delays: Vec<DelayTrim>,
    /// 应用时间（unix 秒；0=未应用）。
    pub applied_unix: u64,
    /// 清除时间（unix 秒；0=未清除）。
    pub cleared_unix: u64,
    /// 来源 job id。
    pub source_job: Option<String>,
    /// 已应用的 bass management 配置。
    pub bass: Option<BassSettings>,
    /// 已应用的 FIR 配置。
    pub fir: Option<FirSettings>,
    /// 已应用的 modal EQ 配置。
    pub modal: Option<ModalEqSettings>,
}

impl CalAppliedState {
    /// JSON 投影。
    pub fn to_json(&self) -> JsonValue {
        JsonValue::obj(vec![
            (
                "level_trims",
                JsonValue::arr(
                    self.level_trims
                        .iter()
                        .map(|trim| {
                            JsonValue::obj(vec![
                                ("lane", JsonValue::str(trim.lane.as_str())),
                                ("gain_db", JsonValue::from(trim.gain_db)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "delays",
                JsonValue::arr(
                    self.delays
                        .iter()
                        .map(|delay| {
                            JsonValue::obj(vec![
                                ("lane", JsonValue::str(delay.lane.as_str())),
                                ("delay_samples", JsonValue::from(delay.delay_samples)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("applied_unix", JsonValue::from(self.applied_unix)),
            ("cleared_unix", JsonValue::from(self.cleared_unix)),
            ("source_job", opt_string_json(&self.source_job)),
            (
                "bass",
                match &self.bass {
                    Some(bass) => bass.to_json(),
                    None => JsonValue::Null,
                },
            ),
            (
                "fir",
                match &self.fir {
                    Some(fir) => fir.to_json(),
                    None => JsonValue::Null,
                },
            ),
            (
                "modal",
                match &self.modal {
                    Some(modal) => modal.to_json(),
                    None => JsonValue::Null,
                },
            ),
        ])
    }

    /// 是否为空（无应用内容）。
    pub fn is_empty(&self) -> bool {
        self.level_trims.is_empty()
            && self.delays.is_empty()
            && self.bass.is_none()
            && self.fir.is_none()
            && self.modal.is_none()
    }
}

/// 校准设置响应体（GET/POST /api/calibration/settings 共用）。
pub fn settings_body(settings: &CalibrationSettings, info: &CalEngineInfo) -> JsonValue {
    let validation = settings.validation();
    let mut fields = vec![
        ("schema", JsonValue::str(CAL_SETTINGS_SCHEMA)),
        ("settings", settings.to_json()),
        (
            "calibration_state",
            JsonValue::str(settings.calibration_state()),
        ),
        ("validation", validation.to_json()),
        (
            "absolute_level",
            JsonValue::Bool(settings.calibration_state() == "CALIBRATED_ABSOLUTE"),
        ),
        ("pending_host_fields", pending_host_fields_json()),
        ("engine", JsonValue::str(info.engine)),
        ("synthetic", JsonValue::Bool(info.synthetic)),
        ("cal_engine", info.to_json()),
    ];
    if settings.cal_file.is_some() {
        fields.push(("cal_file_configured", JsonValue::Bool(true)));
    }
    JsonValue::obj(fields)
}

/// host socket 请求 JSON（唯一构造点；level/delay 使用扩展字段）。
#[allow(clippy::too_many_arguments)]
pub fn host_stage_request(
    op: &str,
    audible: bool,
    f_hi: f64,
    target: &str,
    supports: &[String],
    settings: Option<&CalibrationSettings>,
    lanes: &[String],
    reference_lane: Option<&str>,
    target_dbfs: Option<f64>,
    job_id: Option<&str>,
) -> String {
    let mut fields = vec![
        ("op", JsonValue::str(op)),
        ("audible", JsonValue::Bool(audible)),
        ("f_hi", JsonValue::from(f_hi)),
        ("target", JsonValue::str(target)),
        (
            "supports",
            JsonValue::arr(
                supports
                    .iter()
                    .map(|name| JsonValue::str(name.as_str()))
                    .collect(),
            ),
        ),
    ];
    if let Some(settings) = settings {
        fields.push(("settings", settings.to_json()));
        fields.push((
            "mic",
            JsonValue::obj(vec![
                ("kind", JsonValue::str(settings.mic.kind.as_str())),
                ("device", JsonValue::str(settings.mic.device.as_str())),
                ("channel", JsonValue::from(settings.mic.channel)),
                ("phantom_48v", JsonValue::Bool(settings.mic.phantom_48v)),
                ("gain_db", JsonValue::from(settings.mic.gain_db)),
            ]),
        ));
        fields.push(("orientation_deg", JsonValue::from(settings.orientation_deg)));
        fields.push(("target_mode", JsonValue::str(settings.target_mode.as_str())));
        fields.push((
            "cal_file",
            match &settings.cal_file {
                Some(cal_file) => cal_file.to_json(),
                None => JsonValue::Null,
            },
        ));
        fields.push((
            "calibration_state",
            JsonValue::str(settings.calibration_state()),
        ));
    }
    if !lanes.is_empty() {
        fields.push((
            "lanes",
            JsonValue::arr(lanes.iter().map(|lane| JsonValue::str(lane.as_str())).collect()),
        ));
    }
    if let Some(reference_lane) = reference_lane {
        fields.push(("reference_lane", JsonValue::str(reference_lane)));
    }
    if let Some(target_dbfs) = target_dbfs {
        fields.push(("target_dbfs", JsonValue::from(target_dbfs)));
    }
    if let Some(job_id) = job_id {
        fields.push(("job_id", JsonValue::str(job_id)));
    }
    JsonValue::obj(fields).to_json()
}
