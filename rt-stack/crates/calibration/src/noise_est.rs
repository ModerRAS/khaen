//! 稳健噪声 / SNR 估计（meas-fix：T3 remeasure SNR 断崖的测量学修复）。
//!
//! 背景（evidence/calib-chain-20261121/g0-g1-20261121/snr-cliff.json）：T3 apply 后
//! remeasure 的 SNR 从 15.99 掉到 0.98 dB，原因是 apply-host 退出时 MOTU 流切换在采集
//! 开头 t≈6 ms 产生一个 pop（峰值 −31.57 dBFS，比扫频还高 9.9 dB）：
//! 1. capture::seed_from_stream 把 seed 取了负号，compute_noise_floor 的窗口被
//!    clamp 到 [0, 0.308 s)，正好吃到 pop；
//! 2. stage_backend::compute_snr_db 用固定的 "前静音" 窗 [0, seed+pre_frames)，同样含 pop。
//!
//! 本模块给出**不依赖固定前静音**的估计器：
//! - onset：分块 RMS 能量门 + 连续 hold（以 seed + pre_frames 为**搜索锚点**，
//!   而不是把锚点当真实 onset），检出真实扫频起点；
//! - 噪声：全采集的分块 RMS 中位数（**排除信号块与瞬态块**，再剔除最高
//!   noise_trim_top_fraction 分位），pop / 流切换瞬态不进噪声；post-sweep 尾段
//!   自然包含在候选块里；
//! - 信号：真实 onset + 单遍扫频中部（两侧各 trim signal_trim_fraction）；
//! - SNR = 信号块 RMS − 稳健噪声 RMS。
//!
//! 纯函数、无设备依赖；T62 compute_noise_floor 与 stage backend compute_snr_db
//! 共用。合成单测见 crates/calibration/tests/meas_fix/robust_snr.rs（**synthetic**，
//! 不是真实板端证据）。

/// 估计器 schema（写进测量 summary 证据，冻结口径）。
pub const ROBUST_SNR_SCHEMA: &str = "tspi-robust-snr/v1";

/// dBFS 下限（0 幅度）。
pub const SILENCE_DBFS: f64 = -200.0;

/// 缺省分块时长（秒；48 kHz 下 480 帧 ≈ 10 ms）。
pub const DEFAULT_BLOCK_SECONDS: f64 = 0.010;

/// 稳健估计参数（全部可在配置/证据中冻结）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RobustSnrConfig {
    /// 分块时长（秒；0.002..=0.050）。
    pub block_seconds: f64,
    /// onset 能量门：高于低分位噪声参考多少 dB 视为信号（3..=30）。
    pub onset_margin_db: f64,
    /// onset 需要连续超门的分块数（1..=20）。
    pub onset_hold_blocks: u32,
    /// 以期望 onset 为锚点向前搜索的秒数（0.1..=5.0）。
    pub search_back_seconds: f64,
    /// 以期望 onset 为锚点向后搜索的秒数（0.1..=8.0；aplay 启动晚于时基估计时需要）。
    pub search_forward_seconds: f64,
    /// 采集开头瞬态保护窗（秒；0.0..=2.0）。apply-host 退出后的流切换 pop 落在
    /// 这里，直接不进 onset 搜索/噪声候选。
    pub transient_guard_seconds: f64,
    /// 信号块排除余量（dB；高于稳健噪声参考多少 dB 的块视为信号块）。
    pub signal_exclude_margin_db: f64,
    /// 噪声候选块剔除最高分位比例（0.0..=0.25）。
    pub noise_trim_top_fraction: f64,
    /// 信号窗两侧各 trim 单遍扫频的比例（0.05..=0.45）。
    pub signal_trim_fraction: f64,
    /// 噪声候选块最少数量（1..=1000）。
    pub min_noise_blocks: u32,
    /// 瞬态告警门（dB；采集开头最大块 RMS 高于稳健噪声多少 dB 记 transient_rejected）。
    pub transient_alert_db: f64,
}

