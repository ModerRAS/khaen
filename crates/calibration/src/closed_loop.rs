//! 闭环复测判定（T42 离线部分）：预测 vs 实测在 1/3 octave 带域上的 ≤2 dB 判定。
//!
//! # 输入
//!
//! 两份「带域电平集合」（schema 见 CLI `khaen-closed-loop`）：
//! - predicted：设计/求解给出的**预测**后级电平（dB，逐测点逐带）；
//! - observed：**实测**后级电平（dB，逐测点逐带）。
//!
//! 本模块只做对账：同测点、同频带中心上 `delta = observed - predicted`，
//! `|delta| <= tolerance_db`（默认 2.0 dB）即该带通过。它**不做**测量、不做 DSP、
//! 不碰设备；真实采集与 apply 属 T42 设备侧（NOT_RUN）。
//!
//! # 判定语义（fail-closed）
//!
//! | 结论 | 条件 |
//! | --- | --- |
//! | PASS | 所有可比较带都在容差内，且可比较带数 >= min_compared_bands |
//! | FAIL | 出现至少一个超出容差的带（可比较带数足够） |
//! | INCOMPLETE | 可比较带数 < min_compared_bands（数据不够，**不得**报 PASS） |
//!
//! 缺数据（任一侧为 null）算 missing，**不计为通过**；对齐错误（测点/频带集合不一致、
//! 采样率不一致、重复测点、非有限电平）一律返回稳定错误码，不做"尽量对齐"。
//!
//! # 可复现
//!
//! `report_bytes` 输出确定性 JSON（固定键序、10 位有效数字科学计数、末尾换行），
//! 同一输入两次得到逐字节相同的报告与 sha256（`report_sha256`）。

/// 报告 schema。
pub const REPORT_SCHEMA: &str = "khaen-closed-loop-report/v1";
/// 电平集合 schema（CLI 输入文档用）。
pub const LEVELS_SCHEMA: &str = "khaen-closed-loop-levels/v1";
/// 默认容差（dB）。
pub const DEFAULT_TOLERANCE_DB: f64 = 2.0;
/// 默认最少可比较带数：低于此数判 INCOMPLETE，不硬判 PASS/FAIL。
pub const DEFAULT_MIN_COMPARED_BANDS: usize = 3;

/// 稳定错误码（可被 REST / 报告直接匹配，不要改字符串）。
pub mod error {
    /// 输入结构/字段非法（形状、重复测点、非有限值等）。
    pub const BAD_INPUT: &str = "E_CLOSED_LOOP_INPUT";
    /// 两份输入无法对齐（测点集合、频带中心、采样率不一致）。
    pub const ALIGNMENT: &str = "E_CLOSED_LOOP_ALIGNMENT";
    /// 判定配置非法（容差 / 最少带数）。
    pub const CONFIG: &str = "E_CLOSED_LOOP_CONFIG";
}

/// 判定错误：稳定码 + 说明。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedLoopError {
    code: &'static str,
    message: String,
}

impl ClosedLoopError {
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

