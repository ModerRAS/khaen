//! 从已编译方案（plan ABI v3）推出预测带域电平 —— 闭环复测的 predicted 侧。
//!
//! # 口径
//!
//! 预测值 = 每个保护项（lane）的 `gain_db` + 该 lane 全部 PEQ biquad 的级联幅度（dB），
//! 在给定 1/3 oct 中心频率处取值。biquad 差分方程：
//! `y[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y[n-1] - a2*y[n-2]`，
//! 因此稳态频响为 `(b0 + b1*z^-1 + b2*z^-2) / (1 + a1*z^-1 + a2*z^-2)`。
//!
//! # 诚实边界
//!
//! - 只覆盖方案里的保护段（PEQ + 每 lane 增益）；内容布局 / 分频 / 矩阵 / 路由必须
//!   是恒等段，否则返回 E_PLAN_LEVELS_PLAN 而不是悄悄少算。
//! - 预测是**相对频响**（0 dB = 方案外无滤波），不含房间、单元与麦克风响应；
//!   实测侧必须用同一参考归一化（实测带电平 - 实测参考电平）后再喂闭环对账。
//! - 测点名来自 lane 序号（L00 / L01 / ...），因为方案文件里没有测点名字；
//!   实测文件必须使用同一命名才能对齐。
//! - 延迟与极性不改变幅度响应，因此不参与预测。

use khaen_core::plan::{
    ContentSection, CrossoverSection, MatrixSection, PeqSection, Plan, RoutingSection,
};
use khaen_core::rt::SAMPLE_RATES_HZ;

use crate::closed_loop::{BandLevel, LevelSet, PointLevels};

/// 工具名（诊断/交接用）。
pub const TOOL_NAME: &str = "khaen-plan-levels";

/// 输入错误：频带中心列表非法。
pub const BAD_INPUT: &str = "E_PLAN_LEVELS_INPUT";
/// 配置错误：频带中心列表为空。
pub const CONFIG: &str = "E_PLAN_LEVELS_CONFIG";
/// 方案错误：采样率不支持、保护段缺失/不完整、非恒等段、非有限系数。
pub const PLAN: &str = "E_PLAN_LEVELS_PLAN";

/// 默认 1/3 oct 中心频率（25 Hz - 20 kHz，30 带）。
pub const DEFAULT_CENTERS_HZ: [f64; 30] = [
    25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0,
    630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0, 8000.0,
    10000.0, 12500.0, 16000.0, 20000.0,
];

/// 预测电平错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct PlanLevelsError {
    code: &'static str,
    message: String,
}