impl Default for RobustSnrConfig {
    fn default() -> Self {
        RobustSnrConfig {
            block_seconds: DEFAULT_BLOCK_SECONDS,
            onset_margin_db: 10.0,
            onset_hold_blocks: 3,
            search_back_seconds: 1.0,
            search_forward_seconds: 2.5,
            transient_guard_seconds: 0.35,
            signal_exclude_margin_db: 8.0,
            noise_trim_top_fraction: 0.05,
            signal_trim_fraction: 0.25,
            min_noise_blocks: 8,
            transient_alert_db: 20.0,
        }
    }
}

impl RobustSnrConfig {
    /// 严格校验（越界拒绝，不静默降级）。
    pub fn validate(&self) -> Result<(), String> {
        let finite = |value: f64| value.is_finite();
        if !finite(self.block_seconds) || !(0.002..=0.050).contains(&self.block_seconds) {
            return Err("block_seconds must be finite and in 0.002..=0.050".to_string());
        }
        if !finite(self.onset_margin_db) || !(3.0..=30.0).contains(&self.onset_margin_db) {
            return Err("onset_margin_db must be in 3..=30".to_string());
        }
        if !(1..=20).contains(&self.onset_hold_blocks) {
            return Err("onset_hold_blocks must be in 1..=20".to_string());
        }
        if !finite(self.search_back_seconds) || !(0.1..=5.0).contains(&self.search_back_seconds) {
            return Err("search_back_seconds must be in 0.1..=5.0".to_string());
        }
        if !finite(self.search_forward_seconds)
            || !(0.1..=8.0).contains(&self.search_forward_seconds)
        {
            return Err("search_forward_seconds must be in 0.1..=8.0".to_string());
        }
        if !finite(self.transient_guard_seconds)
            || !(0.0..=2.0).contains(&self.transient_guard_seconds)
        {
            return Err("transient_guard_seconds must be in 0.0..=2.0".to_string());
        }
        if !finite(self.signal_exclude_margin_db)
            || !(3.0..=30.0).contains(&self.signal_exclude_margin_db)
        {
            return Err("signal_exclude_margin_db must be in 3..=30".to_string());
        }
        if !finite(self.noise_trim_top_fraction)
            || !(0.0..=0.25).contains(&self.noise_trim_top_fraction)
        {
            return Err("noise_trim_top_fraction must be in 0.0..=0.25".to_string());
        }
        if !finite(self.signal_trim_fraction) || !(0.05..=0.45).contains(&self.signal_trim_fraction)
        {
            return Err("signal_trim_fraction must be in 0.05..=0.45".to_string());
        }
        if !(1..=1000).contains(&self.min_noise_blocks) {
            return Err("min_noise_blocks must be in 1..=1000".to_string());
        }
        if !finite(self.transient_alert_db) || !(6.0..=60.0).contains(&self.transient_alert_db) {
            return Err("transient_alert_db must be in 6..=60".to_string());
        }
        Ok(())
    }
}

/// onset 来源（证据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnsetSource {
    /// 能量门在采集里检出的真实 onset。
    Detected,
    /// 未检出 → 回退到期望 onset（seed + pre_frames，此时证据里如实标记）。
    Expected,
}

impl OnsetSource {
    /// 机器可读标签。
    pub const fn as_str(self) -> &'static str {
        match self {
            OnsetSource::Detected => "detected",
            OnsetSource::Expected => "expected-fallback",
        }
    }
}

/// 稳健噪声 / SNR 估计结果（含窗口与来源，供证据冻结）。
#[derive(Debug, Clone, PartialEq)]
pub struct RobustSnrEstimate {
    /// 估计器 schema。
    pub schema: &'static str,
    /// 检出/回退的 onset（采集 sample 索引，0 = 采集起点）。
    pub onset_sample: f64,
    /// onset 来源。
    pub onset_source: OnsetSource,
    /// 使用的分块帧数。
    pub block_frames: usize,
    /// 噪声窗口（采集 sample；首个/末个被采用的候选块边界）。
    pub noise_window_start: usize,
    /// 噪声窗口结束（不含）。
    pub noise_window_end: usize,
    /// 采用的噪声候选块数。
    pub noise_blocks_used: u32,
    /// 因 "信号块" 被排除的分块数。
    pub noise_blocks_excluded_signal: u32,
    /// 因 "最高分位剔除" 被丢弃的分块数。
    pub noise_blocks_trimmed: u32,
    /// 稳健噪声 RMS（dBFS）。
    pub noise_rms_dbfs: f64,
    /// 信号窗口（采集 sample）。
    pub signal_window_start: usize,
    /// 信号窗口结束（不含）。
    pub signal_window_end: usize,
    /// 信号 RMS（dBFS）。
    pub signal_rms_dbfs: f64,
    /// SNR（dB）。
    pub snr_db: f64,
    /// 采集开头（onset 前）最大块 RMS（dBFS）。
    pub pre_onset_max_dbfs: f64,
    /// 采集开头最大块 RMS 的时刻（秒）。
    pub pre_onset_max_seconds: f64,
    /// 是否检测到高于噪声 transient_alert_db 的瞬态（pop 等；证据标记）。
    pub transient_rejected: bool,
}

