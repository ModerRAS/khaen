//! 4.4 安全钳位 + 三态输出（FULL / PARTIAL / NONE）。
//!
//! 计划 D8/5.1：**安全类钳位不越过、目标类顶满并输出 PARTIAL + 残差**。
//! 本模块把"补不动"和"不许补"分开：
//!
//! - 安全限（硬上界，永不越过）：
//!   1. 单段 boost <= +3 dB（每 source、每 1/3 oct 带）；
//!   2. 每通道正向增益和 <= +6 dB（每 source 跨带求和）；
//!   3. 预测峰值 <= -1 dBFS。
//! - 投影顺序：带帽 → 正向和 → 重拟 FIR → 重算峰值；仍超限时**先降 pre_gain**，
//!   **再缩目标幅度**（把修正量向直通收缩），仍不行才判失败（NONE，且交付物仍被
//!   强制压到限内 —— 不许为了好看把超限解当成功）。
//! - 三态：FULL（无钳位且达标）/ PARTIAL（有钳位或有残差）/ NONE（安全限内无可行解）。
//!   PARTIAL/NONE 都带逐项残差：角色（source id / target / pre_gain / target_scale）、
//!   频点、差多少 dB、撞了哪条限。
//!
//! 峰值口径（与 constraints::enforce_fir_constraints 的声明式 headroom 一致）：
//!   `predicted_peak_dbfs = reference_peak_dbfs + pre_gain_db + 10log10(max_m,f |P|/max_m,f|P0|)`
//! 其中 P = H·G（含源间相干求和），P0 = B0 压力；reference_peak_dbfs 默认
//! `20log10(REFERENCE_INPUT_PEAK) = -6.02 dBFS`（仿真无真实 playback level 的声明值，
//! 有实测电平时由调用方覆盖）。峰值是**相对 B0 的绝对电平**，所以降 pre_gain 会真的降峰值。
//!
//! 安全限一旦确定就**永不越过**：最后一层 `final_guard` 全局缩放保证三条上界成立，
//! 并把缩放量写进报告（宁可交付更小声的解，也不交付超限解）。

use crate::evaluate::BandPlan;
use crate::fir;
use crate::report::Json;
use crate::solve::{self, Cx};

/// 单段 boost 上限（dB）。
pub const MAX_BAND_BOOST_DB: f64 = 3.0;
/// 每通道正向增益和上限（dB）。
pub const MAX_POSITIVE_SUM_DB: f64 = 6.0;
/// 预测峰值上限（dBFS）。
pub const MAX_PREDICTED_PEAK_DBFS: f64 = -1.0;
/// 声明式 B0 参考峰值（dBFS）= 20log10(0.5)。
pub const REFERENCE_PEAK_DBFS: f64 = -6.020599913279624;

/// 三态结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeState {
    Full,
    Partial,
    None,
}

impl OutcomeState {
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeState::Full => "FULL",
            OutcomeState::Partial => "PARTIAL",
            OutcomeState::None => "NONE",
        }
    }
}

/// 撞到的安全限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitKind {
    BandBoost,
    PositiveSum,
    PredictedPeak,
}

impl LimitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitKind::BandBoost => "band_boost",
            LimitKind::PositiveSum => "positive_sum",
            LimitKind::PredictedPeak => "predicted_peak",
        }
    }
}

/// 一次钳位事件。
#[derive(Clone, Debug)]
pub struct ClampHit {
    pub limit: LimitKind,
    /// 相关 source（峰值/全局类事件为 None）。
    pub source: Option<usize>,
    pub band_index: Option<usize>,
    pub band_center_hz: Option<f64>,
    /// 钳位前该量的值（dB / dBFS）。
    pub before_db: f64,
    /// 钳位后该量的值。
    pub after_db: f64,
    /// 超出量（dB，> 0）。
    pub excess_db: f64,
}

/// 逐项残差（PARTIAL/NONE 必带）。
#[derive(Clone, Debug)]
pub struct Residual {
    /// 角色："target" / source id / "pre_gain" / "target_scale" / "final_guard"。
    pub role: String,
    pub source: Option<usize>,
    pub band_center_hz: Option<f64>,
    /// 想达到的量（dB；target 项是相对 B0 的目标变化）。
    pub target_db: f64,
    /// 实际达到的量（dB）。
    pub achieved_db: f64,
    /// 差多少 dB（> 0 = 未达标）。
    pub deficit_db: f64,
    /// 造成该残差的安全限（target 项的纯目标不足为 None）。
    pub limit: Option<LimitKind>,
}

/// 安全钳位配置（RunConfig.safety；默认关闭 → 与旧行为逐位一致）。
#[derive(Clone, Debug)]
pub struct SafetyConfig {
    pub enabled: bool,
    pub max_band_boost_db: f64,
    pub max_positive_sum_db: f64,
    pub max_predicted_peak_dbfs: f64,
    /// B0 参考峰值（dBFS）；默认 REFERENCE_PEAK_DBFS。
    pub reference_peak_dbfs: f64,
    /// 初始 pre_gain（dB）。
    pub pre_gain_db: f64,
    /// pre_gain 最多可降多少 dB（到 -max_pre_gain_reduction_db 为止）。
    pub max_pre_gain_reduction_db: f64,
    /// 目标幅度最多可缩多少 dB。
    pub max_target_scale_db: f64,
    /// 每次缩目标的步长（dB）。
    pub target_scale_step_db: f64,
    /// FULL 判据容差（dB）；残差 <= 容差算达成。
    pub tolerance_db: f64,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_band_boost_db: MAX_BAND_BOOST_DB,
            max_positive_sum_db: MAX_POSITIVE_SUM_DB,
            max_predicted_peak_dbfs: MAX_PREDICTED_PEAK_DBFS,
            reference_peak_dbfs: REFERENCE_PEAK_DBFS,
            pre_gain_db: 0.0,
            max_pre_gain_reduction_db: 12.0,
            max_target_scale_db: 6.0,
            target_scale_step_db: 1.0,
            tolerance_db: 1.0,
        }
    }
}

