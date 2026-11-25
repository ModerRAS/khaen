//! 4.1 测量质量门：逐 1/3 oct 带 SNR → Known / Unknown（**Unknown ≠ 能力不足**）。
//!
//! 口径（与既有工具链一致，不另造一套）：
//! - 频带：ISO 1/3 oct 中心 20 Hz–20 kHz（31 带），边界 `[c·2^(-1/6), c·2^(+1/6))`；
//! - SNR：**IR 直达峰值 / IR 末端噪声底**，先按 1/3 oct 带通再算（带限 SNR，
//!   对应 calibration-spec/ACCEPTANCE §0.5）。公式与
//!   `rt-stack/tools/calib-batch/calbatch/dsp.py::estimate_snr_db` 同口径：
//!   `20·log10(max|ir_band| / rms(尾段 0.1 s))`；带通复用 [crate::decay::bandpass_ir]
//!   （RBJ 1/3 oct biquad，前向+反向零相位，与 T20 口径同一实现）；
//! - 证据优先：`khaen-channel-measurement/v1` 已有 `snr_band_db`（+ 通带 `snr_band_hz`）
//!   时直接复用（[QualityGate::from_evidence] / [QualityConfig::evidence_snr_band_db]），
//!   **不在证据结构里另加字段**；
//! - 门限可配，默认 [DEFAULT_MIN_SNR_DB] = 15 dB（计划 5.1 的验收矩阵）；
//! - 低于门限 / 无证据 / 证据通带之外的频带判 **Unknown**，并从拟合中排除
//!   （[QualityGate::bin_fit_mask] / [QualityGate::excluded_for_plan]），
//!   报告里列出频带 + 原因。
//!
//! **语义（硬要求）**：Unknown 表示"这次测量在该频段不可判定"，唯一正确的动作是不拟合它；
//! 它**不是**"该音箱在该频点没能力"。能力不足由权威加权（[crate::authority]）处理，
//! 两者不得互相替代：Unknown 的频段不进目标/不评判，权威对待的是测量合格但物理上弱的频点。

use crate::decay;
use crate::evaluate::BandPlan;
use crate::report::Json;

/// ISO 1/3 oct 中心频率（20 Hz–20 kHz，31 带）。
pub const THIRD_OCTAVE_CENTERS_HZ_20_20K: [f64; 31] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// 默认 SNR 门限（dB）：低于此值的 1/3 oct 带判 Unknown 并排除拟合。
pub const DEFAULT_MIN_SNR_DB: f64 = 15.0;
/// 噪声底估计窗（秒；IR 末端），与 calib-batch `estimate_snr_db` 的默认一致。
pub const SNR_NOISE_TAIL_SECONDS: f64 = 0.1;
/// 噪声底为 0 时的饱和值（dB；对应 dsp.py 的 120 dB，不返回 inf）。
pub const SNR_SATURATED_DB: f64 = 120.0;

/// 频带判定（Known / Unknown）。**Unknown ≠ 能力不足**。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandState {
    Known,
    Unknown,
}

impl BandState {
    pub fn as_str(self) -> &'static str {
        match self {
            BandState::Known => "known",
            BandState::Unknown => "unknown",
        }
    }
}

/// 单频带的测量质量结论。
#[derive(Clone, Debug)]
pub struct BandQuality {
    pub center_hz: f64,
    pub low_hz: f64,
    pub high_hz: f64,
    /// 该带 SNR（dB）；证据缺失时为 None。
    pub snr_db: Option<f64>,
    pub state: BandState,
    /// Unknown 原因码（Known 恒 None）：
    /// `snr_below_min` / `no_snr_evidence` / `outside_measurement_band` /
    /// `ir_too_short` / `silent_band` / `ir_not_finite`。
    pub reason: Option<&'static str>,
}

impl BandQuality {
    /// 是否被排除出拟合（= Unknown）。
    pub fn excluded(&self) -> bool {
        self.state == BandState::Unknown
    }