    /// 人读说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for ClosedLoopError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ClosedLoopError {}

/// 一个频带的电平；`None` = 该带没有数据（N/A）。
#[derive(Clone, Debug, PartialEq)]
pub struct BandLevel {
    /// 1/3 octave 中心频率（Hz）。
    pub center_hz: f64,
    /// 带域电平（dB）；None = 无数据。
    pub level_db: Option<f64>,
}

/// 一个测点的逐带电平。
#[derive(Clone, Debug, PartialEq)]
pub struct PointLevels {
    /// 测点名（同一份集合内唯一）。
    pub name: String,
    /// 逐带电平（调用方按中心频率升序给出）。
    pub bands: Vec<BandLevel>,
}

/// 一份带域电平集合。
#[derive(Clone, Debug, PartialEq)]
pub struct LevelSet {
    /// 采样率（Hz）；两份集合必须一致。
    pub sample_rate_hz: f64,
    /// 测点集合（不要求有序；对账时按名字排序）。
    pub points: Vec<PointLevels>,
}

/// 判定配置。
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedLoopConfig {
    /// 容差（dB），必须有限且 > 0。
    pub tolerance_db: f64,
    /// 最少可比较带数，必须 >= 1。
    pub min_compared_bands: usize,
}

impl Default for ClosedLoopConfig {
    fn default() -> Self {
        Self {
            tolerance_db: DEFAULT_TOLERANCE_DB,
            min_compared_bands: DEFAULT_MIN_COMPARED_BANDS,
        }
    }
}

impl ClosedLoopConfig {
    fn validate(&self) -> Result<(), ClosedLoopError> {
        if !self.tolerance_db.is_finite() || self.tolerance_db <= 0.0 {
            return Err(ClosedLoopError::new(
                error::CONFIG,
                "tolerance_db must be a finite value > 0",
            ));
        }
        if self.min_compared_bands == 0 {
            return Err(ClosedLoopError::new(
                error::CONFIG,
                "min_compared_bands must be >= 1",
            ));
        }
        Ok(())
    }
}

/// 一次对账的结论（三态；数据不足是 INCOMPLETE，不冒充 PASS）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// 所有可比较带都在容差内且数量足够。
    Pass,
    /// 至少一个可比较带超容差。
    Fail,
    /// 可比较带数不足（数据不够）。
    Incomplete,
}

impl Verdict {
    /// 稳定字符串（报告/CLI 用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Incomplete => "INCOMPLETE",
        }
    }
}

/// 单带对账结果。
#[derive(Clone, Debug, PartialEq)]
pub struct BandComparison {
    /// 中心频率（Hz）。
    pub center_hz: f64,
    /// 预测电平（dB）。
    pub predicted_db: Option<f64>,
    /// 实测电平（dB）。
    pub observed_db: Option<f64>,
    /// 实测 - 预测（dB）；任一侧无数据为 None。
    pub delta_db: Option<f64>,
    /// 是否在容差内；任一侧无数据为 None（不算通过）。
    pub within_tolerance: Option<bool>,
}

/// 单测点对账结果。
#[derive(Clone, Debug, PartialEq)]
pub struct PointComparison {
    /// 测点名。
    pub name: String,
    /// 逐带结果（中心频率升序）。
    pub bands: Vec<BandComparison>,
    /// 本测点可比较带数。
    pub compared_bands: usize,
    /// 本测点缺数据带数。
    pub missing_bands: usize,
    /// 本测点最大 |delta|（dB）；没有可比较带为 None。
    pub max_abs_delta_db: Option<f64>,
}

/// 闭环复测报告（纯数据；序列化见 [`report_bytes`]）。
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedLoopReport {
    /// 容差（dB）。
    pub tolerance_db: f64,
    /// 最少可比较带数。
    pub min_compared_bands: usize,
    /// 三态结论。
    pub verdict: Verdict,
    /// 全部测点合计可比较带数。
    pub compared_bands: usize,
    /// 全部测点合计缺数据带数。
    pub missing_bands: usize,
    /// 超出容差的带数。
    pub violations: usize,
    /// 全部可比较带的最大 |delta|（dB）。
    pub max_abs_delta_db: Option<f64>,
    /// 逐测点结果（按测点名升序）。
    pub points: Vec<PointComparison>,
    /// 判定说明（固定文本，便于人读）。
    pub notes: Vec<&'static str>,
}

const NOTE_MISSING: &str = "bands with missing data are counted as missing, never as pass";
const NOTE_INCOMPLETE: &str =
    "INCOMPLETE means there are too few comparable bands; it is not a pass";
const NOTE_NO_AUTO_PASS: &str =
    "PASS requires every comparable band within tolerance and enough comparable bands";

