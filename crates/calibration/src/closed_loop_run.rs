//! 闭环复测运行包：把「方案 + 基线录音 + 施加方案后的录音」一次做成可审计的证据包。
//!
//! 产物（全部确定性、键序固定、末尾换行）：
//!
//! - `predicted.json`：由方案推出的预测带域电平（crate::plan_levels）；
//! - `observed.json`：两段录音相除得到的实测相对电平（crate::band_levels）；
//! - `report.json`：三态判定报告（crate::closed_loop）；
//! - `manifest.json`：输入文件 sha256、输出文件 sha256、判定与配置（本模块）。
//!
//! 本模块只做纯计算，不读写文件、不碰设备：文件落盘与退出码在命令行工具里，
//! 真机采集仍是 T42 设备侧流程（NOT_RUN）。

use khaen_core::plan::Plan;

use crate::band_levels::{
    relative_band_levels, BandLevelsConfig, DEFAULT_WINDOW_LEN, MAX_WINDOW_LEN, MIN_WINDOW_LEN,
};
use crate::closed_loop::{
    compare, levels_bytes, levels_sha256, report_bytes, report_sha256, ClosedLoopConfig,
    ClosedLoopReport, LevelSet, PointLevels, Verdict, DEFAULT_MIN_COMPARED_BANDS,
    DEFAULT_TOLERANCE_DB,
};
use crate::plan_levels::{predicted_levels, DEFAULT_CENTERS_HZ};

/// 运行包清单 schema。
pub const MANIFEST_SCHEMA: &str = "khaen-closed-loop-run-manifest/v1";
/// 工具名（诊断/交接用）。
pub const TOOL_NAME: &str = "khaen-closed-loop-run";
/// 预测电平文件名。
pub const PREDICTED_FILE: &str = "predicted.json";
/// 实测电平文件名。
pub const OBSERVED_FILE: &str = "observed.json";
/// 判定报告文件名。
pub const REPORT_FILE: &str = "report.json";
/// 运行包清单文件名。
pub const MANIFEST_FILE: &str = "manifest.json";

/// 输入错误：录音样本非法或录音长度不足。
pub const INPUT: &str = "E_CLOSED_LOOP_RUN_INPUT";
/// 对齐错误：方案采样率与录音不一致、测点/频带对不上。
pub const ALIGNMENT: &str = "E_CLOSED_LOOP_RUN_ALIGNMENT";
/// 配置错误：测点名/容差/最少带数/窗长非法。
pub const CONFIG: &str = "E_CLOSED_LOOP_RUN_CONFIG";
/// 方案错误：方案无法推出预测电平（采样率不支持、非恒等段、保护段不完整等）。
pub const PLAN: &str = "E_CLOSED_LOOP_RUN_PLAN";

/// 运行包错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedLoopRunError {
    code: &'static str,
    message: String,
}

impl ClosedLoopRunError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ClosedLoopRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ClosedLoopRunError {}

/// 运行包配置。
#[derive(Clone, Debug, PartialEq)]
pub struct RunConfig {
    /// 测点名（写入 observed.json 与清单；只允许 ASCII 字母数字与 `-_.`，最长 64）。
    pub point: String,
    /// 本次测量对应的方案 lane（预测侧取该 lane 的预测电平；默认 0）。
    pub lane: u16,
    /// 1/3 oct 中心频率（升序，Hz）。
    pub centers_hz: Vec<f64>,
    /// 判定容差（dB，> 0）。
    pub tolerance_db: f64,
    /// 最少可比较带数（>= 1）。
    pub min_compared_bands: usize,
    /// 分析窗长（样本，2 的幂，256..=65536）。
    pub window_len: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            point: "P01".to_string(),
            lane: 0,
            centers_hz: DEFAULT_CENTERS_HZ.to_vec(),
            tolerance_db: DEFAULT_TOLERANCE_DB,
            min_compared_bands: DEFAULT_MIN_COMPARED_BANDS,
            window_len: DEFAULT_WINDOW_LEN,
        }
    }
}