fn dbfs_of(amplitude: f64) -> f64 {
    if amplitude > 0.0 && amplitude.is_finite() {
        20.0 * amplitude.log10()
    } else {
        SILENCE_DBFS
    }
}

fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for sample in samples {
        sum += (*sample as f64) * (*sample as f64);
    }
    (sum / samples.len() as f64).sqrt()
}

/// 分块 RMS（dBFS）。
pub fn block_rms_dbfs(samples: &[f32], block_frames: usize) -> Vec<f64> {
    if block_frames == 0 {
        return Vec::new();
    }
    let count = samples.len() / block_frames;
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let start = index * block_frames;
        out.push(dbfs_of(rms(&samples[start..start + block_frames])));
    }
    out
}

fn quantile_sorted(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return SILENCE_DBFS;
    }
    let fraction = fraction.clamp(0.0, 1.0);
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn median_sorted(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return SILENCE_DBFS;
    }
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// 在 [start_block, end_block) 里按 "连续 hold 个分块高于 threshold" 检 onset；
/// 命中后在命中块内做阈值扫描精化到 sample。
fn detect_onset(
    samples: &[f32],
    block_db: &[f64],
    block_frames: usize,
    start_block: usize,
    end_block: usize,
    threshold_db: f64,
    hold_blocks: usize,
) -> Option<f64> {
    let threshold_linear = 10.0f64.powf(threshold_db / 20.0);
    let mut run = 0usize;
    for index in start_block..end_block.min(block_db.len()) {
        if block_db[index] >= threshold_db {
            run += 1;
            if run >= hold_blocks {
                let onset_block = index + 1 - hold_blocks;
                let start = onset_block * block_frames;
                let end = ((onset_block + 1) * block_frames).min(samples.len());
                for (offset, sample) in samples[start..end].iter().enumerate() {
                    if sample.abs() as f64 >= threshold_linear {
                        return Some((start + offset) as f64);
                    }
                }
                return Some(start as f64);
            }
        } else {
            run = 0;
        }
    }
    None
}