    pub fn contains(&self, f: f64) -> bool {
        f >= self.low_hz && f < self.high_hz
    }
}

/// 逐 1/3 oct 带的质量门结果（频带表 + 门限）。
#[derive(Clone, Debug)]
pub struct QualityGate {
    pub min_snr_db: f64,
    pub bands: Vec<BandQuality>,
}

impl QualityGate {
    pub fn len(&self) -> usize {
        self.bands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bands.is_empty()
    }

    /// 被排除（Unknown）的频带。
    pub fn excluded_bands(&self) -> Vec<&BandQuality> {
        self.bands.iter().filter(|b| b.excluded()).collect()
    }

    /// 被排除频带的中心频率（升序）。
    pub fn excluded_centers_hz(&self) -> Vec<f64> {
        self.excluded_bands().iter().map(|b| b.center_hz).collect()
    }

    /// 频率落在哪个 1/3 oct 带（不在 20 Hz–20 kHz 内返回 None：此类频点不受门控）。
    pub fn band_for_freq(&self, f: f64) -> Option<usize> {
        self.bands.iter().position(|b| b.contains(f))
    }

    /// 该频点是否被排除（True = 不参与拟合）。
    pub fn is_excluded_freq(&self, f: f64) -> bool {
        self.band_for_freq(f)
            .map(|i| self.bands[i].excluded())
            .unwrap_or(false)
    }

    /// per-bin 拟合掩码（true = 参与拟合）；门控范围之外的频点一律 true（沿用旧行为）。
    pub fn bin_fit_mask(&self, freqs_hz: &[f64]) -> Vec<bool> {
        freqs_hz
            .iter()
            .map(|f| !self.is_excluded_freq(*f))
            .collect()
    }

    /// evaluated 计划里每条带是否被排除（用于报告/判据，不改变 BandPlan 结构）。
    pub fn excluded_for_plan(&self, plan: &BandPlan) -> Vec<bool> {
        plan.centers_hz
            .iter()
            .map(|c| self.is_excluded_freq(*c))
            .collect()
    }

    /// 质量门 JSON（报告用；含排除列表与原因）。
    pub fn json(&self) -> Json {
        let bands = self
            .bands
            .iter()
            .map(|b| {
                Json::Obj(vec![
                    ("center_hz".to_string(), Json::num(b.center_hz)),
                    ("low_hz".to_string(), Json::num(b.low_hz)),
                    ("high_hz".to_string(), Json::num(b.high_hz)),
                    ("snr_db".to_string(), Json::opt_num(b.snr_db)),
                    ("state".to_string(), Json::str(b.state.as_str())),
                    (
                        "reason".to_string(),
                        match b.reason {
                            Some(r) => Json::str(r),
                            None => Json::Null,
                        },
                    ),
                ])
            })
            .collect();
        let excluded: Vec<Json> = self
            .excluded_bands()
            .iter()
            .map(|b| {
                Json::Obj(vec![
                    ("center_hz".to_string(), Json::num(b.center_hz)),
                    ("snr_db".to_string(), Json::opt_num(b.snr_db)),
                    (
                        "reason".to_string(),
                        match b.reason {
                            Some(r) => Json::str(r),
                            None => Json::Null,
                        },
                    ),
                ])
            })
            .collect();
        Json::Obj(vec![
            ("kind".to_string(), Json::str("snr_per_third_octave")),
            ("min_snr_db".to_string(), Json::num(self.min_snr_db)),
            ("bands".to_string(), Json::Arr(bands)),
            ("excluded_bands".to_string(), Json::Arr(excluded)),
            (
                "semantics".to_string(),
                Json::str("unknown_ne_insufficient_authority: Unknown 只表示该频段测量不可判定并被排除出拟合；能力不足由 authority 加权处理"),
            ),
        ])
    }
}

/// 1/3 oct 带边界 `[c·2^(-1/6), c·2^(+1/6))`。
pub fn band_bounds(center_hz: f64) -> (f64, f64) {
    let k = 2.0f64.powf(1.0 / 6.0);
    (center_hz / k, center_hz * k)
}