impl SafetyConfig {
    /// 启用（默认限值）。
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), String> {
        let finite = |v: f64| v.is_finite();
        if !finite(self.max_band_boost_db) {
            return Err("safety: max_band_boost_db 非法".to_string());
        }
        if !finite(self.max_positive_sum_db) || self.max_positive_sum_db < 0.0 {
            return Err("safety: max_positive_sum_db 非法".to_string());
        }
        if !finite(self.max_predicted_peak_dbfs) {
            return Err("safety: max_predicted_peak_dbfs 非法".to_string());
        }
        if !finite(self.reference_peak_dbfs) {
            return Err("safety: reference_peak_dbfs 非法".to_string());
        }
        if !finite(self.pre_gain_db) {
            return Err("safety: pre_gain_db 非法".to_string());
        }
        if !finite(self.max_pre_gain_reduction_db) || self.max_pre_gain_reduction_db < 0.0 {
            return Err("safety: max_pre_gain_reduction_db 非法".to_string());
        }
        if !finite(self.max_target_scale_db) || self.max_target_scale_db < 0.0 {
            return Err("safety: max_target_scale_db 非法".to_string());
        }
        if !finite(self.target_scale_step_db) || self.target_scale_step_db <= 0.0 {
            return Err("safety: target_scale_step_db 非法".to_string());
        }
        if !finite(self.tolerance_db) || self.tolerance_db < 0.0 {
            return Err("safety: tolerance_db 非法".to_string());
        }
        Ok(())
    }
}

/// 安全钳位报告（PARTIAL/NONE 的残差都在这里）。
#[derive(Clone, Debug)]
pub struct SafetyReport {
    pub state: OutcomeState,
    /// 生效限值快照。
    pub limits: SafetyConfig,
    /// 钳位后每 source、每 1/3 oct 带的增益（dB；[source][band]）。
    pub max_band_gain_db: Vec<Vec<f64>>,
    /// 钳位后每 source 的正向增益和（dB）。
    pub positive_sum_db: Vec<f64>,
    /// 钳位前预测峰值（dBFS，仅供参考）。
    pub peak_before_clamp_dbfs: f64,
    /// 最终预测峰值（dBFS）。
    pub predicted_peak_dbfs: f64,
    /// 最终 pre_gain（dB；<= 初始值）。
    pub pre_gain_db: f64,
    /// 最终目标幅度缩放（dB；<= 0）。
    pub target_scale_db: f64,
    /// final_guard 全局缩放（dB；<= 0，仅兜底时非 0）。
    pub final_guard_db: f64,
    pub hits: Vec<ClampHit>,
    pub residuals: Vec<Residual>,
    /// 目标带内最大缺口（dB）。
    pub target_deficit_max_db: f64,
    /// 安全限内无可行解（state = NONE 时 True）。
    pub exhausted: bool,
    /// 三条上界是否全部成立（永不越过的证据）。
    pub never_exceeded: bool,
    pub notes: Vec<&'static str>,
}