/// 稳健噪声 / SNR 估计。
///
/// expected_onset_sample 是 seed + first_render_frame（seed = render frame 0 对应的
/// 采集 sample 索引；采集先起时为正），只当**搜索锚点**用；真实 onset 由能量门从采集
/// 里检出。sweep_frames 是单遍扫频帧数（信号窗取单遍扫频中部）。
pub fn estimate(
    samples: &[f32],
    sample_rate_hz: u32,
    sweep_frames: u64,
    expected_onset_sample: Option<f64>,
    config: &RobustSnrConfig,
) -> Option<RobustSnrEstimate> {
    if samples.is_empty() || sample_rate_hz == 0 || config.validate().is_err() {
        return None;
    }
    let sample_rate = sample_rate_hz as f64;
    let block_frames = ((config.block_seconds * sample_rate).round() as usize).clamp(64, 4096);
    let block_db = block_rms_dbfs(samples, block_frames);
    if block_db.len() < config.min_noise_blocks as usize + 1 {
        return None;
    }
    let mut sorted_all = block_db.clone();
    sorted_all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 低分位 = 稳健噪声参考（扫频/pop 占采集少数，20% 分位不会被它们抬走）。
    let quiet_ref_db = quantile_sorted(&sorted_all, 0.20);
    let onset_threshold_db = quiet_ref_db + config.onset_margin_db;

    let transient_guard_frames = (config.transient_guard_seconds * sample_rate).round() as usize;
    let (search_start_samples, search_end_samples) = match expected_onset_sample {
        Some(expected) if expected.is_finite() => {
            let expected = expected.clamp(0.0, samples.len() as f64);
            let start = (expected - config.search_back_seconds * sample_rate).max(0.0);
            let end =
                (expected + config.search_forward_seconds * sample_rate).min(samples.len() as f64);
            (start, end)
        }
        _ => (0.0, samples.len() as f64),
    };
    let search_start_samples = search_start_samples.max(transient_guard_frames as f64);
    let search_start_block = (search_start_samples as usize) / block_frames;
    let search_end_block = ((search_end_samples as usize) / block_frames).min(block_db.len());
    let mut onset_sample = None;
    if search_end_block > search_start_block {
        onset_sample = detect_onset(
            samples,
            &block_db,
            block_frames,
            search_start_block,
            search_end_block,
            onset_threshold_db,
            config.onset_hold_blocks as usize,
        );
    }
    let onset_source;
    let onset_sample = match onset_sample {
        Some(value) => {
            onset_source = OnsetSource::Detected;
            value
        }
        None => match expected_onset_sample {
            Some(expected) if expected.is_finite() => {
                onset_source = OnsetSource::Expected;
                expected.clamp(0.0, samples.len() as f64)
            }
            _ => return None,
        },
    };

    // 信号块掩码：高于噪声参考 signal_exclude_margin_db 的块 + 与首遍扫频重叠的块
    // + 其邻块（fade/尾部振铃）。pop 也被这条规则当瞬态排除。
    let signal_floor_db = quiet_ref_db + config.signal_exclude_margin_db;
    let sweep_len = (sweep_frames as usize).max(1);
    let first_sweep_start = onset_sample as usize;
    let first_sweep_end = (first_sweep_start + sweep_len).min(samples.len());
    let mut signal_mask = vec![false; block_db.len()];
    for (index, db) in block_db.iter().enumerate() {
        let start = index * block_frames;
        if *db > signal_floor_db
            || (start < first_sweep_end && start + block_frames > first_sweep_start)
        {
            signal_mask[index] = true;
        }
    }
    let neighbor_radius = ((0.02 * sample_rate) as usize / block_frames).max(1);
    let mut expanded = signal_mask.clone();
    for (index, flagged) in signal_mask.iter().enumerate() {
        if *flagged {
            let start = index.saturating_sub(neighbor_radius);
            let end = (index + neighbor_radius + 1).min(signal_mask.len());
            for slot in expanded.iter_mut().take(end).skip(start) {
                *slot = true;
            }
        }
    }

    // 噪声候选：transient guard 之后、非信号掩码块。
    let guard_blocks = transient_guard_frames / block_frames;
    let mut candidates: Vec<f64> = Vec::new();
    let mut excluded_signal = 0u32;
    let mut candidate_indices: Vec<usize> = Vec::new();
    for index in 0..block_db.len() {
        if expanded[index] {
            excluded_signal += 1;
            continue;
        }
        if index < guard_blocks {
            continue;
        }
        candidates.push(block_db[index]);
        candidate_indices.push(index);
    }
    let mut trimmed = 0u32;
    let noise_db;
    let noise_window_start;
    let noise_window_end;
    if candidates.len() >= config.min_noise_blocks as usize {
        let total = candidates.len();
        let mut sorted_candidates = candidates.clone();
        sorted_candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let trim = ((total as f64) * config.noise_trim_top_fraction).floor() as usize;
        let trim = trim.min(total.saturating_sub(1));
        trimmed = trim as u32;
        let kept_values = total - trim;
        let cutoff = sorted_candidates[kept_values - 1];
        noise_db = median_sorted(&sorted_candidates[..kept_values]);
        let mut first = None;
        let mut last = 0usize;
        for (slot, index) in candidate_indices.iter().enumerate() {
            if candidates[slot] <= cutoff {
                if first.is_none() {
                    first = Some(*index);
                }
                last = *index;
            }
        }
        let first = first.unwrap_or(0);
        noise_window_start = first * block_frames;
        noise_window_end = ((last + 1) * block_frames).min(samples.len());
    } else if !candidates.is_empty() {
        let mut sorted_candidates = candidates.clone();
        sorted_candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        noise_db = median_sorted(&sorted_candidates);
        noise_window_start = candidate_indices.first().copied().unwrap_or(0) * block_frames;
        noise_window_end = (candidate_indices.last().copied().unwrap_or(0) + 1) * block_frames;
    } else {
        noise_db = quiet_ref_db;
        noise_window_start = 0;
        noise_window_end = 0;
    }

    let trim = config.signal_trim_fraction;
    let signal_start = (onset_sample + sweep_frames as f64 * trim).round() as usize;
    let signal_end = (onset_sample + sweep_frames as f64 * (1.0 - trim)).round() as usize;
    let signal_start = signal_start.min(samples.len());
    let signal_end = signal_end.min(samples.len());
    if signal_end <= signal_start {
        return None;
    }
    let signal_db = dbfs_of(rms(&samples[signal_start..signal_end]));

    // 采集开头（onset 前）的瞬态证据：pop 会在这里露头。
    let mut pre_onset_max = SILENCE_DBFS;
    let mut pre_onset_max_seconds = 0.0;
    let pre_blocks = ((onset_sample as usize) / block_frames).min(block_db.len());
    for (index, db) in block_db.iter().enumerate().take(pre_blocks) {
        if *db > pre_onset_max {
            pre_onset_max = *db;
            pre_onset_max_seconds = (index * block_frames) as f64 / sample_rate;
        }
    }
    let transient_rejected = pre_onset_max.is_finite()
        && noise_db.is_finite()
        && pre_onset_max > noise_db + config.transient_alert_db;

    Some(RobustSnrEstimate {
        schema: ROBUST_SNR_SCHEMA,
        onset_sample,
        onset_source,
        block_frames,
        noise_window_start,
        noise_window_end,
        noise_blocks_used: candidates.len().saturating_sub(trimmed as usize) as u32,
        noise_blocks_excluded_signal: excluded_signal,
        noise_blocks_trimmed: trimmed,
        noise_rms_dbfs: noise_db,
        signal_window_start: signal_start,
        signal_window_end: signal_end,
        signal_rms_dbfs: signal_db,
        snr_db: signal_db - noise_db,
        pre_onset_max_dbfs: pre_onset_max,
        pre_onset_max_seconds,
        transient_rejected,
    })
}