/// 单带 SNR 估计（dB）：1/3 oct 带通 IR 的峰值 / 尾段噪声底。
///
/// 尾段窗 = 末 [SNR_NOISE_TAIL_SECONDS] 秒（至少 16 样本）；噪声底为 0 时返回
/// [SNR_SATURATED_DB]。失败返回原因码（调用方据此标 Unknown，不得当 0 处理）。
pub fn band_snr_estimate(ir: &[f64], fs: f64, center_hz: f64) -> Result<f64, &'static str> {
    if ir.len() < 64 {
        return Err("ir_too_short");
    }
    if !(fs.is_finite() && fs > 0.0) || !(center_hz.is_finite() && center_hz > 0.0) {
        return Err("ir_not_finite");
    }
    let band = decay::bandpass_ir(ir, fs, center_hz);
    if !band.iter().all(|v| v.is_finite()) {
        return Err("ir_not_finite");
    }
    let peak = band.iter().fold(0.0f64, |acc, v| acc.max(v.abs()));
    let n_tail = ((SNR_NOISE_TAIL_SECONDS * fs).round() as usize).max(16);
    if band.len() <= n_tail {
        return Err("ir_too_short");
    }
    let tail = &band[band.len() - n_tail..];
    let noise = (tail.iter().map(|v| v * v).sum::<f64>() / tail.len() as f64).sqrt();
    if peak <= 0.0 {
        return Err("silent_band");
    }
    if !(noise.is_finite() && noise > 0.0) {
        return Ok(SNR_SATURATED_DB);
    }
    Ok(20.0 * (peak / noise).log10())
}

/// 单带 SNR（失败 → None；未知原因用 [band_snr_estimate] 取）。
pub fn band_snr_db(ir: &[f64], fs: f64, center_hz: f64) -> Option<f64> {
    band_snr_estimate(ir, fs, center_hz).ok()
}

/// 逐带 SNR 估计（每条带一个 Result；Unknown 原因随结果返回）。
pub fn band_snr_curve(ir: &[f64], fs: f64, centers_hz: &[f64]) -> Vec<Result<f64, &'static str>> {
    centers_hz
        .iter()
        .map(|c| band_snr_estimate(ir, fs, *c))
        .collect()
}

fn gate_err(msg: &str) -> String {
    format!("quality gate: {msg}")
}

/// 由逐带 SNR 证据构造质量门（None = 无证据 → Unknown/`no_snr_evidence`）。
pub fn gate_from_band_snr(
    centers_hz: &[f64],
    snr_db: &[Option<f64>],
    min_snr_db: f64,
) -> Result<QualityGate, String> {
    if centers_hz.is_empty() {
        return Err(gate_err("空频带表"));
    }
    if snr_db.len() != centers_hz.len() {
        return Err(gate_err(&format!(
            "snr 证据长度 {} != 频带数 {}",
            snr_db.len(),
            centers_hz.len()
        )));
    }
    if !(min_snr_db.is_finite() && min_snr_db >= 0.0) {
        return Err(gate_err(&format!("门限 {min_snr_db} 非法")));
    }
    let bands = centers_hz
        .iter()
        .zip(snr_db.iter())
        .map(|(c, s)| {
            let (low, high) = band_bounds(*c);
            let (state, reason) = match s {
                Some(v) if v.is_finite() => {
                    if *v >= min_snr_db {
                        (BandState::Known, None)
                    } else {
                        (BandState::Unknown, Some("snr_below_min"))
                    }
                }
                _ => (BandState::Unknown, Some("no_snr_evidence")),
            };
            BandQuality {
                center_hz: *c,
                low_hz: low,
                high_hz: high,
                snr_db: s.filter(|v| v.is_finite()),
                state,
                reason,
            }
        })
        .collect();
    Ok(QualityGate { min_snr_db, bands })
}