/// 对账：预测 vs 实测（默认容差 2.0 dB、至少 3 个可比较带）。
pub fn compare(
    predicted: &LevelSet,
    observed: &LevelSet,
    config: &ClosedLoopConfig,
) -> Result<ClosedLoopReport, ClosedLoopError> {
    config.validate()?;
    validate_levels("predicted", predicted)?;
    validate_levels("observed", observed)?;
    if !same_sample_rate(predicted.sample_rate_hz, observed.sample_rate_hz) {
        return Err(ClosedLoopError::new(
            error::ALIGNMENT,
            format!(
                "sample rate mismatch: predicted {} Hz, observed {} Hz",
                predicted.sample_rate_hz, observed.sample_rate_hz
            ),
        ));
    }

    let mut predicted_points: Vec<&PointLevels> = predicted.points.iter().collect();
    let mut observed_points: Vec<&PointLevels> = observed.points.iter().collect();
    predicted_points.sort_by(|a, b| a.name.cmp(&b.name));
    observed_points.sort_by(|a, b| a.name.cmp(&b.name));
    if predicted_points.len() != observed_points.len() {
        return Err(alignment_points(predicted_points, observed_points));
    }

    let mut points = Vec::with_capacity(predicted_points.len());
    let mut compared_bands = 0usize;
    let mut missing_bands = 0usize;
    let mut violations = 0usize;
    let mut max_abs_delta: Option<f64> = None;

    for (predicted_point, observed_point) in predicted_points.iter().zip(observed_points.iter()) {
        if predicted_point.name != observed_point.name {
            return Err(ClosedLoopError::new(
                error::ALIGNMENT,
                format!(
                    "point sets differ: predicted has {}, observed has {}",
                    predicted_point.name, observed_point.name
                ),
            ));
        }
        let predicted_bands = sorted_bands(&predicted_point.name, &predicted_point.bands)?;
        let observed_bands = sorted_bands(&observed_point.name, &observed_point.bands)?;
        if predicted_bands.len() != observed_bands.len() {
            return Err(ClosedLoopError::new(
                error::ALIGNMENT,
                format!(
                    "point {} has {} predicted bands but {} observed bands",
                    predicted_point.name,
                    predicted_bands.len(),
                    observed_bands.len()
                ),
            ));
        }
        let mut bands = Vec::with_capacity(predicted_bands.len());
        let mut point_compared = 0usize;
        let mut point_missing = 0usize;
        let mut point_max_abs: Option<f64> = None;
        for (predicted_band, observed_band) in predicted_bands.iter().zip(observed_bands.iter()) {
            if !same_center(predicted_band.center_hz, observed_band.center_hz) {
                return Err(ClosedLoopError::new(
                    error::ALIGNMENT,
                    format!(
                        "point {} band centers differ: predicted {} Hz, observed {} Hz",
                        predicted_point.name, predicted_band.center_hz, observed_band.center_hz
                    ),
                ));
            }
            let (delta_db, within_tolerance) =
                match (predicted_band.level_db, observed_band.level_db) {
                    (Some(predicted_db), Some(observed_db)) => {
                        let delta = observed_db - predicted_db;
                        let within = delta.abs() <= config.tolerance_db + 1e-9;
                        point_compared += 1;
                        compared_bands += 1;
                        if !within {
                            violations += 1;
                        }
                        point_max_abs =
                            Some(point_max_abs.map_or(delta.abs(), |best| best.max(delta.abs())));
                        max_abs_delta =
                            Some(max_abs_delta.map_or(delta.abs(), |best| best.max(delta.abs())));
                        (Some(delta), Some(within))
                    }
                    _ => {
                        point_missing += 1;
                        missing_bands += 1;
                        (None, None)
                    }
                };
            bands.push(BandComparison {
                center_hz: predicted_band.center_hz,
                predicted_db: predicted_band.level_db,
                observed_db: observed_band.level_db,
                delta_db,
                within_tolerance,
            });
        }
        points.push(PointComparison {
            name: predicted_point.name.clone(),
            bands,
            compared_bands: point_compared,
            missing_bands: point_missing,
            max_abs_delta_db: point_max_abs,
        });
    }

    let verdict = if compared_bands < config.min_compared_bands {
        Verdict::Incomplete
    } else if violations > 0 {
        Verdict::Fail
    } else {
        Verdict::Pass
    };
    Ok(ClosedLoopReport {
        tolerance_db: config.tolerance_db,
        min_compared_bands: config.min_compared_bands,
        verdict,
        compared_bands,
        missing_bands,
        violations,
        max_abs_delta_db: max_abs_delta,
        points,
        notes: vec![NOTE_MISSING, NOTE_INCOMPLETE, NOTE_NO_AUTO_PASS],
    })
}