/// 回归对照：**修复前**的固定前静音宽带 SNR（仅测试用，证明旧口径会被 pop 拉跨）。
///
/// 复刻修复前的 stage_backend::compute_snr_db：噪声窗 [0, seed+first_frame)，
/// 信号窗 onset+sweep/4 .. onset+3*sweep/4，seed 是 "render frame 0 对应的采集
/// sample 索引"（采集先起时为正）。
#[cfg(test)]
pub(crate) fn legacy_fixed_pre_silence_snr_db(
    samples: &[f32],
    sweep_frames: u64,
    first_frame: u64,
    seed_render_frame: f64,
) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let first_onset = (seed_render_frame + first_frame as f64).max(0.0) as usize;
    if first_onset < 256 {
        return None;
    }
    let noise_end = first_onset.min(samples.len());
    let noise_window = &samples[..noise_end];
    if noise_window.len() < 256 {
        return None;
    }
    let noise_rms = rms(noise_window);
    let sweep_frames = sweep_frames as usize;
    let signal_start = first_onset
        .saturating_add(sweep_frames / 4)
        .min(samples.len());
    let signal_end = first_onset
        .saturating_add(sweep_frames)
        .saturating_sub(sweep_frames / 4)
        .min(samples.len());
    if signal_end <= signal_start {
        return None;
    }
    let signal_rms = rms(&samples[signal_start..signal_end]);
    if noise_rms <= 0.0 {
        return Some(f64::INFINITY);
    }
    if signal_rms <= 0.0 {
        return Some(f64::NEG_INFINITY);
    }
    Some(20.0 * (signal_rms / noise_rms).log10())
}