/// 由测量 IR 构造质量门（逐 1/3 oct 带通 → 峰值/尾段噪声底 → 门限）。
///
/// 估计失败的频带标 Unknown 并把失败原因写进 `reason`（不当 0 dB 处理）。
pub fn gate_from_ir(
    ir: &[f64],
    fs: f64,
    centers_hz: &[f64],
    min_snr_db: f64,
) -> Result<QualityGate, String> {
    if centers_hz.is_empty() {
        return Err(gate_err("空频带表"));
    }
    if !(min_snr_db.is_finite() && min_snr_db >= 0.0) {
        return Err(gate_err(&format!("门限 {min_snr_db} 非法")));
    }
    let mut gate = gate_from_band_snr(centers_hz, &vec![None; centers_hz.len()], min_snr_db)?;
    for (band, estimate) in gate
        .bands
        .iter_mut()
        .zip(band_snr_curve(ir, fs, centers_hz))
    {
        match estimate {
            Ok(snr) => {
                band.snr_db = Some(snr);
                if snr >= min_snr_db {
                    band.state = BandState::Known;
                    band.reason = None;
                } else {
                    band.state = BandState::Unknown;
                    band.reason = Some("snr_below_min");
                }
            }
            Err(reason) => {
                band.snr_db = None;
                band.state = BandState::Unknown;
                band.reason = Some(reason);
            }
        }
    }
    Ok(gate)
}

/// 复用既有测量证据（`khaen-channel-measurement/v1` 的 `snr_band_db`）构造质量门。
///
/// - `snr_band_db = None` → 全部 Unknown（`no_snr_evidence`，fail-closed）；
/// - `evidence_band_hz = Some((lo, hi))` → 只有完全落在证据通带内的频带采用该值，
///   带外频带 Unknown（`outside_measurement_band`）；
/// - `evidence_band_hz = None` → 该值作用于全部频带（旧工具只给全带 SNR 的情形）。
pub fn gate_from_evidence(
    snr_band_db: Option<f64>,
    evidence_band_hz: Option<(f64, f64)>,
    centers_hz: &[f64],
    min_snr_db: f64,
) -> Result<QualityGate, String> {
    if centers_hz.is_empty() {
        return Err(gate_err("空频带表"));
    }
    let Some(snr) = snr_band_db.filter(|v| v.is_finite()) else {
        return gate_from_band_snr(centers_hz, &vec![None; centers_hz.len()], min_snr_db);
    };
    let mut gate = gate_from_band_snr(centers_hz, &vec![Some(snr); centers_hz.len()], min_snr_db)?;
    if let Some((lo, hi)) = evidence_band_hz {
        if !(lo.is_finite() && hi.is_finite() && hi > lo) {
            return Err(gate_err(&format!("证据通带 {lo}..{hi} 非法")));
        }
        for band in gate.bands.iter_mut() {
            if band.low_hz < lo || band.high_hz > hi {
                band.state = BandState::Unknown;
                band.reason = Some("outside_measurement_band");
            }
        }
    }
    Ok(gate)
}

/// 质量门配置（RunConfig.quality；默认关闭 → 与旧行为逐位一致）。
#[derive(Clone, Debug)]
pub struct QualityConfig {
    pub enabled: bool,
    /// SNR 门限（dB）
    pub min_snr_db: f64,
    /// 1/3 oct 中心（默认 20 Hz–20 kHz 31 带）
    pub centers_hz: Vec<f64>,
    /// 逐带 SNR 证据（来自 [band_snr_curve] 或外部证据）；空 = 无逐带证据
    pub band_snr_db: Vec<Option<f64>>,
    /// 整段带限 SNR 证据（复用 `khaen-channel-measurement/v1::snr_band_db`）
    pub evidence_snr_band_db: Option<f64>,
    /// 该证据的测量通带（复用 `snr_band_hz`）
    pub evidence_band_hz: Option<(f64, f64)>,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_snr_db: DEFAULT_MIN_SNR_DB,
            centers_hz: THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec(),
            band_snr_db: Vec::new(),
            evidence_snr_band_db: None,
            evidence_band_hz: None,
        }
    }
}

