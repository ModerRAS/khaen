//! 多遍扫频聚合：中位数延迟、两层极性投票、时钟 ppm、可选重采样补偿。
use crate::dsp::{self, ClockFit};
use crate::model::{FrPoint, PolarityEstimators, RepeatDetail};
#[cfg(test)]
use crate::sweep::SweepSpec;

pub struct RepeatAnalysis {
    /// 重复 0 的起点（已去掉时钟斜率），样本。调用方再加上窗口起点。
    pub delay_samples: f64,
    pub delay_coarse_samples: i64,
    #[allow(dead_code)]
    pub polarity_legacy: i32,
    pub robust_polarity: i32,
    pub listening_polarity: i32,
    pub confidence: String,
    pub estimators: PolarityEstimators,
    pub repeats: RepeatDetail,
    pub clock_ppm: Option<f64>,
    pub clock_ppm_ok: bool,
    /// A1：ppm 不确定度（ppm）；clock_ppm 与它同量级时视为噪声，不当物理漂移。
    pub clock_uncertainty_ppm: f64,
    /// A1：|ppm| > 2*uncertainty 才为 true。
    pub clock_significant: bool,
    pub onset_invalid: bool,
    pub drift_corrected: bool,
    pub fr: Vec<FrPoint>,
    pub fr_modal: Vec<FrPoint>,
    pub peak_db: f64,
    pub phase_1k_deg: f64,
    pub snr_db: f64,
    /// P0-3：通道通带内 SNR（逐遍中位数），供 merge 的测点门限使用。
    pub snr_band_db: f64,
    pub snr_band_hz: (f64, f64),
    pub warnings: Vec<String>,
}

struct Hit {
    k: usize,
    onset: f64,
    result_polarity_legacy: i32,
    result: crate::dsp::DeconvResult,
}