impl RunConfig {
    fn validate(&self) -> Result<(), ClosedLoopRunError> {
        if self.point.is_empty() || self.point.len() > 64 {
            return Err(ClosedLoopRunError::new(
                CONFIG,
                "point name must be 1..=64 characters",
            ));
        }
        if !self.point.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        }) {
            return Err(ClosedLoopRunError::new(
                CONFIG,
                "point name may only contain ASCII letters, digits, '-', '_' and '.'",
            ));
        }
        if !self.tolerance_db.is_finite() || self.tolerance_db <= 0.0 {
            return Err(ClosedLoopRunError::new(
                CONFIG,
                "tolerance_db must be a finite value > 0",
            ));
        }
        if self.min_compared_bands == 0 {
            return Err(ClosedLoopRunError::new(
                CONFIG,
                "min_compared_bands must be >= 1",
            ));
        }
        if !self.window_len.is_power_of_two()
            || !(MIN_WINDOW_LEN..=MAX_WINDOW_LEN).contains(&self.window_len)
        {
            return Err(ClosedLoopRunError::new(
                CONFIG,
                format!(
                    "window_len {} must be a power of two in [{MIN_WINDOW_LEN}, {MAX_WINDOW_LEN}]",
                    self.window_len
                ),
            ));
        }
        Ok(())
    }
}

/// 运行包输入（方案 + 两段录音 + 它们的 sha256）。
pub struct RunInputs<'a> {
    /// 已编译方案。
    pub plan: &'a Plan,
    /// 方案文件 sha256（64 位十六进制）。
    pub plan_sha256: &'a str,
    /// 基线录音样本（单声道，采样率见 `sample_rate_hz`）。
    pub baseline_samples: &'a [f64],
    /// 基线录音文件 sha256（64 位十六进制）。
    pub baseline_wav_sha256: &'a str,
    /// 施加方案后录音样本（单声道，采样率与基线一致）。
    pub applied_samples: &'a [f64],
    /// 施加方案后录音文件 sha256（64 位十六进制）。
    pub applied_wav_sha256: &'a str,
    /// 两段录音的采样率（Hz）。
    pub sample_rate_hz: f64,
}

/// 运行包产物（纯数据）。
#[derive(Clone, Debug)]
pub struct RunBundle {
    predicted_bytes: Vec<u8>,
    observed_bytes: Vec<u8>,
    report_bytes: Vec<u8>,
    manifest_bytes: Vec<u8>,
    report: ClosedLoopReport,
    predicted_sha256: String,
    observed_sha256: String,
    report_sha256: String,
}

impl RunBundle {
    /// 判定报告。
    pub fn report(&self) -> &ClosedLoopReport {
        &self.report
    }

    /// 三态结论。
    pub fn verdict(&self) -> Verdict {
        self.report.verdict
    }

    /// 预测电平 JSON 字节。
    pub fn predicted_bytes(&self) -> &[u8] {
        &self.predicted_bytes
    }

    /// 实测电平 JSON 字节。
    pub fn observed_bytes(&self) -> &[u8] {
        &self.observed_bytes
    }

    /// 判定报告 JSON 字节。
    pub fn report_bytes(&self) -> &[u8] {
        &self.report_bytes
    }

    /// 清单 JSON 字节。
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    /// 预测电平 sha256。
    pub fn predicted_sha256(&self) -> &str {
        &self.predicted_sha256
    }

    /// 实测电平 sha256。
    pub fn observed_sha256(&self) -> &str {
        &self.observed_sha256
    }

    /// 判定报告 sha256。
    pub fn report_sha256(&self) -> &str {
        &self.report_sha256
    }

    /// 清单 sha256。
    pub fn manifest_sha256(&self) -> String {
        khaen_core::sha256::sha256_hex(&self.manifest_bytes)
    }