impl QualityConfig {
    /// 由逐带 SNR 构造（enabled = true）。
    pub fn from_band_snr(band_snr_db: Vec<Option<f64>>) -> Self {
        Self {
            enabled: true,
            band_snr_db,
            ..Self::default()
        }
    }

    /// 由测量 IR 构造（逐 1/3 oct 带通估计）。
    pub fn from_ir(ir: &[f64], fs: f64) -> Result<Self, String> {
        let centers = THIRD_OCTAVE_CENTERS_HZ_20_20K.to_vec();
        let curve = band_snr_curve(ir, fs, &centers);
        let band_snr_db = curve.into_iter().map(|estimate| estimate.ok()).collect();
        Ok(Self {
            enabled: true,
            centers_hz: centers,
            band_snr_db,
            ..Self::default()
        })
    }

    /// 由既有证据构造（`snr_band_db` + 可选通带）。
    pub fn from_evidence(snr_band_db: Option<f64>, evidence_band_hz: Option<(f64, f64)>) -> Self {
        Self {
            enabled: true,
            evidence_snr_band_db: snr_band_db,
            evidence_band_hz,
            ..Self::default()
        }
    }

    /// 构造质量门（fail-closed：证据长度不符/门限非法一律 Err，不静默降级）。
    pub fn gate(&self) -> Result<QualityGate, String> {
        if !self.enabled {
            return Err(gate_err("未启用（QualityConfig.enabled = false）"));
        }
        if !self.band_snr_db.is_empty() {
            if self.band_snr_db.len() != self.centers_hz.len() {
                return Err(gate_err(&format!(
                    "逐带 SNR 证据长度 {} != 频带数 {}",
                    self.band_snr_db.len(),
                    self.centers_hz.len()
                )));
            }
            return gate_from_band_snr(&self.centers_hz, &self.band_snr_db, self.min_snr_db);
        }
        gate_from_evidence(
            self.evidence_snr_band_db,
            self.evidence_band_hz,
            &self.centers_hz,
            self.min_snr_db,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: u64) -> f64 {
        // 确定性 LCG（不用外部随机数，保证可复现）。
        let mut x = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        x ^= x >> 33;
        (x as f64 / u64::MAX as f64) * 2.0 - 1.0
    }

    /// 合成 IR：60 Hz 衰减正弦（信号）+ 尾段 8 kHz 单音（把 8 kHz 带的噪声底抬到
    /// "无信号"水平）+ 极低全带白噪声。这样 63 Hz 带 SNR 高、8 kHz 带 SNR 低，
    /// 判定与门限的关系是解析可预期的（不依赖随机数统计）。
    fn synthetic_ir(fs: f64, seconds: f64) -> Vec<f64> {
        let n = (seconds * fs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / fs;
                let signal = if t < 0.1 {
                    (-t / 0.05).exp() * (std::f64::consts::TAU * 60.0 * t).sin()
                } else {
                    0.0
                };
                let tail_tone = if t > 0.15 {
                    5e-3 * (std::f64::consts::TAU * 8000.0 * t).sin()
                } else {
                    0.0
                };
                signal + tail_tone + 1e-6 * noise(i as u64 + 7)
            })
            .collect()
    }

    #[test]
    fn band_snr_separates_signal_from_noise_only_bands() {
        let fs = 48000.0;
        let ir = synthetic_ir(fs, 0.3);
        let low = band_snr_estimate(&ir, fs, 63.0).expect("63 Hz SNR");
        let high = band_snr_estimate(&ir, fs, 8000.0).expect("8 kHz SNR");
        assert!(low > 30.0, "63 Hz SNR={low} 应远高于门限");
        assert!(high < 15.0, "8 kHz SNR={high} 应低于 15 dB（只有噪声）");
    }