/// 把相距不超过 radius 的峰收成一个，留下归一化相关更大的那个。
fn cluster_peaks(peaks: &[dsp::CorrPeak], radius: f64) -> Vec<dsp::CorrPeak> {
    let mut xs = peaks.to_vec();
    xs.sort_by(|a, b| {
        a.lag
            .partial_cmp(&b.lag)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out: Vec<dsp::CorrPeak> = Vec::new();
    for p in xs {
        if let Some(prev) = out.last_mut() {
            if (p.lag - prev.lag).abs() <= radius {
                if p.mag > prev.mag {
                    *prev = p.clone();
                }
                continue;
            }
        }
        out.push(p);
    }
    out
}

/// 偏差同时受 5% 间距和 2000 样本约束，取更严的那个。
fn grid_tolerance(spacing: f64) -> f64 {
    (0.05 * spacing).min(2000.0).max(1.0)
}

/// 相邻间隔里，只拿和标称间隔相差不超过 1000 ppm 的那些做中位数。
/// 旁瓣会把相邻差拉偏上千样本，那种差不能拿来当栅格间距。
/// 离栅格超过容差的孤立峰丢掉；同一格点留归一化相关更大的那个。
/// 锚点必须是自己那一格里相关最强的峰，否则旁瓣会把 2–3k 外的假峰拉进容差。
fn filter_peaks_on_grid(peaks: &[dsp::CorrPeak], nominal_interval: f64) -> Vec<dsp::CorrPeak> {
    if peaks.len() <= 1 || !(nominal_interval.is_finite() && nominal_interval > 1.0) {
        return peaks.to_vec();
    }
    let mut order: Vec<usize> = (0..peaks.len()).collect();
    order.sort_by(|&a, &b| {
        peaks[a]
            .lag
            .partial_cmp(&peaks[b].lag)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let lags: Vec<f64> = order.iter().map(|&i| peaks[i].lag).collect();
    let diffs: Vec<f64> = lags.windows(2).map(|w| w[1] - w[0]).collect();
    let spacing_tol = (nominal_interval * 1e-3).max(2.0);
    let near: Vec<f64> = diffs
        .iter()
        .copied()
        .filter(|d| (*d - nominal_interval).abs() <= spacing_tol)
        .collect();
    let spacing = if near.is_empty() {
        nominal_interval
    } else {
        dsp::median_f64(&near)
    };
    if !(spacing.is_finite() && spacing > 1.0) {
        return peaks.to_vec();
    }
    let tol = grid_tolerance(spacing);
    let mut best_count = 0usize;
    let mut best_mag = f64::NEG_INFINITY;
    let mut best_mask = vec![false; lags.len()];
    for &anchor in &lags {
        let mut best_for_k: Vec<(i64, usize, f64)> = Vec::new();
        for (i, &lag) in lags.iter().enumerate() {
            let k = ((lag - anchor) / spacing).round() as i64;
            let pred = anchor + k as f64 * spacing;
            if (lag - pred).abs() > tol {
                continue;
            }
            let mag = peaks[order[i]].mag;
            if let Some(pos) = best_for_k.iter().position(|(kk, _, _)| *kk == k) {
                if mag > best_for_k[pos].2 {
                    best_for_k[pos] = (k, i, mag);
                }
            } else {
                best_for_k.push((k, i, mag));
            }
        }
        // 旁瓣不能当锚点：它自己会被同一格里更强的峰替换，却把 2–3k 外的假峰拉进容差。
        let anchor_kept = best_for_k
            .iter()
            .any(|(k, i, _)| *k == 0 && (lags[*i] - anchor).abs() <= 1e-3);
        if !anchor_kept {
            continue;
        }
        let count = best_for_k.len();
        let mag_sum: f64 = best_for_k.iter().map(|(_, _, m)| *m).sum();
        if count > best_count || (count == best_count && mag_sum > best_mag) {
            best_count = count;
            best_mag = mag_sum;
            best_mask = vec![false; lags.len()];
            for (_, i, _) in &best_for_k {
                best_mask[*i] = true;
            }
        }
    }
    let mut kept: Vec<dsp::CorrPeak> = best_mask
        .iter()
        .enumerate()
        .filter(|(_, keep)| **keep)
        .map(|(i, _)| peaks[order[i]].clone())
        .collect();
    kept.sort_by(|a, b| {
        a.lag
            .partial_cmp(&b.lag)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    kept
}

/// 单遍锚点约束的 onset 容差（样本）。
///
/// 取与逐遍残差门限 CLOCK_RESID_TOL 相同的 500 样本（48 kHz 下约 10.4 ms）：
/// - 足以容纳 --max-ppm 100 ppm 在 8 遍重复内累积的时钟漂移（约 175 样本）；
/// - 远小于 pilot FR-ch01 事故里 88 ms（4236 样本）的墙反射。
/// 窗口外的峰不会被当成干净起点：调用方会把该遍强制标 invalid。
const ANCHOR_ONSET_TOL: f64 = dsp::CLOCK_RESID_TOL;

struct CollectResult {
    hits: Vec<Hit>,
    /// 旧栅格路径剔除的孤立峰数（锚点路径恒为 0）。
    rejected: usize,
    /// 锚点窗口内没有相关峰、只能退回最近峰的第 k 遍（随后强制标 invalid）。
    outside_anchor_window: Vec<usize>,
    /// 连最近峰都切不出有效扫频段的第 k 遍（不进入 hits，不参与聚合）。
    missing_passes: Vec<usize>,
}

fn hit_from_peak(
    capture: &[f64],
    source: &[f64],
    rate: u32,
    start_hz: f64,
    end_hz: f64,
    nominal_interval: f64,
    p: &dsp::CorrPeak,
    k: usize,
) -> Result<Option<Hit>, String> {
    let start = p.lag.floor().max(0.0) as usize;
    if start >= capture.len() {
        return Ok(None);
    }
    // 尾部不能吃到下一遍，否则互相关/相位斜率会锁到后一遍上。
    let gap = (nominal_interval as usize).saturating_sub(source.len());
    let tail = ((rate as usize) / 2).min(gap.saturating_sub(16));
    let end = (start + source.len() + tail).min(capture.len());
    if end <= start + source.len() / 4 {
        return Ok(None);
    }
    let slice = &capture[start..end];
    let result = dsp::analyze_capture(slice, source, rate, start_hz, end_hz, 0, None)?;
    let frac = result.delay_samples - result.delay_coarse_samples as f64;
    let onset = if frac.abs() < 1.5 {
        p.lag + frac
    } else {
        p.lag
    };
    Ok(Some(Hit {
        k,
        onset,
        result_polarity_legacy: result.polarity,
        result,
    }))
}

fn collect_hits(
    capture: &[f64],
    source: &[f64],
    rate: u32,
    start_hz: f64,
    end_hz: f64,
    nominal_interval: f64,
    repeats: usize,
    anchor_hint: Option<f64>,
) -> Result<CollectResult, String> {
    // 选峰前先按扫频通带滤掉低频隆隆。切段和分析仍用原始采集。
    // 间隔留 2000，让相距约 2.7k 的真峰和假峰同时留下，交给栅格/锚点取舍。
    let (band_lo, band_hi) = dsp::onset_band(start_hz, end_hz);
    let cap_f = dsp::bandpass(capture, rate as f64, band_lo, band_hi);
    let src_f = dsp::bandpass(source, rate as f64, band_lo, band_hi);
    let want = repeats.saturating_mul(4).max(8);
    // v1.1 P0-A：不再把带通峰 snap 到“未归一化全带 |c| 的 ±1200”：
    // 全带 |c| 被 20–300 Hz 宽主瓣主导，argmax 会把起点拉晚数百样本
    // （FR 事故 pass1 +706；FL 三遍 +639..670 → 伪 -62.4 ppm）。
    // 带通归一化峰本身已含抛物线亚样本，和单遍 dsp::bandpassed_delay 同源，直接使用。
    // 收簇半径必须小于真峰和隆隆假峰的间距，否则假峰会把真峰吞掉。
    let raw = dsp::correlation_peaks(&cap_f, &src_f, want, 2_000, 0);
    if raw.is_empty() {
        return Err("未找到扫频相关峰".into());
    }
    let clustered = cluster_peaks(&raw, 800.0);

    if let Some(hint) = anchor_hint.filter(|h| h.is_finite()) {
        // measure-multi 已用 bandpassed_delay 得到可靠的单遍起点（anchor_hint），
        // 这里拿它约束逐遍 onset：每遍只在 hint + k*interval ± ANCHOR_ONSET_TOL 内
        // 取归一化相关最强的峰。旧栅格选择会整组锁到每个重复里同相位出现的强反射
        // （pilot FR-ch01：真起点 2188，被 4236 样本/88 ms 的墙反射顶替）。
        // 窗口内没有峰时退回最近峰并记录，调用方会把该遍强制标 invalid，
        // 绝不把窗口外的反射静默当成干净起点。
        let mut hits = Vec::new();
        let mut outside = Vec::new();
        let mut missing = Vec::new();
        for k in 0..repeats {
            let expected = hint + k as f64 * nominal_interval;
            let in_window = clustered
                .iter()
                .filter(|p| (p.lag - expected).abs() <= ANCHOR_ONSET_TOL)
                .max_by(|a, b| {
                    a.mag
                        .partial_cmp(&b.mag)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            if in_window.is_none() {
                outside.push(k);
            }
            let chosen = in_window.or_else(|| {
                clustered.iter().min_by(|a, b| {
                    (a.lag - expected)
                        .abs()
                        .partial_cmp(&(b.lag - expected).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            });
            match chosen {
                Some(p) => {
                    match hit_from_peak(
                        capture,
                        source,
                        rate,
                        start_hz,
                        end_hz,
                        nominal_interval,
                        p,
                        k,
                    )? {
                        Some(h) => hits.push(h),
                        None => missing.push(k),
                    }
                }
                None => missing.push(k),
            }
        }
        if hits.is_empty() {
            return Err(format!(
                "锚点约束下无法切出任何重复扫频段（hint={hint:.1}, tol={ANCHOR_ONSET_TOL:.0}）"
            ));
        }
        return Ok(CollectResult {
            hits,
            rejected: 0,
            outside_anchor_window: outside,
            missing_passes: missing,
        });
    }

    // 无单遍锚点（analyze --repeats 的旧路径）：仍用栅格选峰。
    let peaks = filter_peaks_on_grid(&clustered, nominal_interval);
    let rejected = clustered.len().saturating_sub(peaks.len());
    let peaks = if peaks.is_empty() { clustered } else { peaks };
    let origin = peaks[0].lag;
    let mut best: Vec<Option<(f64, Hit)>> = (0..repeats).map(|_| None).collect();
    for p in &peaks {
        let k = if peaks.len() == 1 {
            0usize
        } else {
            ((p.lag - origin) / nominal_interval).round().max(0.0) as usize
        };
        if k >= repeats {
            continue;
        }
        let Some(hit) = hit_from_peak(
            capture,
            source,
            rate,
            start_hz,
            end_hz,
            nominal_interval,
            p,
            k,
        )?
        else {
            continue;
        };
        let replace = match &best[k] {
            None => true,
            Some((mag, _)) => p.mag > *mag,
        };
        if replace {
            best[k] = Some((p.mag, hit));
        }
    }
    let hits: Vec<Hit> = best.into_iter().flatten().map(|(_, h)| h).collect();
    if hits.is_empty() {
        return Err("相关峰无法切成有效扫频段".into());
    }
    Ok(CollectResult {
        hits,
        rejected,
        outside_anchor_window: vec![],
        missing_passes: vec![],
    })
}

fn analysis_from_hits(
    hits: &[Hit],
    clock: &ClockFit,
    suspect: &[bool],
    nominal_interval: f64,
    rate: u32,
    drift_corrected: bool,
    warnings: Vec<String>,
) -> RepeatAnalysis {
    let slope = if clock.valid {
        clock.slope
    } else {
        nominal_interval
    };
    let mut delay_rel = Vec::new();
    let mut votes = Vec::new();
    let mut confs = Vec::new();
    let mut ests = Vec::new();
    let mut peaks = Vec::new();
    let mut snrs = Vec::new();
    let mut snr_bands = Vec::new();
    let mut snr_band_hz = (0.0f64, 0.0f64);
    let mut invalid = Vec::new();
    let mut frs = Vec::new();
    let mut modal_frs = Vec::new();
    let mut fr_delays = Vec::new();
    let mut legacy = Vec::new();
    let mut listening = Vec::new();
    let mut usable_delay = Vec::new();
    for (i, h) in hits.iter().enumerate() {
        let bad = clock.point_invalid.get(i).copied().unwrap_or(!clock.valid);
        let rel = h.onset - h.k as f64 * slope;
        delay_rel.push(rel);
        let vote = if h.result.polarity_confidence == "unknown" {
            h.result.polarity_listening
        } else {
            h.result.robust_polarity
        };
        votes.push(vote);
        confs.push(h.result.polarity_confidence.clone());
        ests.push(h.result.estimators.clone());
        peaks.push(h.result.peak_idx as i64);
        snrs.push(h.result.snr_db);
        snr_bands.push(h.result.snr_band_db);
        if h.result.snr_band_hz.1 > 0.0 {
            snr_band_hz = h.result.snr_band_hz;
        }
        invalid.push(bad);
        legacy.push(h.result_polarity_legacy);
        listening.push(h.result.polarity_listening);
        if !bad {
            usable_delay.push(rel);
            frs.push(h.result.fr.clone());
            modal_frs.push(h.result.fr_modal.clone());
            fr_delays.push(h.result.delay_samples);
        }
    }
    if usable_delay.is_empty() {
        usable_delay = delay_rel.clone();
        frs = hits.iter().map(|h| h.result.fr.clone()).collect();
        modal_frs = hits.iter().map(|h| h.result.fr_modal.clone()).collect();
        fr_delays = hits.iter().map(|h| h.result.delay_samples).collect();
    }
    let delay = dsp::median_f64(&usable_delay);
    let pairs: Vec<(i32, String)> = votes.iter().copied().zip(confs.iter().cloned()).collect();
    let (robust, confidence) = aggregate_votes(&pairs);
    let listening_pol = dsp::majority_sign(&listening);
    let estimators = PolarityEstimators {
        ir_peak: dsp::majority_sign(&ests.iter().map(|e| e.ir_peak).collect::<Vec<_>>()),
        direct_window: dsp::majority_sign(
            &ests.iter().map(|e| e.direct_window).collect::<Vec<_>>(),
        ),
        phase_vote: dsp::majority_sign(&ests.iter().map(|e| e.phase_vote).collect::<Vec<_>>()),
        lowband: dsp::majority_sign(&ests.iter().map(|e| e.lowband).collect::<Vec<_>>()),
        matched: dsp::majority_sign(&ests.iter().map(|e| e.matched).collect::<Vec<_>>()),
        reference_xcorr: None,
    };
    let fr = dsp::average_fr(&frs, &fr_delays, rate as f64);
    let fr_modal = dsp::average_fr(&modal_frs, &fr_delays, rate as f64);
    let phase_1k = fr
        .iter()
        .min_by(|a, b| {
            (a.freq_hz - 1000.0)
                .abs()
                .partial_cmp(&(b.freq_hz - 1000.0).abs())
                .unwrap()
        })
        .map(|p| p.phase_deg)
        .unwrap_or(0.0);
    let peak_db = dsp::median_f64(&hits.iter().map(|h| h.result.peak_db).collect::<Vec<_>>());
    let snr = dsp::median_f64(&snrs);
    let snr_band = if snr_bands.is_empty() {
        0.0
    } else {
        dsp::median_f64(&snr_bands)
    };
    // 总体 onset_invalid 只表示“存在坏遍/无法估计栅格”，不再等价于整通道不可用；
    // 延迟/FR/极性聚合只看逐遍 invalid（usable_delay/frs 已按 bad 过滤）。
    let onset_invalid = invalid.iter().any(|v| *v) || clock.insufficient_valid_passes;
    let onsets: Vec<f64> = hits.iter().map(|h| h.onset).collect();
    let repeat_diffs: Vec<f64> = onsets.windows(2).map(|w| w[1] - w[0]).collect();
    RepeatAnalysis {
        delay_samples: delay,
        delay_coarse_samples: delay.round() as i64,
        polarity_legacy: dsp::majority_sign(&legacy),
        robust_polarity: robust,
        listening_polarity: listening_pol,
        confidence,
        estimators,
        repeats: RepeatDetail {
            count: hits.len(),
            delay_samples: delay_rel,
            polarity_votes: votes,
            ir_peak_idx: peaks,
            snr_db: snrs,
            per_repeat_confidence: confs,
            per_repeat_estimators: ests,
            onset_invalid: invalid,
            onsets,
            repeat_diffs,
            accepted_diffs: clock.accepted_diffs.clone(),
            rejected_diffs: clock.rejected_diffs.clone(),
            spacing_samples: Some(clock.spacing),
            anchor_samples: Some(clock.anchor),
            residuals: clock.residuals.clone(),
            clock_ppm_source: Some(clock.ppm_source.clone()),
            insufficient_valid_passes: clock.insufficient_valid_passes,
            ir_peak_suspect: suspect.to_vec(),
        },
        clock_ppm: if clock.valid { Some(clock.ppm) } else { None },
        clock_ppm_ok: clock.valid,
        clock_uncertainty_ppm: clock.uncertainty_ppm,
        clock_significant: clock.significant,
        onset_invalid,
        drift_corrected,
        fr,
        fr_modal,
        peak_db,
        phase_1k_deg: phase_1k,
        snr_db: snr,
        snr_band_db: snr_band,
        snr_band_hz,
        warnings,
    }
}

/// 跨遍投票。任一遍 confidence=unknown 的符号不参与；全体不可用 → unknown。
/// 参与者一致且每一遍都是 high → high；否则多数决且 confidence=low。
pub fn aggregate_votes(per: &[(i32, String)]) -> (i32, String) {
    let usable: Vec<i32> = per
        .iter()
        .filter(|(_, c)| c != "unknown")
        .map(|(p, _)| *p)
        .filter(|p| *p != 0)
        .collect();
    if usable.is_empty() {
        return (0, "unknown".into());
    }
    let pos = usable.iter().filter(|v| **v > 0).count();
    let neg = usable.len() - pos;
    let pol = if pos > neg {
        1
    } else if neg > pos {
        -1
    } else {
        0
    };
    if pol == 0 {
        return (0, "unknown".into());
    }
    let unanimous = pos == usable.len() || neg == usable.len();
    let all_high = per
        .iter()
        .filter(|(_, c)| c != "unknown")
        .all(|(_, c)| c == "high");
    if unanimous && all_high {
        (pol, "high".into())
    } else {
        (pol, "low".into())
    }
}

/// A3：跨遍 IR 峰一致性门。每遍的 IR 峰应在切片内同一位置（切片起点=该遍起点）；
/// 任一遍偏离中位数 > 200 样本，说明它锁到了反射/旁瓣（robust-t4 UMIK FR ±1534）。
fn ir_peak_suspects(hits: &[Hit]) -> Vec<bool> {
    if hits.len() < 2 {
        return vec![false; hits.len()];
    }
    let peaks: Vec<f64> = hits.iter().map(|h| h.result.peak_idx as f64).collect();
    let med = dsp::median_f64(&peaks);
    peaks.iter().map(|p| (p - med).abs() > 200.0).collect()
}

/// 先按 IR 峰一致性剔除 suspect 遍，再在剩余遍上做稳健 fit；并把结果展开回全遍 mask。
/// forced_invalid：调用方已确定不可用的遍（例如锚点窗口外），不参与拟合且恒为 invalid。
fn fit_clock_with_ir_gate(
    hits: &[Hit],
    nominal_interval: f64,
    max_ppm: f64,
    anchor_hint: Option<f64>,
    forced_invalid: &[bool],
) -> (dsp::ClockFit, Vec<bool>) {
    let suspect = ir_peak_suspects(hits);
    let forced = |i: usize| forced_invalid.get(i).copied().unwrap_or(false);
    let kept: Vec<usize> = (0..hits.len())
        .filter(|i| !suspect[*i] && !forced(*i))
        .collect();
    // 全部遍都被剔除时仍用全体做一次拟合，让 point_invalid/fallback 如实标记，
    // 而不是在这里假装拟合成功。
    let fit_idx: Vec<usize> = if kept.is_empty() {
        (0..hits.len()).collect()
    } else {
        kept
    };
    let pairs: Vec<(usize, f64)> = fit_idx
        .iter()
        .map(|&i| (hits[i].k, hits[i].onset))
        .collect();
    let mut clock = dsp::fit_clock(&pairs, nominal_interval, max_ppm, anchor_hint);
    let mut invalid = vec![true; hits.len()];
    for (pos, &orig) in fit_idx.iter().enumerate() {
        invalid[orig] = clock.point_invalid.get(pos).copied().unwrap_or(true);
    }
    for i in 0..hits.len() {
        if suspect[i] || forced(i) {
            invalid[i] = true;
        }
    }
    let residuals: Vec<f64> = hits
        .iter()
        .map(|h| h.onset - (clock.anchor + h.k as f64 * clock.spacing))
        .collect();
    if invalid.iter().all(|v| *v) && !hits.is_empty() {
        // 退化保留最接近拟合的 2 遍；强制 invalid 的遍不参与“救回”。
        // 若所有遍都是强制剔除（例如单遍锚点和任何峰都对不上），则不再救回任何遍，
        // 让 insufficient_valid_passes=true 如实暴露证据不足。
        let mut order: Vec<usize> = (0..hits.len()).filter(|i| !forced(*i)).collect();
        if !order.is_empty() {
            order.sort_by(|&a, &b| {
                residuals[a]
                    .abs()
                    .partial_cmp(&residuals[b].abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let keep: Vec<usize> = order.into_iter().take(2.min(hits.len())).collect();
            invalid = (0..hits.len()).map(|k| !keep.contains(&k)).collect();
            clock.fallback_kept_closest_two = true;
        }
    }
    clock.point_invalid = invalid;
    clock.residuals = residuals.clone();
    let valid_idx: Vec<usize> = (0..hits.len())
        .filter(|k| !clock.point_invalid[*k])
        .collect();
    // 2 遍且没有 accepted 相邻差、也没有单遍 anchor_hint：无法判断哪一遍坏 → 证据不足。
    let ambiguous_two_pass =
        hits.len() == 2 && clock.accepted_diffs.is_empty() && anchor_hint.is_none();
    clock.insufficient_valid_passes = valid_idx.len() < 2 || ambiguous_two_pass;
    if clock.valid && valid_idx.len() >= 2 {
        let kmin = valid_idx.iter().map(|&i| hits[i].k).min().unwrap();
        let kmax = valid_idx.iter().map(|&i| hits[i].k).max().unwrap();
        let span = (kmax - kmin) as f64 * clock.spacing;
        let sse: f64 = valid_idx.iter().map(|&i| residuals[i] * residuals[i]).sum();
        let dof = (valid_idx.len() as f64 - 2.0).max(1.0);
        if span > 1.0 {
            clock.uncertainty_ppm = 1e6 * (sse / dof).sqrt() / span;
            clock.significant = clock.ppm.abs() > 2.0 * clock.uncertainty_ppm;
        }
    } else {
        clock.significant = false;
    }
    (clock, suspect)
}

pub fn analyze_repeats(
    capture: &[f64],
    source: &[f64],
    rate: u32,
    start_hz: f64,
    end_hz: f64,
    nominal_interval: f64,
    repeats: usize,
    drift_correct: bool,
    max_ppm: f64,
    // 单遍 actual_start - rep_lo（样本）；仅当 accepted 相邻差不足时用作抗坏点锚点。
    anchor_hint: Option<f64>,
) -> Result<RepeatAnalysis, String> {
    if repeats == 0 {
        return Err("repeats=0".into());
    }
    if nominal_interval <= 0.0 {
        return Err("repeat interval <= 0".into());
    }
    let CollectResult {
        hits,
        rejected,
        outside_anchor_window,
        missing_passes,
    } = collect_hits(
        capture,
        source,
        rate,
        start_hz,
        end_hz,
        nominal_interval,
        repeats,
        anchor_hint,
    )?;
    let forced: Vec<bool> = (0..hits.len())
        .map(|i| outside_anchor_window.contains(&hits[i].k))
        .collect();
    let (clock, suspect) =
        fit_clock_with_ir_gate(&hits, nominal_interval, max_ppm, anchor_hint, &forced);
    let mut warnings = collect_warnings(
        &clock,
        hits.len(),
        rejected,
        &missing_passes,
        &outside_anchor_window,
        repeats,
        max_ppm,
    );
    // A2：--drift-correct 只在漂移显著（|ppm| > 2σ）时启用，否则把噪声当漂移重采样。
    if drift_correct && !(clock.valid && clock.significant) {
        warnings.push(format!(
            "clock_ppm 不显著（ppm={:.2}, ±{:.2} ppm, |ppm|<=2σ）→ 跳过 --drift-correct，保留未重采样分析",
            clock.ppm, clock.uncertainty_ppm
        ));
    }
    if !(drift_correct && clock.valid && clock.significant) {
        return Ok(finish_analysis(
            &hits,
            &clock,
            &suspect,
            nominal_interval,
            rate,
            false,
            warnings,
            &missing_passes,
            &outside_anchor_window,
        ));
    }
    let stretch = nominal_interval / clock.slope;
    let corrected = dsp::resample_sinc(capture, stretch);
    let CollectResult {
        hits: hits2,
        outside_anchor_window: outside2,
        missing_passes: missing2,
        ..
    } = collect_hits(
        &corrected,
        source,
        rate,
        start_hz,
        end_hz,
        nominal_interval,
        repeats,
        anchor_hint,
    )?;
    let forced2: Vec<bool> = (0..hits2.len())
        .map(|i| outside2.contains(&hits2[i].k))
        .collect();
    let (mut clock2, suspect2) =
        fit_clock_with_ir_gate(&hits2, nominal_interval, max_ppm, anchor_hint, &forced2);
    // 报告校正前的 ppm/不确定度；校正后的拟合只用于延迟。
    clock2.ppm = clock.ppm;
    clock2.valid = clock.valid;
    clock2.significant = clock.significant;
    clock2.uncertainty_ppm = clock.uncertainty_ppm;
    warnings.push(format!(
        "已按 {:.2} ppm（±{:.2}, 显著）重采样后再分析",
        clock.ppm, clock.uncertainty_ppm
    ));
    Ok(finish_analysis(
        &hits2,
        &clock2,
        &suspect2,
        nominal_interval,
        rate,
        true,
        warnings,
        &missing2,
        &outside2,
    ))
}

/// 聚合前的 warning 汇总（clock/重复遍/锚点这三类诊断都写进 analysis 的 warnings）。
#[allow(clippy::too_many_arguments)]
fn collect_warnings(
    clock: &dsp::ClockFit,
    hits_len: usize,
    rejected: usize,
    missing_passes: &[usize],
    outside_anchor_window: &[usize],
    repeats: usize,
    max_ppm: f64,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if !outside_anchor_window.is_empty() {
        warnings.push(format!(
            "第 {:?} 遍在单遍锚点 ±{:.0} 样本窗口内没有相关峰，退回最近峰并标 invalid（不参与延迟/ppm 聚合）",
            outside_anchor_window, ANCHOR_ONSET_TOL
        ));
    }
    if !missing_passes.is_empty() {
        warnings.push(format!(
            "第 {:?} 遍无法切出扫频段（不参与延迟/ppm 聚合）",
            missing_passes
        ));
    }
    if rejected > 0 && hits_len < repeats {
        warnings.push(format!(
            "栅格校验剔除 {rejected} 个孤立起点（偏差 >5% 或 >2000 样本），剩下 {hits_len}/{repeats} 遍"
        ));
    } else if hits_len < repeats && missing_passes.is_empty() {
        warnings.push(format!("只找到 {hits_len}/{repeats} 遍扫频"));
    }
    let valid_starts = clock.point_invalid.iter().filter(|v| !**v).count();
    if clock.fallback_kept_closest_two {
        warnings.push("所有遍残差都超门限，退化保留最接近拟合的 2 遍（clock_ppm 不估）".into());
    }
    if clock.insufficient_valid_passes {
        warnings.push(format!(
            "有效起点只有 {valid_starts} 个（<2），不估计 clock_ppm；延迟取可用遍中位数"
        ));
    } else if !clock.valid {
        warnings.push(format!(
            "clock_ppm 不可估/超限：accepted 相邻差 {} 个（需 >=2），ppm_raw={:.2}（上限 {:.1}），spacing={:.2}；通道仍可用，延迟取可用遍中位数",
            clock.accepted_diffs.len(),
            clock.ppm,
            max_ppm,
            clock.spacing
        ));
    }
    if clock.point_invalid.iter().any(|v| *v) && valid_starts >= 2 {
        warnings.push(format!(
            "部分重复起点偏离拟合（残差 >{:.0} 样本），只从延迟/ppm 聚合中剔除坏遍",
            dsp::CLOCK_RESID_TOL
        ));
    }
    warnings
}

#[allow(clippy::too_many_arguments)]
fn finish_analysis(
    hits: &[Hit],
    clock: &dsp::ClockFit,
    suspect: &[bool],
    nominal_interval: f64,
    rate: u32,
    drift_corrected: bool,
    warnings: Vec<String>,
    missing_passes: &[usize],
    outside_anchor_window: &[usize],
) -> RepeatAnalysis {
    let mut ra = analysis_from_hits(
        hits,
        clock,
        suspect,
        nominal_interval,
        rate,
        drift_corrected,
        warnings,
    );
    if !missing_passes.is_empty() {
        // 少一遍就拿不到完整重复证据：整体 onset 标 invalid，merge 的延迟质量门会看见。
        ra.onset_invalid = true;
        ra.repeats.insufficient_valid_passes |= hits.len() < 2;
        ra.warnings.push(format!(
            "锚点约束下缺少第 {:?} 遍可用扫频段，onset_invalid=true",
            missing_passes
        ));
    }
    if !outside_anchor_window.is_empty() {
        ra.onset_invalid = true;
    }
    ra
}

#[cfg(test)]
pub fn synth_sweep(rate: u32, seconds: f64, start_hz: f64, end_hz: f64) -> Vec<f64> {
    let spec = SweepSpec {
        sample_rate: rate,
        start_hz,
        end_hz,
        seconds,
        level_db: -20.0,
        fade_ms: 5.0,
    };
    spec.generate()
}

/// 把同一段扫频按标称间隔重复放置，并在整体上做时钟拉伸（stretch>1 拉长）。
#[cfg(test)]
pub fn repeat_capture(
    source: &[f64],
    repeats: usize,
    interval: usize,
    delay: usize,
    invert_repeat: Option<usize>,
    stretch: f64,
    noise_amp: f64,
) -> Vec<f64> {
    let tail = interval.saturating_sub(source.len()) + source.len() / 5 + 64;
    let n = delay + repeats * interval + tail;
    let mut x = vec![0.0f64; n];
    for k in 0..repeats {
        let off = delay + k * interval;
        let sign = if invert_repeat == Some(k) { -1.0 } else { 1.0 };
        for (i, &s) in source.iter().enumerate() {
            if off + i < x.len() {
                x[off + i] += sign * s;
            }
        }
    }
    if noise_amp > 0.0 {
        let mut s = 0x1234_5678u32;
        for v in x.iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let u = ((s >> 8) as f64) / ((1u32 << 24) as f64);
            *v += (u * 2.0 - 1.0) * noise_amp;
        }
    }
    if (stretch - 1.0).abs() < 1e-15 {
        x
    } else {
        dsp::resample_sinc(&x, stretch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(got: f64, expect: f64, tol: f64, what: &str) {
        assert!(
            (got - expect).abs() <= tol,
            "{what}: got {got} expect {expect} tol {tol}"
        );
    }

    #[test]
    fn repeated_identical_is_high_and_median_delay() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.45, 20.0, 16_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 37usize;
        let cap = repeat_capture(&src, 3, interval, delay, None, 1.0, 1.0e-5);
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            16_000.0,
            interval as f64,
            3,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert_eq!(
            agg.confidence, "high",
            "estimators={:?} warnings={:?}",
            agg.estimators, agg.warnings
        );
        assert_eq!(agg.robust_polarity, 1);
        assert_eq!(agg.estimators.ir_peak, 1);
        assert_eq!(agg.estimators.direct_window, 1);
        assert_eq!(agg.estimators.lowband, 1);
        assert_close(agg.delay_samples, delay as f64, 0.75, "median delay");
        assert!(
            agg.clock_ppm.unwrap().abs() < 5.0,
            "ppm={}",
            agg.clock_ppm.unwrap()
        );
        assert!(agg.clock_ppm_ok);
    }

    #[test]
    fn one_inverted_repeat_majority_low() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.45, 20.0, 16_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let cap = repeat_capture(&src, 3, interval, 40, Some(1), 1.0, 1.0e-5);
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            16_000.0,
            interval as f64,
            3,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert_eq!(
            agg.robust_polarity, 1,
            "votes={:?}",
            agg.repeats.polarity_votes
        );
        assert_eq!(
            agg.confidence, "low",
            "conf={:?}",
            agg.repeats.per_repeat_confidence
        );
        let neg = agg
            .repeats
            .polarity_votes
            .iter()
            .filter(|v| **v < 0)
            .count();
        assert_eq!(neg, 1);
    }

    #[test]
    fn clock_ppm_within_2_and_drift_correct_delay() {
        let rate = 48_000u32;
        // 4 kHz 上限，让线性相位/sinc 重采样不被超声段带偏。
        let src = synth_sweep(rate, 0.80, 20.0, 4_000.0);
        let interval = src.len() + (0.70 * rate as f64) as usize;
        let delay = 50usize;
        for ppm in [20.0, 50.0] {
            let stretch = 1.0 + ppm / 1.0e6;
            let cap = repeat_capture(&src, 3, interval, delay, None, stretch, 0.0);
            let raw = analyze_repeats(
                &cap,
                &src,
                rate,
                20.0,
                4_000.0,
                interval as f64,
                3,
                false,
                100.0,
                None,
            )
            .unwrap();
            let got = raw.clock_ppm.unwrap();
            assert_close(got, ppm, 2.0, &format!("ppm {ppm}"));
            assert!(raw.clock_ppm_ok, "ppm {got} should be ok");
            let fixed = analyze_repeats(
                &cap,
                &src,
                rate,
                20.0,
                4_000.0,
                interval as f64,
                3,
                true,
                100.0,
                None,
            )
            .unwrap();
            assert!(fixed.drift_corrected);
            assert_close(
                fixed.delay_samples,
                delay as f64,
                0.5,
                &format!("corrected delay ppm {ppm}"),
            );
        }
    }

    #[test]
    fn sub_band_is_not_a_committed_sign() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.50, 20.0, 130.0);
        let mut cap = vec![0.0; 80];
        cap.extend_from_slice(&src);
        // 长振铃：60 Hz 附近的衰减正弦叠在直达声后面。
        let mut y = cap.clone();
        let mut s1 = 0.0;
        let mut s2 = 0.0;
        let w = 2.0 * std::f64::consts::PI * 60.0 / rate as f64;
        for i in 0..y.len() {
            let x = cap[i];
            let s = 1.98 * w.cos() * s1 - 0.98 * s2 + x;
            s2 = s1;
            s1 = s;
            y[i] = s;
        }
        let r = dsp::analyze_capture(&y, &src, rate, 20.0, 130.0, 0, None).unwrap();
        assert_eq!(r.polarity_confidence, "unknown");
        assert_eq!(r.robust_polarity, 0);
        assert_ne!(r.polarity_confidence, "high");
    }

    #[test]
    fn reference_and_main_delays() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 12_000.0);
        let interval = src.len() + (0.15 * rate as f64) as usize;
        let main = repeat_capture(&src, 2, interval, 30, None, 1.0, 1e-6);
        let reference = repeat_capture(&src, 2, interval, 45, None, 1.0, 1e-6);
        let a = analyze_repeats(
            &main,
            &src,
            rate,
            20.0,
            12_000.0,
            interval as f64,
            2,
            false,
            100.0,
            None,
        )
        .unwrap();
        let b = analyze_repeats(
            &reference,
            &src,
            rate,
            20.0,
            12_000.0,
            interval as f64,
            2,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert_close(a.delay_samples, 30.0, 0.75, "main");
        assert_close(b.delay_samples, 45.0, 0.75, "ref");
        assert_eq!(dsp::relative_polarity(&main, &reference), 1);
        let mut flipped = reference.clone();
        for v in flipped.iter_mut() {
            *v = -*v;
        }
        assert_eq!(dsp::relative_polarity(&main, &flipped), -1);
    }

    #[test]
    fn loud_low_frequency_burst_does_not_steal_the_grid() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.45, 20.0, 8_000.0);
        let gap = (0.25 * rate as f64) as usize;
        let interval = src.len() + gap;
        let delay = 3_000usize;
        let mut cap = repeat_capture(&src, 3, interval, delay, None, 1.0, 0.0);
        // 扫频开头 90 ms 落在 20–70 Hz。放大 18 dB 塞进间隔里，绝对相关会压过真扫频。
        let burst = (0.09 * rate as f64) as usize;
        let low = dsp::bandpass(&src, rate as f64, 20.0, 55.0);
        let amp = 10.0f64.powf(18.0 / 20.0);
        let fake_at = delay + src.len() + 1_500;
        for i in 0..burst.min(low.len()) {
            if fake_at + i < cap.len() {
                cap[fake_at + i] += amp * low[i];
            }
        }
        for (i, v) in cap.iter_mut().enumerate() {
            let t = i as f64 / rate as f64;
            *v += 0.04 * (2.0 * std::f64::consts::PI * 40.0 * t).sin();
        }
        let curve = dsp::correlation_curve(&cap, &src);
        let mut raw_at = 0usize;
        let mut raw_mag = 0.0f64;
        for (i, v) in curve.iter().take(cap.len()).enumerate() {
            if v.abs() > raw_mag {
                raw_mag = v.abs();
                raw_at = i;
            }
        }
        assert!(
            (raw_at as f64 - fake_at as f64).abs() < 800.0,
            "旧的绝对相关应锁到假峰 {fake_at}，实际 {raw_at}"
        );
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            3,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert_eq!(agg.repeats.count, 3, "warnings={:?}", agg.warnings);
        assert_close(agg.delay_samples, delay as f64, 0.75, "real grid delay");
        assert!(
            agg.clock_ppm.unwrap().abs() < 5.0,
            "ppm={} warnings={:?}",
            agg.clock_ppm.unwrap(),
            agg.warnings
        );
        assert!(agg.clock_ppm_ok, "warnings={:?}", agg.warnings);
        let worst = agg
            .repeats
            .delay_samples
            .iter()
            .map(|d| (*d - delay as f64).abs())
            .fold(0.0f64, f64::max);
        assert!(
            worst < 1.5,
            "per-repeat delays {:?} far from {delay}",
            agg.repeats.delay_samples
        );
    }

    #[test]
    fn grid_filter_rejects_peak_shifted_by_3000() {
        let nominal = 20_000.0;
        let peaks = vec![
            dsp::CorrPeak {
                lag: 1_000.0,
                signed: 10.0,
                mag: 0.8,
            },
            dsp::CorrPeak {
                lag: 1_000.0 + nominal,
                signed: 10.0,
                mag: 0.8,
            },
            dsp::CorrPeak {
                lag: 1_000.0 + 2.0 * nominal + 3_000.0,
                signed: 30.0,
                mag: 0.99,
            },
        ];
        let kept = filter_peaks_on_grid(&peaks, nominal);
        assert_eq!(
            kept.len(),
            2,
            "kept={:?}",
            kept.iter().map(|p| p.lag).collect::<Vec<_>>()
        );
        assert!(kept
            .iter()
            .all(|p| (p.lag - (1_000.0 + 2.0 * nominal + 3_000.0)).abs() > 100.0));
    }

    #[test]
    fn sidelobe_cannot_pull_false_peak_onto_the_grid() {
        // robust-t4 参考麦：真起点 7861，隆隆假峰 10741，第二遍真峰 257467
        // 旁边还有一个晚 1085 样本的旁瓣。旁瓣当锚点时，假峰残差被压进 2000。
        let nominal = 249_600.0;
        let peaks = vec![
            dsp::CorrPeak {
                lag: 7_860.7,
                signed: 1.0,
                mag: 0.23,
            },
            dsp::CorrPeak {
                lag: 10_740.8,
                signed: -1.0,
                mag: 0.60,
            },
            dsp::CorrPeak {
                lag: 12_225.2,
                signed: 1.0,
                mag: 0.20,
            },
            dsp::CorrPeak {
                lag: 257_467.1,
                signed: 1.0,
                mag: 0.76,
            },
            dsp::CorrPeak {
                lag: 258_551.8,
                signed: 1.0,
                mag: 0.20,
            },
            dsp::CorrPeak {
                lag: 507_067.1,
                signed: 1.0,
                mag: 0.76,
            },
            dsp::CorrPeak {
                lag: 508_151.8,
                signed: 1.0,
                mag: 0.20,
            },
        ];
        let kept = filter_peaks_on_grid(&peaks, nominal);
        let lags: Vec<f64> = kept.iter().map(|p| p.lag).collect();
        assert_eq!(kept.len(), 3, "kept={lags:?}");
        assert!((lags[0] - 7_860.7).abs() < 1.0, "kept={lags:?}");
        assert!((lags[1] - 257_467.1).abs() < 1.0, "kept={lags:?}");
        assert!((lags[2] - 507_067.1).abs() < 1.0, "kept={lags:?}");
    }

    #[test]
    fn shifted_repeat_is_dropped_and_two_points_do_not_estimate_ppm() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 200usize;
        let mut cap = vec![0.0; delay + 3 * interval + 4_000];
        for k in 0..3 {
            let off = delay + k * interval + if k == 2 { 3_000 } else { 0 };
            for (i, &s) in src.iter().enumerate() {
                if off + i < cap.len() {
                    cap[off + i] += s;
                }
            }
        }
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            3,
            false,
            100.0,
            None,
        )
        .unwrap();
        // 栅格过滤可能直接把 +3000 的孤立峰剔除（count=2），也可能保留但标 invalid（count=3）；
        // 两种都允许，但绝不许可“坏点仍被当成 clean 起点”。
        assert!(
            agg.repeats.count == 3 || agg.repeats.count == 2,
            "count={} warnings={:?}",
            agg.repeats.count,
            agg.warnings
        );
        if agg.repeats.count == 3 {
            assert_eq!(
                agg.repeats.onset_invalid,
                vec![false, false, true],
                "bad pass must be marked individually, delays={:?}",
                agg.repeats.delay_samples
            );
        } else {
            assert!(
                agg.repeats.onset_invalid.iter().all(|v| !*v),
                "dropped pass must not appear in onset_invalid"
            );
        }
        assert_close(agg.delay_samples, delay as f64, 1.0, "good-pass delay");
        assert!(
            agg.clock_ppm.is_none() || !agg.clock_ppm_ok,
            "ppm={:?} ok={}",
            agg.clock_ppm,
            agg.clock_ppm_ok
        );
        assert!(
            agg.warnings
                .iter()
                .any(|w| w.contains("clock_ppm") || w.contains("残差")),
            "warnings={:?}",
            agg.warnings
        );
    }

    fn hit_with_peak(k: usize, peak_idx: usize) -> Hit {
        Hit {
            k,
            onset: 0.0,
            result_polarity_legacy: 1,
            result: dsp::DeconvResult {
                delay_samples: 0.0,
                delay_coarse_samples: 0,
                polarity: 1,
                peak_idx,
                robust_polarity: 1,
                polarity_listening: 1,
                polarity_confidence: "low".into(),
                estimators: crate::model::PolarityEstimators {
                    ir_peak: 1,
                    direct_window: 1,
                    phase_vote: 1,
                    lowband: 1,
                    matched: 1,
                    reference_xcorr: None,
                },
                peak_db: 0.0,
                phase_1k_deg: 0.0,
                snr_db: 30.0,
                snr_band_db: 20.0,
                snr_band_hz: (20.0, 130.0),
                fr: vec![],
                fr_modal: vec![],
                impulse: vec![],
            },
        }
    }

    /// robust-t4 记录峰：FR-ch01 交替 238457/239990/238457（中遍偏 1533），
    /// REF-ch00 第一遍 237119 相对后两遍 239993 偏 2874。偏离中位数 >200 标 suspect。
    #[test]
    fn ir_peak_offset_over_200_is_suspect() {
        let fr = vec![
            hit_with_peak(0, 238_457),
            hit_with_peak(1, 239_990),
            hit_with_peak(2, 238_457),
        ];
        assert_eq!(ir_peak_suspects(&fr), vec![false, true, false]);
        let refr = vec![
            hit_with_peak(0, 237_119),
            hit_with_peak(1, 239_993),
            hit_with_peak(2, 239_993),
        ];
        assert_eq!(ir_peak_suspects(&refr), vec![true, false, false]);
    }

    /// FR 事故回归：第 1 遍物理偏 +706 样本，后两遍正常。
    /// 期望：count=3、只第 1 遍 invalid、ppm 不估、延迟取后两遍中位数。
    #[test]
    fn first_repeat_shifted_706_is_marked_invalid_only() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 200usize;
        let mut cap = vec![0.0; delay + 3 * interval + 4_000];
        for k in 0..3 {
            let off = delay + k * interval + if k == 0 { 706 } else { 0 };
            for (i, &s) in src.iter().enumerate() {
                if off + i < cap.len() {
                    cap[off + i] += s;
                }
            }
        }
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            3,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert_eq!(agg.repeats.count, 3);
        assert_eq!(agg.repeats.onset_invalid, vec![true, false, false]);
        assert!(agg.clock_ppm.is_none(), "ppm={:?}", agg.clock_ppm);
        assert_close(
            agg.delay_samples,
            delay as f64,
            1.0,
            "two good passes delay",
        );
    }

    /// 2 遍 + 第 1 遍坏：没有第三个点，必须用 anchor_hint 判断哪遍坏。
    #[test]
    fn two_passes_one_bad_needs_anchor_hint() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 200usize;
        let mut cap = vec![0.0; delay + 2 * interval + 4_000];
        for k in 0..2 {
            let off = delay + k * interval + if k == 0 { 706 } else { 0 };
            for (i, &s) in src.iter().enumerate() {
                if off + i < cap.len() {
                    cap[off + i] += s;
                }
            }
        }
        let with_hint = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            2,
            false,
            100.0,
            Some(delay as f64),
        )
        .unwrap();
        assert_eq!(with_hint.repeats.count, 2);
        assert_eq!(with_hint.repeats.onset_invalid, vec![true, false]);
        assert!(with_hint.clock_ppm.is_none());
        assert_close(
            with_hint.delay_samples,
            delay as f64,
            1.0,
            "hint keeps good pass",
        );
        let no_hint = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            2,
            false,
            100.0,
            None,
        )
        .unwrap();
        assert!(no_hint.clock_ppm.is_none());
        assert!(
            no_hint.repeats.insufficient_valid_passes
                || no_hint
                    .repeats
                    .onset_invalid
                    .iter()
                    .filter(|v| !**v)
                    .count()
                    <= 1,
            "without hint the two-pass ambiguity must not fake a clean fit"
        );
    }

    /// pilot FR-ch01 回归：每个重复里 88 ms（4236 样本）处有一个同幅墙反射。
    /// 旧栅格选择会整组锁到反射上（analysis 里 delay 偏离真值 4236 样本）；
    /// 单遍 anchor_hint 约束后必须逐遍选回真起点。
    #[test]
    fn anchor_hint_rejects_88ms_reflection_grid() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let gap = (0.20 * rate as f64) as usize;
        let interval = src.len() + gap;
        let delay = 200usize;
        let reflection = 4_236usize; // 88 ms @ 48 kHz
        let repeats = 3usize;
        let n = delay + repeats * interval + reflection + src.len() + 4_000;
        let mut cap = vec![0.0f64; n];
        for k in 0..repeats {
            let off = delay + k * interval;
            for (i, &s) in src.iter().enumerate() {
                if off + i < n {
                    cap[off + i] += 0.35 * s; // 直达声被反射压过（房间过阻尼/低频抵消）
                }
                if off + reflection + i < n {
                    cap[off + reflection + i] += s; // 反射更强且逐遍同相位
                }
            }
        }
        let with_hint = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            repeats,
            false,
            100.0,
            Some(delay as f64),
        )
        .unwrap();
        assert_eq!(
            with_hint.repeats.count, 3,
            "warnings={:?}",
            with_hint.warnings
        );
        assert!(
            with_hint.repeats.onset_invalid.iter().all(|v| !*v),
            "anchored passes must all be clean: {:?} warnings={:?}",
            with_hint.repeats.onset_invalid,
            with_hint.warnings
        );
        assert_close(
            with_hint.delay_samples,
            delay as f64,
            1.0,
            "anchored delay must be the direct onset",
        );
        // 锚点约束下逐遍 onset 应保持同一个相对延迟（不把反射的 4236 带进聚合）。
        let worst = with_hint
            .repeats
            .delay_samples
            .iter()
            .map(|d| (*d - delay as f64).abs())
            .fold(0.0f64, f64::max);
        assert!(
            worst < 1.5,
            "per-repeat delays {:?} far from {delay}",
            with_hint.repeats.delay_samples
        );
        // 无锚点的旧路径在同样数据上会（至少）不再干净：要么锁到反射、要么标 invalid，
        // 绝不能给出“干净且正确”的假象。
        let no_hint = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            repeats,
            false,
            100.0,
            None,
        )
        .unwrap();
        let no_hint_locked = (no_hint.delay_samples - (delay + reflection) as f64).abs() < 800.0;
        assert!(
            no_hint_locked,
            "no-hint grid path must reproduce the pilot FR-ch01 reflection lock (delay≈{}); got delay={} onset_invalid={} warnings={:?}",
            delay + reflection,
            no_hint.delay_samples,
            no_hint.onset_invalid,
            no_hint.warnings
        );
    }

    /// 单遍锚点和所有相关峰都对不上时，必须全部标 invalid/证据不足，不得静默给干净延迟。
    #[test]
    fn anchor_window_miss_is_not_silently_clean() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 200usize;
        let cap = repeat_capture(&src, 3, interval, delay, None, 1.0, 0.0);
        // 锚点偏 3000 样本：窗口内没有任何峰。
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            3,
            false,
            100.0,
            Some((delay + 3_000) as f64),
        )
        .unwrap();
        assert!(
            agg.onset_invalid || agg.repeats.insufficient_valid_passes,
            "out-of-window anchor must not look clean: onset_invalid={} insufficient={} warnings={:?}",
            agg.onset_invalid,
            agg.repeats.insufficient_valid_passes,
            agg.warnings
        );
        assert!(
            agg.warnings.iter().any(|w| w.contains("窗口")),
            "warnings={:?}",
            agg.warnings
        );
    }

    /// 锚点窗口外的单遍（例如物理偏 3000 样本）必须强制标 invalid，其它遍仍可算延迟。
    #[test]
    fn anchor_hint_marks_out_of_window_pass_invalid() {
        let rate = 48_000u32;
        let src = synth_sweep(rate, 0.40, 20.0, 8_000.0);
        let interval = src.len() + (0.20 * rate as f64) as usize;
        let delay = 200usize;
        let mut cap = vec![0.0; delay + 3 * interval + 4_000];
        for k in 0..3 {
            let off = delay + k * interval + if k == 2 { 3_000 } else { 0 };
            for (i, &s) in src.iter().enumerate() {
                if off + i < cap.len() {
                    cap[off + i] += s;
                }
            }
        }
        let agg = analyze_repeats(
            &cap,
            &src,
            rate,
            20.0,
            8_000.0,
            interval as f64,
            3,
            false,
            100.0,
            Some(delay as f64),
        )
        .unwrap();
        assert_eq!(agg.repeats.count, 3);
        assert_eq!(
            agg.repeats.onset_invalid,
            vec![false, false, true],
            "out-of-window pass must be forced invalid; warnings={:?}",
            agg.warnings
        );
        assert_close(agg.delay_samples, delay as f64, 1.0, "two good passes");
    }
}