impl PlanLevelsError {
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

impl std::fmt::Display for PlanLevelsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PlanLevelsError {}

/// 从方案推出每个 lane 的预测带域电平（中心频率升序）。
///
/// `centers_hz` 必须非空、有限、严格升序、两两不同，且全部低于 Nyquist 频率；
/// 返回的 LevelSet 采样率取自方案，测点按 lane 升序，名字为 L00 / L01 / ...
pub fn predicted_levels(plan: &Plan, centers_hz: &[f64]) -> Result<LevelSet, PlanLevelsError> {
    if !SAMPLE_RATES_HZ.contains(&plan.sample_rate) {
        return Err(PlanLevelsError::new(
            PLAN,
            format!(
                "unsupported plan sample rate {} Hz (supported: 48000 / 96000)",
                plan.sample_rate
            ),
        ));
    }
    let sample_rate_hz = f64::from(plan.sample_rate);
    validate_centers(centers_hz, sample_rate_hz)?;
    require_identity_sections(plan)?;

    let protection = plan.protection.as_ref().ok_or_else(|| {
        PlanLevelsError::new(PLAN, "plan has no protection section (nothing to predict)")
    })?;
    let lanes = usize::from(protection.lanes);
    if lanes == 0 {
        return Err(PlanLevelsError::new(PLAN, "plan protection has zero lanes"));
    }
    let mut per_lane: Vec<Option<Vec<f64>>> = vec![None; lanes];
    for entry in &protection.entries {
        let lane = usize::from(entry.lane);
        if lane >= lanes {
            return Err(PlanLevelsError::new(
                PLAN,
                format!(
                    "protection entry references lane {} but plan declares {} lanes",
                    entry.lane, lanes
                ),
            ));
        }
        if per_lane[lane].is_some() {
            return Err(PlanLevelsError::new(
                PLAN,
                format!("duplicate protection entry for lane {}", entry.lane),
            ));
        }
        if !entry.gain_db.is_finite() {
            return Err(PlanLevelsError::new(
                PLAN,
                format!("lane {} gain_db is not finite", entry.lane),
            ));
        }
        let mut values = Vec::with_capacity(centers_hz.len());
        for &center_hz in centers_hz {
            let peq_db = cascade_magnitude_db(&entry.peq, center_hz, sample_rate_hz);
            let value = f64::from(entry.gain_db) + peq_db;
            if !value.is_finite() {
                return Err(PlanLevelsError::new(
                    PLAN,
                    format!(
                        "lane {} predicted level at {center_hz} Hz is not finite",
                        entry.lane
                    ),
                ));
            }
            values.push(value);
        }
        per_lane[lane] = Some(values);
    }
    let mut points = Vec::with_capacity(lanes);
    for (lane, values) in per_lane.iter().enumerate() {
        let values = values.as_ref().ok_or_else(|| {
            PlanLevelsError::new(
                PLAN,
                format!(
                    "plan protection is missing lane {} (predicted levels need every lane covered)",
                    lane
                ),
            )
        })?;
        points.push(PointLevels {
            name: lane_name(lane),
            bands: values
                .iter()
                .zip(centers_hz.iter())
                .map(|(&level_db, &center_hz)| BandLevel {
                    center_hz,
                    level_db: Some(level_db),
                })
                .collect(),
        });
    }
    Ok(LevelSet {
        sample_rate_hz,
        points,
    })
}

/// lane 序号到测点名：L00 / L01 / ...
pub fn lane_name(lane: usize) -> String {
    format!("L{lane:02}")
}

fn validate_centers(centers_hz: &[f64], sample_rate_hz: f64) -> Result<(), PlanLevelsError> {
    if centers_hz.is_empty() {
        return Err(PlanLevelsError::new(
            CONFIG,
            "band centers must not be empty",
        ));
    }
    let nyquist = sample_rate_hz / 2.0;
    let mut previous: Option<f64> = None;
    for &center_hz in centers_hz {
        if !center_hz.is_finite() || center_hz <= 0.0 {
            return Err(PlanLevelsError::new(
                BAD_INPUT,
                format!("band center {center_hz} Hz must be a finite value > 0"),
            ));
        }
        if center_hz >= nyquist {
            return Err(PlanLevelsError::new(
                BAD_INPUT,
                format!("band center {center_hz} Hz must be below Nyquist {nyquist} Hz"),
            ));
        }
        if let Some(previous) = previous {
            if center_hz <= previous {
                return Err(PlanLevelsError::new(
                    BAD_INPUT,
                    format!(
                        "band centers must be strictly ascending and unique: {} after {previous}",
                        center_hz
                    ),
                ));
            }
        }
        previous = Some(center_hz);
    }
    Ok(())
}

/// 只接受恒等的内容布局 / 分频 / 矩阵 / 路由：预测只覆盖保护段。
fn require_identity_sections(plan: &Plan) -> Result<(), PlanLevelsError> {
    if let Some(content) = plan.content.as_ref() {
        if !content_is_identity(content) {
            return Err(PlanLevelsError::new(
                PLAN,
                "plan content section is not identity; predicted levels only cover protection PEQ",
            ));
        }
    }
    if let Some(crossover) = plan.crossover.as_ref() {
        if !crossover_is_identity(crossover) {
            return Err(PlanLevelsError::new(
                PLAN,
                "plan crossover section is not identity; predicted levels only cover protection PEQ",
            ));
        }
    }
    if let Some(matrix) = plan.matrix.as_ref() {
        if !matrix_is_identity(matrix) {
            return Err(PlanLevelsError::new(
                PLAN,
                "plan matrix section is not identity; predicted levels only cover protection PEQ",
            ));
        }
    }
    if let Some(routing) = plan.routing.as_ref() {
        if !routing_is_identity(routing) {
            return Err(PlanLevelsError::new(
                PLAN,
                "plan routing section is not identity; predicted levels only cover protection PEQ",
            ));
        }
    }
    Ok(())
}

fn content_is_identity(content: &ContentSection) -> bool {
    if content.in_channels != content.out_lanes
        || usize::from(content.in_channels) != content.folds.len()
    {
        return false;
    }
    content.folds.iter().enumerate().all(|(index, fold)| {
        usize::from(fold.src) == index
            && usize::from(fold.dst) == index
            && (fold.gain - 1.0).abs() <= 1e-12
    })
}

fn crossover_is_identity(crossover: &CrossoverSection) -> bool {
    if usize::from(crossover.lanes) != crossover.filters.len() {
        return false;
    }
    crossover.filters.iter().enumerate().all(|(index, filter)| {
        usize::from(filter.lane) == index
            && filter.coeffs.len() == 1
            && (filter.coeffs[0] - 1.0).abs() <= 1e-12
    })
}

fn matrix_is_identity(matrix: &MatrixSection) -> bool {
    let lanes = usize::from(matrix.rows);
    if usize::from(matrix.cols) != lanes
        || matrix.decimation != 1
        || matrix.interpolation != 1
        || matrix.taps != 1
    {
        return false;
    }
    if matrix.coeffs.len() != lanes * lanes {
        return false;
    }
    matrix.coeffs.iter().enumerate().all(|(index, &value)| {
        let row = index / lanes;
        let column = index % lanes;
        let expected = if row == column { 1.0 } else { 0.0 };
        (value - expected).abs() <= 1e-12
    })
}

fn routing_is_identity(routing: &RoutingSection) -> bool {
    let lanes = routing.routes.len();
    usize::from(routing.dac_channels) == lanes
        && routing
            .routes
            .iter()
            .enumerate()
            .all(|(index, &(lane, dac))| usize::from(lane) == index && usize::from(dac) == index)
}

/// biquad 级联在 freq_hz 处的幅度（dB）。
fn cascade_magnitude_db(peq: &[PeqSection], freq_hz: f64, sample_rate_hz: f64) -> f64 {
    let omega = 2.0 * std::f64::consts::PI * freq_hz / sample_rate_hz;
    // z^-1 = e^{-j w}
    let (z1_im, z2_im) = (-omega.sin(), -(2.0 * omega).sin());
    let (z1_re, z2_re) = (omega.cos(), (2.0 * omega).cos());
    let mut total_db = 0.0;
    for section in peq {
        let b0 = f64::from(section.b0);
        let b1 = f64::from(section.b1);
        let b2 = f64::from(section.b2);
        let a1 = f64::from(section.a1);
        let a2 = f64::from(section.a2);
        let num_re = b0 + b1 * z1_re + b2 * z2_re;
        let num_im = b1 * z1_im + b2 * z2_im;
        let den_re = 1.0 + a1 * z1_re + a2 * z2_re;
        let den_im = a1 * z1_im + a2 * z2_im;
        let num_mag = num_re.hypot(num_im);
        let den_mag = den_re.hypot(den_im);
        if !num_mag.is_finite() || !den_mag.is_finite() || den_mag == 0.0 {
            return f64::NAN;
        }
        total_db += 20.0 * (num_mag / den_mag).log10();
    }
    total_db
}