    #[test]
    fn gate_marks_low_snr_band_unknown_with_reason() {
        let centers = [63.0, 100.0, 125.0, 160.0];
        let snr = [Some(30.0), Some(12.0), None, Some(15.0)];
        let gate = gate_from_band_snr(&centers, &snr, 15.0).expect("gate");
        assert_eq!(gate.bands[0].state, BandState::Known);
        assert_eq!(gate.bands[1].state, BandState::Unknown);
        assert_eq!(gate.bands[1].reason, Some("snr_below_min"));
        assert_eq!(gate.bands[2].state, BandState::Unknown);
        assert_eq!(gate.bands[2].reason, Some("no_snr_evidence"));
        // 恰好等于门限算 Known（>= 门限）
        assert_eq!(gate.bands[3].state, BandState::Known);
        assert_eq!(gate.excluded_centers_hz(), vec![100.0, 125.0]);
        // 掩码：100 Hz 的频点被排除，63 Hz 的保留
        let mask = gate.bin_fit_mask(&[63.0, 100.0, 125.0]);
        assert_eq!(mask, vec![true, false, false]);
        // 门控范围外（10 Hz）不受影响
        assert!(gate.bin_fit_mask(&[10.0])[0]);
    }

    #[test]
    fn gate_from_ir_excludes_bands_without_signal() {
        let fs = 48000.0;
        let ir = synthetic_ir(fs, 0.3);
        let centers = [63.0, 8000.0];
        let gate = gate_from_ir(&ir, fs, &centers, 15.0).expect("gate");
        assert_eq!(gate.bands[0].state, BandState::Known, "{:?}", gate.bands[0]);
        assert_eq!(gate.bands[1].state, BandState::Unknown);
        assert_eq!(gate.bands[1].reason, Some("snr_below_min"));
    }

    #[test]
    fn evidence_without_passband_broadcasts_and_with_passband_excludes_outside() {
        let centers = [63.0, 1000.0, 8000.0];
        let g = gate_from_evidence(Some(30.0), None, &centers, 15.0).expect("gate");
        assert!(g.bands.iter().all(|b| b.state == BandState::Known));
        // 通带 50..5000：63 Hz/1 kHz 在内，8 kHz 在外
        let g = gate_from_evidence(Some(30.0), Some((50.0, 5000.0)), &centers, 15.0).expect("gate");
        assert_eq!(g.bands[0].state, BandState::Known);
        assert_eq!(g.bands[1].state, BandState::Known);
        assert_eq!(g.bands[2].state, BandState::Unknown);
        assert_eq!(g.bands[2].reason, Some("outside_measurement_band"));
        // 无证据 → 全 Unknown（fail-closed）
        let g = gate_from_evidence(None, None, &centers, 15.0).expect("gate");
        assert!(g.bands.iter().all(|b| b.reason == Some("no_snr_evidence")));
    }

    #[test]
    fn config_reuses_evidence_fields_and_validates_length() {
        let cfg = QualityConfig::from_evidence(Some(20.0), Some((40.0, 200.0)));
        assert!(cfg.enabled);
        let gate = cfg.gate().expect("gate");
        assert!(gate.band_for_freq(63.0).is_some());
        assert!(!gate.is_excluded_freq(63.0));
        assert!(gate.is_excluded_freq(1000.0));
        // 长度不符 → Err（不静默降级）
        let mut bad = QualityConfig::from_band_snr(vec![Some(30.0)]);
        assert!(bad.gate().is_err());
        bad.centers_hz = vec![63.0];
        assert!(bad.gate().is_ok());
    }

    #[test]
    fn unknown_is_not_insufficient_authority() {
        // 语义断言：低 SNR → Unknown（排除拟合）；高 SNR 的弱频点仍是 Known。
        let centers = [100.0];
        let gate = gate_from_band_snr(&centers, &[Some(10.0)], 15.0).expect("gate");
        assert_eq!(gate.bands[0].state, BandState::Unknown);
        let gate = gate_from_band_snr(&centers, &[Some(40.0)], 15.0).expect("gate");
        assert_eq!(gate.bands[0].state, BandState::Known);
        assert!(gate.bands[0].reason.is_none());
    }
}