    /// 全部产物文件（固定顺序：预测、实测、报告、清单）。
    pub fn files(&self) -> [(&'static str, &[u8]); 4] {
        [
            (PREDICTED_FILE, &self.predicted_bytes),
            (OBSERVED_FILE, &self.observed_bytes),
            (REPORT_FILE, &self.report_bytes),
            (MANIFEST_FILE, &self.manifest_bytes),
        ]
    }
}

/// 构建运行包：方案 -> 预测电平；两段录音 -> 实测电平；两者 -> 判定报告与清单。
pub fn build_run_bundle(
    inputs: &RunInputs<'_>,
    config: &RunConfig,
) -> Result<RunBundle, ClosedLoopRunError> {
    config.validate()?;
    if !inputs.sample_rate_hz.is_finite() || inputs.sample_rate_hz <= 0.0 {
        return Err(ClosedLoopRunError::new(
            INPUT,
            format!(
                "sample rate {} must be a finite value > 0",
                inputs.sample_rate_hz
            ),
        ));
    }
    let plan_rate = f64::from(inputs.plan.sample_rate);
    if (plan_rate - inputs.sample_rate_hz).abs() > 1e-9 {
        return Err(ClosedLoopRunError::new(
            ALIGNMENT,
            format!(
                "plan sample rate {plan_rate} Hz does not match recordings {} Hz",
                inputs.sample_rate_hz
            ),
        ));
    }
    validate_sha256("plan_sha256", inputs.plan_sha256)?;
    validate_sha256("baseline_wav_sha256", inputs.baseline_wav_sha256)?;
    validate_sha256("applied_wav_sha256", inputs.applied_wav_sha256)?;

    let predicted_all = predicted_levels(inputs.plan, &config.centers_hz).map_err(|err| {
        ClosedLoopRunError::new(PLAN, format!("{}: {}", err.code(), err.message()))
    })?;
    let lane = usize::from(config.lane);
    let lane_levels = predicted_all.points.get(lane).ok_or_else(|| {
        ClosedLoopRunError::new(
            PLAN,
            format!(
                "lane {} selected but plan only has {} lanes",
                config.lane,
                predicted_all.points.len()
            ),
        )
    })?;
    let predicted = LevelSet {
        sample_rate_hz: predicted_all.sample_rate_hz,
        points: vec![PointLevels {
            name: config.point.clone(),
            bands: lane_levels.bands.clone(),
        }],
    };
    let observed_bands = relative_band_levels(
        inputs.applied_samples,
        inputs.baseline_samples,
        inputs.sample_rate_hz,
        &config.centers_hz,
        &BandLevelsConfig {
            window_len: config.window_len,
        },
    )
    .map_err(|err| {
        let code = if err.code() == crate::band_levels::ALIGNMENT {
            ALIGNMENT
        } else {
            INPUT
        };
        ClosedLoopRunError::new(code, format!("{}: {}", err.code(), err.message()))
    })?;
    let observed = LevelSet {
        sample_rate_hz: inputs.sample_rate_hz,
        points: vec![PointLevels {
            name: config.point.clone(),
            bands: observed_bands,
        }],
    };
    let closed_loop_config = ClosedLoopConfig {
        tolerance_db: config.tolerance_db,
        min_compared_bands: config.min_compared_bands,
    };
    let report = compare(&predicted, &observed, &closed_loop_config).map_err(|err| {
        let code = if err.code() == crate::closed_loop::error::ALIGNMENT {
            ALIGNMENT
        } else {
            INPUT
        };
        ClosedLoopRunError::new(code, format!("{}: {}", err.code(), err.message()))
    })?;

    let predicted_bytes = levels_bytes(&predicted);
    let observed_bytes = levels_bytes(&observed);
    let report_bytes = report_bytes(&report);
    let predicted_sha256 = levels_sha256(&predicted);
    let observed_sha256 = levels_sha256(&observed);
    let report_sha256 = report_sha256(&report);
    let manifest_bytes = manifest_bytes(
        config,
        inputs,
        &report,
        &predicted_sha256,
        &observed_sha256,
        &report_sha256,
    );
    Ok(RunBundle {
        predicted_bytes,
        observed_bytes,
        report_bytes,
        manifest_bytes,
        report,
        predicted_sha256,
        observed_sha256,
        report_sha256,
    })
}

fn validate_sha256(name: &str, value: &str) -> Result<(), ClosedLoopRunError> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(ClosedLoopRunError::new(
            INPUT,
            format!("{name} must be a 64-character hex string"),
        ));
    }
    Ok(())
}