impl SafetyReport {
    /// 报告 JSON。
    pub fn json(&self, source_ids: &[String], plan: &BandPlan) -> Json {
        let hits: Vec<Json> = self
            .hits
            .iter()
            .map(|h| {
                Json::Obj(vec![
                    ("limit".to_string(), Json::str(h.limit.as_str())),
                    (
                        "source".to_string(),
                        match h.source {
                            Some(n) => Json::str(
                                source_ids
                                    .get(n)
                                    .cloned()
                                    .unwrap_or_else(|| format!("src{n}")),
                            ),
                            None => Json::Null,
                        },
                    ),
                    (
                        "band_center_hz".to_string(),
                        Json::opt_num(h.band_center_hz),
                    ),
                    ("before_db".to_string(), Json::num(h.before_db)),
                    ("after_db".to_string(), Json::num(h.after_db)),
                    ("excess_db".to_string(), Json::num(h.excess_db)),
                ])
            })
            .collect();
        let residuals: Vec<Json> = self
            .residuals
            .iter()
            .map(|r| {
                Json::Obj(vec![
                    ("role".to_string(), Json::str(r.role.clone())),
                    (
                        "band_center_hz".to_string(),
                        Json::opt_num(r.band_center_hz),
                    ),
                    ("target_db".to_string(), Json::num(r.target_db)),
                    ("achieved_db".to_string(), Json::num(r.achieved_db)),
                    ("deficit_db".to_string(), Json::num(r.deficit_db)),
                    (
                        "limit".to_string(),
                        match r.limit {
                            Some(k) => Json::str(k.as_str()),
                            None => Json::Null,
                        },
                    ),
                ])
            })
            .collect();
        let per_source: Vec<Json> = self
            .max_band_gain_db
            .iter()
            .enumerate()
            .map(|(n, bands)| {
                Json::Obj(vec![
                    (
                        "id".to_string(),
                        Json::str(
                            source_ids
                                .get(n)
                                .cloned()
                                .unwrap_or_else(|| format!("src{n}")),
                        ),
                    ),
                    (
                        "max_band_gain_db".to_string(),
                        Json::num(bands.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                    ),
                    (
                        "positive_sum_db".to_string(),
                        Json::num(self.positive_sum_db[n]),
                    ),
                ])
            })
            .collect();
        Json::Obj(vec![
            ("state".to_string(), Json::str(self.state.as_str())),
            (
                "limits".to_string(),
                Json::Obj(vec![
                    (
                        "max_band_boost_db".to_string(),
                        Json::num(self.limits.max_band_boost_db),
                    ),
                    (
                        "max_positive_sum_db".to_string(),
                        Json::num(self.limits.max_positive_sum_db),
                    ),
                    (
                        "max_predicted_peak_dbfs".to_string(),
                        Json::num(self.limits.max_predicted_peak_dbfs),
                    ),
                    (
                        "reference_peak_dbfs".to_string(),
                        Json::num(self.limits.reference_peak_dbfs),
                    ),
                    (
                        "max_pre_gain_reduction_db".to_string(),
                        Json::num(self.limits.max_pre_gain_reduction_db),
                    ),
                    (
                        "max_target_scale_db".to_string(),
                        Json::num(self.limits.max_target_scale_db),
                    ),
                    ("tolerance_db".to_string(), Json::num(self.limits.tolerance_db)),
                ]),
            ),
            (
                "predicted_peak_dbfs".to_string(),
                Json::num(self.predicted_peak_dbfs),
            ),
            (
                "peak_before_clamp_dbfs".to_string(),
                Json::num(self.peak_before_clamp_dbfs),
            ),
            ("pre_gain_db".to_string(), Json::num(self.pre_gain_db)),
            ("target_scale_db".to_string(), Json::num(self.target_scale_db)),
            ("final_guard_db".to_string(), Json::num(self.final_guard_db)),
            ("target_deficit_max_db".to_string(), Json::num(self.target_deficit_max_db)),
            ("exhausted".to_string(), Json::Bool(self.exhausted)),
            ("never_exceeded".to_string(), Json::Bool(self.never_exceeded)),
            ("per_source".to_string(), Json::Arr(per_source)),
            ("hits".to_string(), Json::Arr(hits)),
            ("residuals".to_string(), Json::Arr(residuals)),
            (
                "semantics".to_string(),
                Json::str("FULL = 无钳位且达标；PARTIAL = 撞安全限或目标残差（残差逐项列出）；NONE = 安全限内无可行解。安全限永不越过，顶满仍不达标不得报成功。"),
            ),
            (
                "band_centers_hz".to_string(),
                Json::Arr(plan.centers_hz.iter().map(|v| Json::num(*v)).collect()),
            ),
            ("notes".to_string(), Json::Arr(self.notes.iter().map(|s| Json::str(*s)).collect())),
        ])
    }
}

/// 频率 → 1/3 oct 带索引（带外取最近带；与 objective::target_per_bin 同口径）。
fn bin_band_index(freqs_hz: &[f64], plan: &BandPlan) -> Vec<usize> {
    let mut out = Vec::with_capacity(freqs_hz.len());
    for f in freqs_hz {
        let mut idx = None;
        for b in 0..plan.len() {
            if plan.bands[b].contains(*f) {
                idx = Some(b);
                break;
            }
        }
        let b = match idx {
            Some(b) => b,
            None => {
                if plan.is_empty() {
                    0
                } else if *f < plan.bands[0].low_hz {
                    0
                } else {
                    plan.len() - 1
                }
            }
        };
        out.push(b);
    }
    out
}

fn db20(x: f64) -> f64 {
    if x > 0.0 && x.is_finite() {
        20.0 * x.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 每 source、每带的带内最大增益（dB）。
fn band_gain_db_table(g: &[Vec<Cx>], freqs_hz: &[f64], plan: &BandPlan) -> Vec<Vec<f64>> {
    let map = bin_band_index(freqs_hz, plan);
    let mut out = vec![vec![f64::NEG_INFINITY; plan.len()]; g.len()];
    for (n, row) in g.iter().enumerate() {
        for (ki, value) in row.iter().enumerate() {
            let b = map[ki];
            let db = db20(value.abs());
            if db > out[n][b] {
                out[n][b] = db;
            }
        }
    }
    for row in out.iter_mut() {
        for v in row.iter_mut() {
            if !v.is_finite() {
                *v = -200.0;
            }
        }
    }
    out
}

/// 每 source 的正向增益和（dB）。
fn positive_sum_db(band_gain: &[Vec<f64>]) -> Vec<f64> {
    band_gain
        .iter()
        .map(|row| row.iter().map(|v| v.max(0.0)).sum::<f64>())
        .collect()
}

/// 安全评估输入（纯数据；便于单测）。
pub struct ClampInput<'a> {
    /// 设计 FIR：[source][tap]
    pub h: &'a [Vec<f64>],
    /// 原始传递函数 H：[mic][bin][source]
    pub h_matrix: &'a [Vec<Vec<Cx>>],
    /// B0 压力（G=1）：[mic][bin]
    pub p_b0: &'a [Vec<Cx>],
    /// 目标幅度 A(f)：[bin]（压力幅度）
    pub target_a: &'a [f64],
    pub freqs_hz: &'a [f64],
    pub fs: f64,
    pub taps: usize,
    pub delay_samples: usize,
    pub nfft: usize,
    /// 1/3 oct 带计划（安全限按它分段）
    pub plan: &'a BandPlan,
    /// design 测点（目标残差用）
    pub design_mics: &'a [usize],
    /// 参与峰值评估的测点（design + holdout）
    pub eval_mics: &'a [usize],
}

fn responses(h: &[Vec<f64>], freqs_hz: &[f64], fs: f64) -> Vec<Vec<Cx>> {
    h.iter()
        .map(|row| fir::fir_response(row, freqs_hz, fs))
        .collect()
}

/// 预测峰值（dBFS）：reference + 10log10(max_m,f |P| / max_m,f |P0|)。
///
/// P = H·G（含源间相干求和），P0 = B0 压力；P0 峰值为 0 时退化为 reference（只报电平）。
pub fn predicted_peak_dbfs(
    g: &[Vec<Cx>],
    h_matrix: &[Vec<Vec<Cx>>],
    p_b0: &[Vec<Cx>],
    mics: &[usize],
    reference_peak_dbfs: f64,
) -> f64 {
    let p = solve::pressure(h_matrix, g);
    let mut pmax = 0.0f64;
    let mut p0max = 0.0f64;
    for &m in mics {
        for (ki, v) in p[m].iter().enumerate() {
            pmax = pmax.max(v.abs());
            p0max = p0max.max(p_b0[m][ki].abs());
        }
    }
    if !(pmax.is_finite() && pmax > 0.0) {
        return f64::NEG_INFINITY;
    }
    let ratio_db = if p0max > 0.0 { db20(pmax / p0max) } else { 0.0 };
    reference_peak_dbfs + ratio_db
}

/// 由"期望响应 F"（不含公共延迟）拟合 FIR：C = (1 + tscale·(F−1))·e^{-jωD}。
fn fit_desired(
    desired: &[Vec<Cx>],
    freqs_hz: &[f64],
    fs: f64,
    taps: usize,
    delay_samples: usize,
    nfft: usize,
    tscale: f64,
) -> Result<Vec<Vec<f64>>, String> {
    let mut out = Vec::with_capacity(desired.len());
    for row in desired {
        let mut c = Vec::with_capacity(freqs_hz.len());
        for (ki, f_value) in row.iter().enumerate() {
            let ph = std::f64::consts::TAU * freqs_hz[ki] * delay_samples as f64 / fs;
            let f_scaled = Cx::new(1.0 + tscale * (f_value.re - 1.0), tscale * f_value.im);
            c.push(f_scaled.mul(Cx::exp_neg_j(ph)));
        }
        out.push(fir::design_fir_by_frequency_sampling(
            freqs_hz,
            fs,
            &c,
            taps,
            delay_samples,
            nfft,
        )?);
    }
    Ok(out)
}

/// 每（source, 带）需要施加的线性比例（<= 1）：先单段 boost 帽，再每通道正向和帽。
fn band_scale_for_limits(
    band_gain: &[Vec<f64>],
    cfg: &SafetyConfig,
    nb: usize,
) -> (Vec<Vec<f64>>, Vec<(LimitKind, usize, usize, f64, f64)>) {
    let n_src = band_gain.len();
    let mut scale = vec![vec![1.0f64; nb]; n_src];
    let mut events = Vec::new();
    for n in 0..n_src {
        for b in 0..nb {
            let gain = band_gain[n][b];
            if gain > cfg.max_band_boost_db {
                let target_db = cfg.max_band_boost_db;
                let s = 10f64.powf((target_db - gain) / 20.0);
                scale[n][b] = s.min(1.0);
                events.push((LimitKind::BandBoost, n, b, gain, target_db));
            }
        }
    }
    for n in 0..n_src {
        let mut pos = 0.0f64;
        for b in 0..nb {
            pos += (band_gain[n][b] + db20(scale[n][b])).max(0.0);
        }
        if pos > cfg.max_positive_sum_db + 1e-9 {
            let ratio = cfg.max_positive_sum_db / pos;
            for b in 0..nb {
                let after = band_gain[n][b] + db20(scale[n][b]);
                if after > 0.0 {
                    let target_db = after * ratio;
                    let extra = 10f64.powf((target_db - after) / 20.0);
                    scale[n][b] *= extra;
                }
            }
            events.push((LimitKind::PositiveSum, n, 0, pos, cfg.max_positive_sum_db));
        }
    }
    (scale, events)
}

/// 目标残差：每带 A_b 与 achieved |P|_b（相对 B0）的差，> tolerance 才保留。
fn target_residuals(
    input: &ClampInput,
    p_final: &[Vec<Cx>],
    tolerance_db: f64,
    band_limit: &[Option<LimitKind>],
) -> Vec<Residual> {
    let mut out = Vec::new();
    for b in 0..input.plan.len() {
        let bins = &input.plan.bins[b];
        if bins.is_empty() {
            continue;
        }
        let mut a = 0.0;
        for k in bins {
            a += input.target_a[*k];
        }
        a /= bins.len() as f64;
        if !(a > 0.0) {
            continue;
        }
        let mut p0_sum = 0.0;
        let mut p_sum = 0.0;
        for &m in input.design_mics {
            let mut e0 = 0.0;
            let mut e = 0.0;
            for k in bins {
                e0 += input.p_b0[m][*k].norm_sqr();
                e += p_final[m][*k].norm_sqr();
            }
            p0_sum += (e0 / bins.len() as f64).sqrt();
            p_sum += (e / bins.len() as f64).sqrt();
        }
        let n = input.design_mics.len().max(1) as f64;
        let p0 = p0_sum / n;
        let p = p_sum / n;
        if !(p0 > 0.0) {
            continue;
        }
        let target_db = db20(a / p0);
        let achieved_db = db20(p / p0);
        let deficit = target_db - achieved_db;
        if deficit > tolerance_db + 1e-9 {
            out.push(Residual {
                role: "target".to_string(),
                source: None,
                band_center_hz: Some(input.plan.centers_hz[b]),
                target_db,
                achieved_db,
                deficit_db: deficit,
                limit: band_limit[b],
            });
        }
    }
    out
}

/// 对所有解做安全投影 + 三态判定。
///
/// 返回（投影后 FIR、其实际响应 G、报告）。**安全限永不越过**：最终解必满足三条上界
/// （必要时由 final_guard 全局衰减，代价写进报告与残差）。
pub fn clamp_solution(
    input: &ClampInput,
    cfg: &SafetyConfig,
) -> Result<(Vec<Vec<f64>>, Vec<Vec<Cx>>, SafetyReport), String> {
    cfg.validate()?;
    let k = input.freqs_hz.len();
    let nb = input.plan.len();
    if input.h.is_empty() || k == 0 || nb == 0 {
        return Err("safety: 空输入（h/freqs/plan）".to_string());
    }
    if input.target_a.len() != k {
        return Err("safety: target_a 长度与频点数不符".to_string());
    }
    if input.h_matrix.is_empty() || input.p_b0.len() != input.h_matrix.len() {
        return Err("safety: H/P0 维度不符".to_string());
    }
    let n_src = input.h.len();
    let map = bin_band_index(input.freqs_hz, input.plan);
    let mut notes: Vec<&'static str> = Vec::new();

    // 初始响应 → 期望响应 F = G·e^{+jωD}
    let g0 = responses(input.h, input.freqs_hz, input.fs);
    let peak_before = predicted_peak_dbfs(
        &g0,
        input.h_matrix,
        input.p_b0,
        input.eval_mics,
        cfg.reference_peak_dbfs,
    );
    let mut desired: Vec<Vec<Cx>> = vec![Vec::with_capacity(k); n_src];
    for n in 0..n_src {
        for ki in 0..k {
            let ph =
                std::f64::consts::TAU * input.freqs_hz[ki] * input.delay_samples as f64 / input.fs;
            desired[n].push(g0[n][ki].mul(Cx::exp_pos_j(ph)));
        }
    }

    // 1) 带帽 + 正向和投影（最多 4 轮，每轮按实际响应修正期望响应）
    let mut hits: Vec<ClampHit> = Vec::new();
    let mut band_hits_first: Vec<Option<LimitKind>> = vec![None; nb];
    // 从"实际解"出发（不是从期望响应重拟合）：无钳位时逐位返回原解；
    // 有钳位时按实际响应逐轮修正期望响应（重拟 FIR）。
    let mut h_cur: Vec<Vec<f64>> = input.h.to_vec();
    for pass in 0..4 {
        let g = responses(&h_cur, input.freqs_hz, input.fs);
        let band_gain = band_gain_db_table(&g, input.freqs_hz, input.plan);
        let (scale, events) = band_scale_for_limits(&band_gain, cfg, nb);
        let changed = scale.iter().flatten().any(|s| *s < 1.0 - 1e-12);
        if !changed {
            break;
        }
        if pass == 0 {
            for (limit, n, b, before_db, after_db) in &events {
                // PositiveSum 是跨带事件（b 只是占位），不留 band 字段以免误导残差
                let (band_index, center) = match limit {
                    LimitKind::BandBoost => {
                        let center = input.plan.centers_hz.get(*b).copied();
                        band_hits_first[*b] = Some(LimitKind::BandBoost);
                        (Some(*b), center)
                    }
                    LimitKind::PositiveSum => (None, None),
                    LimitKind::PredictedPeak => (None, None),
                };
                hits.push(ClampHit {
                    limit: *limit,
                    source: Some(*n),
                    band_index,
                    band_center_hz: center,
                    before_db: *before_db,
                    after_db: *after_db,
                    excess_db: (before_db - after_db).max(0.0),
                });
            }
        }
        // 期望响应按带缩放，再重拟合
        for n in 0..n_src {
            for ki in 0..k {
                let b = map[ki];
                desired[n][ki] = desired[n][ki].scale(scale[n][b]);
            }
        }
        h_cur = fit_desired(
            &desired,
            input.freqs_hz,
            input.fs,
            input.taps,
            input.delay_samples,
            input.nfft,
            1.0,
        )?;
    }

    // 2) 峰值级联：先降 pre_gain，再缩目标幅度，仍不行才判失败
    let mut tscale = 1.0f64;
    let mut shrunk_db = 0.0f64;
    let mut pre_gain = cfg.pre_gain_db;
    let mut g_cur = responses(&h_cur, input.freqs_hz, input.fs);
    let mut peak = predicted_peak_dbfs(
        &g_cur,
        input.h_matrix,
        input.p_b0,
        input.eval_mics,
        cfg.reference_peak_dbfs,
    );
    let mut exhausted = false;
    while peak > cfg.max_predicted_peak_dbfs + 1e-9 {
        let excess = peak - cfg.max_predicted_peak_dbfs;
        let room = (pre_gain + cfg.max_pre_gain_reduction_db).max(0.0);
        if room > 1e-12 {
            let take = excess.min(room);
            pre_gain -= take;
            peak -= take;
            hits.push(ClampHit {
                limit: LimitKind::PredictedPeak,
                source: None,
                band_index: None,
                band_center_hz: None,
                before_db: peak + take,
                after_db: peak,
                excess_db: take,
            });
            notes.push("peak: pre_gain 下调（安全限内）");
            continue;
        }
        if shrunk_db < cfg.max_target_scale_db - 1e-12 {
            let step = excess
                .min(cfg.target_scale_step_db)
                .min(cfg.max_target_scale_db - shrunk_db);
            tscale *= 10f64.powf(-step / 20.0);
            shrunk_db += step;
            let before_peak = peak;
            h_cur = fit_desired(
                &desired,
                input.freqs_hz,
                input.fs,
                input.taps,
                input.delay_samples,
                input.nfft,
                tscale,
            )?;
            g_cur = responses(&h_cur, input.freqs_hz, input.fs);
            peak = predicted_peak_dbfs(
                &g_cur,
                input.h_matrix,
                input.p_b0,
                input.eval_mics,
                cfg.reference_peak_dbfs,
            );
            hits.push(ClampHit {
                limit: LimitKind::PredictedPeak,
                source: None,
                band_index: None,
                band_center_hz: None,
                before_db: before_peak,
                after_db: peak,
                excess_db: (before_peak - peak).max(0.0),
            });
            notes.push("peak: 目标幅度缩放（修正量向直通收缩）");
            continue;
        }
        exhausted = true;
        notes.push("peak: pre_gain 与目标缩放预算耗尽 → NONE");
        break;
    }

    // 3) 三条上界一次性校验 → 计算总共需要的衰减（安全限绝不越过）
    let band_gain_now = band_gain_db_table(&g_cur, input.freqs_hz, input.plan);
    let pos_now = positive_sum_db(&band_gain_now);
    let mut need = 0.0f64;
    for row in &band_gain_now {
        for v in row {
            need = need.max(v - cfg.max_band_boost_db);
        }
    }
    for v in &pos_now {
        need = need.max(v - cfg.max_positive_sum_db);
    }
    need = need.max(peak - cfg.max_predicted_peak_dbfs);
    let guard_db = -need.max(0.0);
    let atten_db = pre_gain + guard_db;
    let atten = 10f64.powf(atten_db / 20.0);
    if (atten - 1.0).abs() > 1e-15 {
        for row in h_cur.iter_mut() {
            for v in row.iter_mut() {
                *v *= atten;
            }
        }
    }
    if guard_db < -1e-9 {
        hits.push(ClampHit {
            limit: LimitKind::PredictedPeak,
            source: None,
            band_index: None,
            band_center_hz: None,
            before_db: 0.0,
            after_db: guard_db,
            excess_db: -guard_db,
        });
        notes.push("final_guard: 追加全局衰减以保证三条上界（安全限永不越过）");
    }

    // 4) 最终实测：带增益 / 正向和 / 峰值 / 目标残差
    let g_final = responses(&h_cur, input.freqs_hz, input.fs);
    let band_gain_final = band_gain_db_table(&g_final, input.freqs_hz, input.plan);
    let pos_final = positive_sum_db(&band_gain_final);
    let peak_final = predicted_peak_dbfs(
        &g_final,
        input.h_matrix,
        input.p_b0,
        input.eval_mics,
        cfg.reference_peak_dbfs,
    );
    let p_final = solve::pressure(input.h_matrix, &g_final);
    let mut residuals: Vec<Residual> = Vec::new();
    let source_role = |n: usize| format!("src{}", n);
    for h in &hits {
        if let Some(n) = h.source {
            residuals.push(Residual {
                role: source_role(n),
                source: Some(n),
                band_center_hz: h.band_center_hz,
                target_db: h.before_db,
                achieved_db: h.after_db,
                deficit_db: h.excess_db,
                limit: Some(h.limit),
            });
        }
    }
    if (pre_gain - cfg.pre_gain_db).abs() > 1e-9 {
        residuals.push(Residual {
            role: "pre_gain".to_string(),
            source: None,
            band_center_hz: None,
            target_db: cfg.pre_gain_db,
            achieved_db: pre_gain,
            deficit_db: cfg.pre_gain_db - pre_gain,
            limit: Some(LimitKind::PredictedPeak),
        });
    }
    if (20.0 * tscale.log10()).abs() > 1e-9 {
        residuals.push(Residual {
            role: "target_scale".to_string(),
            source: None,
            band_center_hz: None,
            target_db: 0.0,
            achieved_db: 20.0 * tscale.log10(),
            deficit_db: -(20.0 * tscale.log10()),
            limit: Some(LimitKind::PredictedPeak),
        });
    }
    let has_peak_hit = hits
        .iter()
        .any(|h| h.limit == LimitKind::PredictedPeak && h.source.is_none());
    if !has_peak_hit && guard_db < -1e-9 {
        residuals.push(Residual {
            role: "final_guard".to_string(),
            source: None,
            band_center_hz: None,
            target_db: 0.0,
            achieved_db: guard_db,
            deficit_db: -guard_db,
            limit: Some(LimitKind::PredictedPeak),
        });
    }
    let mut band_target_residuals =
        target_residuals(input, &p_final, cfg.tolerance_db, &band_hits_first);
    let target_deficit_max = band_target_residuals
        .iter()
        .map(|r| r.deficit_db)
        .fold(0.0f64, f64::max);
    let mut all_residuals = residuals;
    all_residuals.append(&mut band_target_residuals);

    // 5) 三态
    let never_exceeded = band_gain_final
        .iter()
        .all(|row| row.iter().all(|v| *v <= cfg.max_band_boost_db + 1e-6))
        && pos_final
            .iter()
            .all(|v| *v <= cfg.max_positive_sum_db + 1e-6)
        && peak_final <= cfg.max_predicted_peak_dbfs + 1e-6;
    let state = if exhausted || !never_exceeded {
        OutcomeState::None
    } else if hits.is_empty() && all_residuals.is_empty() {
        OutcomeState::Full
    } else {
        OutcomeState::Partial
    };
    notes.push("state: FULL=无钳位且达标 / PARTIAL=撞限或残差 / NONE=安全限内无解");

    Ok((
        h_cur,
        g_final,
        SafetyReport {
            state,
            limits: cfg.clone(),
            max_band_gain_db: band_gain_final,
            positive_sum_db: pos_final,
            peak_before_clamp_dbfs: peak_before,
            predicted_peak_dbfs: peak_final,
            pre_gain_db: pre_gain,
            target_scale_db: 20.0 * tscale.log10(),
            final_guard_db: guard_db,
            hits,
            residuals: all_residuals,
            target_deficit_max_db: target_deficit_max,
            exhausted,
            never_exceeded,
            notes,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constraints::REFERENCE_INPUT_PEAK;
    use crate::evaluate::BandPlan;

    const NFFT: usize = 8192;
    const TAPS: usize = 64;
    const DELAY: usize = 16;

    fn freqs() -> Vec<f64> {
        [7usize, 9, 11, 13]
            .iter()
            .map(|k| *k as f64 * 48000.0 / NFFT as f64)
            .collect()
    }

    fn plan() -> BandPlan {
        BandPlan::from_centers(&[40.0, 50.0, 63.0, 80.0], &freqs())
    }

    /// 2 源、1 测点的 H：H = [1.0, -0.9]（B0 在测点几乎抵消，修正可以形成大峰值）。
    fn h_matrix() -> Vec<Vec<Vec<Cx>>> {
        vec![freqs()
            .iter()
            .map(|_| vec![Cx::new(1.0, 0.0), Cx::new(-0.9, 0.0)])
            .collect()]
    }

    fn p_b0(h: &[Vec<Vec<Cx>>]) -> Vec<Vec<Cx>> {
        let n_src = h[0][0].len();
        let g = vec![vec![Cx::one(); h[0].len()]; n_src];
        solve::pressure(h, &g)
    }

    /// 单 tap 的 FIR（支撑在公共延迟 D）。
    fn delta(gain: f64) -> Vec<f64> {
        let mut h = vec![0.0; TAPS];
        h[DELAY] = gain;
        h
    }

    #[test]
    fn reference_peak_constant_matches_half_scale() {
        assert!((REFERENCE_PEAK_DBFS - 20.0 * REFERENCE_INPUT_PEAK.log10()).abs() < 1e-12);
        assert!((REFERENCE_PEAK_DBFS + 6.020599913279624).abs() < 1e-12);
    }

    #[test]
    fn full_state_when_no_clamp_and_target_met() {
        let hm = h_matrix();
        let p0 = p_b0(&hm);
        let target = vec![0.1; freqs().len()]; // = |P0|
        let input = ClampInput {
            h: &[delta(1.0), delta(1.0)],
            h_matrix: &hm,
            p_b0: &p0,
            target_a: &target,
            freqs_hz: &freqs(),
            fs: 48000.0,
            taps: TAPS,
            delay_samples: DELAY,
            nfft: NFFT,
            plan: &plan(),
            design_mics: &[0],
            eval_mics: &[0],
        };
        let (_h, _g, report) = clamp_solution(&input, &SafetyConfig::enabled()).expect("clamp");
        assert_eq!(report.state, OutcomeState::Full, "{:?}", report.residuals);
        assert!(report.hits.is_empty(), "{:?}", report.hits);
        assert!(report.never_exceeded);
        assert!(report.predicted_peak_dbfs <= MAX_PREDICTED_PEAK_DBFS + 1e-9);
    }

    #[test]
    fn band_and_positive_sum_caps_are_projected_and_never_crossed() {
        // 所有带 +6.02 dB → 单段帽 3 dB 与正向和帽 6 dB 同时触发
        let hm = h_matrix();
        let p0 = p_b0(&hm);
        let target = vec![0.1; freqs().len()];
        let input = ClampInput {
            h: &[delta(2.0), delta(2.0)],
            h_matrix: &hm,
            p_b0: &p0,
            target_a: &target,
            freqs_hz: &freqs(),
            fs: 48000.0,
            taps: TAPS,
            delay_samples: DELAY,
            nfft: NFFT,
            plan: &plan(),
            design_mics: &[0],
            eval_mics: &[0],
        };
        let (_h, _g, report) = clamp_solution(&input, &SafetyConfig::enabled()).expect("clamp");
        assert_eq!(report.state, OutcomeState::Partial);
        let max_band = report
            .max_band_gain_db
            .iter()
            .flatten()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(max_band <= MAX_BAND_BOOST_DB + 1e-6, "max_band={max_band}");
        assert!(
            report
                .positive_sum_db
                .iter()
                .all(|v| *v <= MAX_POSITIVE_SUM_DB + 1e-6),
            "{:?}",
            report.positive_sum_db
        );
        assert!(report.hits.iter().any(|h| h.limit == LimitKind::BandBoost));
        assert!(report
            .hits
            .iter()
            .any(|h| h.limit == LimitKind::PositiveSum));
        assert!(report.never_exceeded);
        // 残差必须交代撞了哪条限
        assert!(report.residuals.iter().any(|r| r.limit.is_some()));
    }

    #[test]
    fn peak_clamp_reduces_pre_gain_and_reports_residuals() {
        // G = [1.12, 1.0]，B0 压力 0.1 → P=0.22，峰值 0.83 dBFS，超出 -1 dBFS 共 1.83 dB；
        // 单段 0.98 dB x 4 带 = 3.9 dB 未触发正向和帽 → 只有峰值限生效。
        let hm = h_matrix();
        let p0 = p_b0(&hm);
        let target = vec![0.3; freqs().len()];
        let input = ClampInput {
            h: &[delta(1.12), delta(1.0)],
            h_matrix: &hm,
            p_b0: &p0,
            target_a: &target,
            freqs_hz: &freqs(),
            fs: 48000.0,
            taps: TAPS,
            delay_samples: DELAY,
            nfft: NFFT,
            plan: &plan(),
            design_mics: &[0],
            eval_mics: &[0],
        };
        let (_h, _g, report) = clamp_solution(&input, &SafetyConfig::enabled()).expect("clamp");
        assert_eq!(report.state, OutcomeState::Partial, "{:?}", report.hits);
        assert!(
            (report.predicted_peak_dbfs - MAX_PREDICTED_PEAK_DBFS).abs() < 1e-6,
            "peak={} before={} pre_gain={} guard={} hits={:?}",
            report.predicted_peak_dbfs,
            report.peak_before_clamp_dbfs,
            report.pre_gain_db,
            report.final_guard_db,
            report.hits
        );
        let expect_excess = 20.0 * 2.2f64.log10() + REFERENCE_PEAK_DBFS - MAX_PREDICTED_PEAK_DBFS;
        assert!(
            (report.pre_gain_db + expect_excess).abs() < 1e-9,
            "pre_gain={}",
            report.pre_gain_db
        );
        let pre = report
            .residuals
            .iter()
            .find(|r| r.role == "pre_gain")
            .unwrap_or_else(|| {
                panic!(
                    "missing pre_gain residual: state={:?} hits={:?} residuals={:?} peak={}",
                    report.state, report.hits, report.residuals, report.predicted_peak_dbfs
                )
            });
        assert!((pre.deficit_db - expect_excess).abs() < 1e-6, "{:?}", pre);
        assert_eq!(pre.limit, Some(LimitKind::PredictedPeak));
        // 目标残差：目标 A=0.3 对应 P/P0=3，实际被 pre_gain 压到 2.2*10^(-e/20)
        let achieved_ratio = 2.2 * 10f64.powf(-expect_excess / 20.0);
        let expect_target_deficit = 20.0 * (3.0 / achieved_ratio).log10();
        let tgt = report
            .residuals
            .iter()
            .find(|r| r.role == "target")
            .expect("missing target residual");
        assert!(
            (tgt.deficit_db - expect_target_deficit).abs() < 1e-6,
            "{:?} want {}",
            tgt,
            expect_target_deficit
        );
        assert!(report.never_exceeded);
    }

    #[test]
    fn peak_budget_exhaustion_fails_safely() {
        // 两个源反相驱动：B0 几乎抵消，压力峰值可到 +28 dB，远超 pre_gain/目标预算
        let hm = h_matrix();
        let p0 = p_b0(&hm);
        let mut h1 = vec![0.0; TAPS];
        h1[DELAY] = 1.41421356;
        let mut h2 = vec![0.0; TAPS];
        h2[DELAY] = -1.41421356;
        let target = vec![0.2; freqs().len()];
        let cfg = SafetyConfig {
            max_pre_gain_reduction_db: 2.0,
            max_target_scale_db: 2.0,
            ..SafetyConfig::enabled()
        };
        let input = ClampInput {
            h: &[h1, h2],
            h_matrix: &hm,
            p_b0: &p0,
            target_a: &target,
            freqs_hz: &freqs(),
            fs: 48000.0,
            taps: TAPS,
            delay_samples: DELAY,
            nfft: NFFT,
            plan: &plan(),
            design_mics: &[0],
            eval_mics: &[0],
        };
        let (_h, _g, report) = clamp_solution(&input, &cfg).expect("clamp");
        assert_eq!(report.state, OutcomeState::None, "{:?}", report.hits);
        assert!(report.exhausted);
        // 安全限绝不越过：即使判失败，交付解仍被压到限内
        assert!(
            report.predicted_peak_dbfs <= MAX_PREDICTED_PEAK_DBFS + 1e-6,
            "peak={}",
            report.predicted_peak_dbfs
        );
        assert!(report.never_exceeded, "{:?}", report);
        assert!(report.final_guard_db < -1e-6);
    }

    #[test]
    fn config_validation_rejects_nonsense() {
        let hm = h_matrix();
        let p0 = p_b0(&hm);
        let target = vec![0.1; freqs().len()];
        let input = ClampInput {
            h: &[delta(1.0), delta(1.0)],
            h_matrix: &hm,
            p_b0: &p0,
            target_a: &target,
            freqs_hz: &freqs(),
            fs: 48000.0,
            taps: TAPS,
            delay_samples: DELAY,
            nfft: NFFT,
            plan: &plan(),
            design_mics: &[0],
            eval_mics: &[0],
        };
        let cfg = SafetyConfig {
            tolerance_db: -1.0,
            ..SafetyConfig::enabled()
        };
        assert!(clamp_solution(&input, &cfg).is_err());
        let cfg = SafetyConfig {
            max_positive_sum_db: -3.0,
            ..SafetyConfig::enabled()
        };
        assert!(clamp_solution(&input, &cfg).is_err());
    }
}