fn validate_levels(label: &str, set: &LevelSet) -> Result<(), ClosedLoopError> {
    if !set.sample_rate_hz.is_finite() || set.sample_rate_hz <= 0.0 {
        return Err(ClosedLoopError::new(
            error::BAD_INPUT,
            format!("{label} sample_rate_hz must be a finite value > 0"),
        ));
    }
    let mut names: Vec<&str> = set.points.iter().map(|point| point.name.as_str()).collect();
    names.sort_unstable();
    for pair in names.windows(2) {
        if pair[0] == pair[1] {
            return Err(ClosedLoopError::new(
                error::BAD_INPUT,
                format!("{label} has duplicate point name: {}", pair[0]),
            ));
        }
    }
    for point in &set.points {
        if point.name.is_empty() {
            return Err(ClosedLoopError::new(
                error::BAD_INPUT,
                format!("{label} has an empty point name"),
            ));
        }
        let mut centers: Vec<f64> = point.bands.iter().map(|band| band.center_hz).collect();
        centers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        for center in &centers {
            if !center.is_finite() || *center <= 0.0 {
                return Err(ClosedLoopError::new(
                    error::BAD_INPUT,
                    format!("{label} point {} has an invalid band center", point.name),
                ));
            }
        }
        for pair in centers.windows(2) {
            if same_center(pair[0], pair[1]) {
                return Err(ClosedLoopError::new(
                    error::BAD_INPUT,
                    format!("{label} point {} has duplicate band center", point.name),
                ));
            }
        }
        for band in &point.bands {
            if let Some(level) = band.level_db {
                if !level.is_finite() {
                    return Err(ClosedLoopError::new(
                        error::BAD_INPUT,
                        format!(
                            "{label} point {} band {} Hz has a non-finite level",
                            point.name, band.center_hz
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn sorted_bands<'a>(
    name: &str,
    bands: &'a [BandLevel],
) -> Result<Vec<&'a BandLevel>, ClosedLoopError> {
    let mut sorted: Vec<&BandLevel> = bands.iter().collect();
    sorted.sort_by(|a, b| {
        a.center_hz
            .partial_cmp(&b.center_hz)
            .unwrap_or(core::cmp::Ordering::Equal)
    });
    for pair in sorted.windows(2) {
        if same_center(pair[0].center_hz, pair[1].center_hz) {
            return Err(ClosedLoopError::new(
                error::BAD_INPUT,
                format!("point {name} has duplicate band center"),
            ));
        }
    }
    Ok(sorted)
}

fn alignment_points<'a>(
    predicted: Vec<&'a PointLevels>,
    observed: Vec<&'a PointLevels>,
) -> ClosedLoopError {
    let predicted_names: Vec<&str> = predicted.iter().map(|point| point.name.as_str()).collect();
    let observed_names: Vec<&str> = observed.iter().map(|point| point.name.as_str()).collect();
    ClosedLoopError::new(
        error::ALIGNMENT,
        format!(
            "point sets differ: predicted {:?}, observed {:?}",
            predicted_names, observed_names
        ),
    )
}

fn same_sample_rate(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

fn same_center(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

/// 确定性报告 JSON 字节（固定键序、10 位有效数字、末尾换行）。
pub fn report_bytes(report: &ClosedLoopReport) -> Vec<u8> {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"schema\":\"{REPORT_SCHEMA}\","));
    out.push_str(&format!("\"verdict\":\"{}\",", report.verdict.as_str()));
    out.push_str(&format!(
        "\"tolerance_db\":{},",
        fmt_sig(report.tolerance_db)
    ));
    out.push_str(&format!(
        "\"min_compared_bands\":{},",
        report.min_compared_bands
    ));
    out.push_str(&format!("\"compared_bands\":{},", report.compared_bands));
    out.push_str(&format!("\"missing_bands\":{},", report.missing_bands));
    out.push_str(&format!("\"violations\":{},", report.violations));
    out.push_str(&format!(
        "\"max_abs_delta_db\":{},",
        match report.max_abs_delta_db {
            Some(value) => fmt_sig(value),
            None => "null".to_string(),
        }
    ));
    out.push_str("\"points\":[");
    for (index, point) in report.points.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!("{{\"name\":\"{}\",", json_escape(&point.name)));
        out.push_str(&format!("\"compared_bands\":{},", point.compared_bands));
        out.push_str(&format!("\"missing_bands\":{},", point.missing_bands));
        out.push_str(&format!(
            "\"max_abs_delta_db\":{},",
            match point.max_abs_delta_db {
                Some(value) => fmt_sig(value),
                None => "null".to_string(),
            }
        ));
        out.push_str("\"bands\":[");
        for (band_index, band) in point.bands.iter().enumerate() {
            if band_index > 0 {
                out.push(',');
            }
            out.push_str(&format!("{{\"center_hz\":{},", fmt_sig(band.center_hz)));
            out.push_str(&format!(
                "\"predicted_db\":{},\"observed_db\":{},",
                opt_fmt(band.predicted_db),
                opt_fmt(band.observed_db)
            ));
            out.push_str(&format!(
                "\"delta_db\":{},\"within_tolerance\":{}}}",
                opt_fmt(band.delta_db),
                match band.within_tolerance {
                    Some(true) => "true".to_string(),
                    Some(false) => "false".to_string(),
                    None => "null".to_string(),
                }
            ));
        }
        out.push_str("]}");
    }
    out.push_str("],\"notes\":[");
    for (index, note) in report.notes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!("\"{}\"", json_escape(note)));
    }
    out.push_str("]}\n");
    out.into_bytes()
}

/// 报告的 sha256（十六进制）；与 [`report_bytes`] 一一对应。
pub fn report_sha256(report: &ClosedLoopReport) -> String {
    khaen_core::sha256::sha256_hex(&report_bytes(report))
}

/// 带域电平集合的确定性 JSON 字节（khaen-closed-loop-levels/v1）。
///
/// 与 report_bytes 同一规范：固定键序、10 位有效数字、末尾换行；
/// 任何用本函数写出的文件都能被闭环 CLI 直接读回。
pub fn levels_bytes(set: &LevelSet) -> Vec<u8> {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"schema\":\"{LEVELS_SCHEMA}\","));
    out.push_str(&format!(
        "\"sample_rate_hz\":{},",
        fmt_sig(set.sample_rate_hz)
    ));
    out.push_str("\"points\":[");
    for (point_index, point) in set.points.iter().enumerate() {
        if point_index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"name\":\"{}\",\"bands\":[",
            json_escape(&point.name)
        ));
        for (band_index, band) in point.bands.iter().enumerate() {
            if band_index > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"center_hz\":{},\"level_db\":{}}}",
                fmt_sig(band.center_hz),
                opt_fmt(band.level_db)
            ));
        }
        out.push_str("]}");
    }
    out.push_str("]}\n");
    out.into_bytes()
}

/// 带域电平集合的 sha256（十六进制）；与 levels_bytes 一一对应。
pub fn levels_sha256(set: &LevelSet) -> String {
    khaen_core::sha256::sha256_hex(&levels_bytes(set))
}

fn opt_fmt(value: Option<f64>) -> String {
    match value {
        Some(value) => fmt_sig(value),
        None => "null".to_string(),
    }
}

/// 10 位有效数字科学计数（确定性；与 mimo report 的规范一致）。
fn fmt_sig(value: f64) -> String {
    format!("{value:.9e}")
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => out.push(character),
        }
    }
    out
}