fn manifest_bytes(
    config: &RunConfig,
    inputs: &RunInputs<'_>,
    report: &ClosedLoopReport,
    predicted_sha256: &str,
    observed_sha256: &str,
    report_sha256: &str,
) -> Vec<u8> {
    let mut out = String::from("{");
    push_str_field(&mut out, "schema", MANIFEST_SCHEMA);
    push_str_field(&mut out, "tool", TOOL_NAME);
    push_str_field(&mut out, "verdict", report.verdict.as_str());
    push_str_field(&mut out, "point", &config.point);
    push_raw_field(&mut out, "sample_rate_hz", &fmt_sig(inputs.sample_rate_hz));
    push_raw_field(&mut out, "tolerance_db", &fmt_sig(config.tolerance_db));
    push_raw_field(
        &mut out,
        "min_compared_bands",
        &config.min_compared_bands.to_string(),
    );
    push_raw_field(&mut out, "window_len", &config.window_len.to_string());
    push_raw_field(&mut out, "lane", &config.lane.to_string());
    push_raw_field(
        &mut out,
        "compared_bands",
        &report.compared_bands.to_string(),
    );
    push_raw_field(&mut out, "missing_bands", &report.missing_bands.to_string());
    push_raw_field(&mut out, "violations", &report.violations.to_string());
    let max_abs = match report.max_abs_delta_db {
        Some(value) => fmt_sig(value),
        None => "null".to_string(),
    };
    push_raw_field(&mut out, "max_abs_delta_db", &max_abs);
    push_key(&mut out, "centers_hz");
    out.push('[');
    for (index, center_hz) in config.centers_hz.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&fmt_sig(*center_hz));
    }
    out.push(']');
    out.push(',');
    push_key(&mut out, "inputs");
    out.push('{');
    push_str_field(&mut out, "plan_sha256", inputs.plan_sha256);
    push_str_field(&mut out, "baseline_wav_sha256", inputs.baseline_wav_sha256);
    push_str_field_last(&mut out, "applied_wav_sha256", inputs.applied_wav_sha256);
    out.push('}');
    out.push(',');
    push_key(&mut out, "outputs");
    out.push('{');
    push_str_field(&mut out, "predicted_json_sha256", predicted_sha256);
    push_str_field(&mut out, "observed_json_sha256", observed_sha256);
    push_str_field_last(&mut out, "report_json_sha256", report_sha256);
    out.push('}');
    out.push('}');
    out.push('\n');
    out.into_bytes()
}

fn push_key(out: &mut String, key: &str) {
    out.push('"');
    out.push_str(key);
    out.push('"');
    out.push(':');
}

fn push_str_field(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push('"');
    out.push_str(value);
    out.push('"');
    out.push(',');
}

fn push_str_field_last(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push('"');
    out.push_str(value);
    out.push('"');
}

fn push_raw_field(out: &mut String, key: &str, value: &str) {
    push_key(out, key);
    out.push_str(value);
    out.push(',');
}

/// 10 位有效数字科学计数（与闭环报告同一规范）。
fn fmt_sig(value: f64) -> String {
    format!("{value:.9e}")
}
