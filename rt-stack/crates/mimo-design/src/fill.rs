//! 单点真实 H 上的 D7 "填谷（null fill）"设计（P=1、M=5；纯离线）。
//!
//! 背景：mimo-design 的 B2/B3 面向"多测点 design 点集"的全局目标 A(f) 设计；单点真实现场
//! 只有 1 个测点（P=1）却有 5 个执行器（M=5），T66 P_post 只能给每个 role 一个对角 FIR。
//! 本模块把同一条算法链（逐频点加权岭 LS + 每源 +6 dB 幅值帽 + 因果实 FIR + 实际 FIR
//! 频响指标）搬到单点：每个输入通道 n 的 P_post 滤波器 F_n(f) 作用在它自己身上，
//! 单点压力 P(f) = sum_n H_n(f)·F_n(f)（对角线模型，可在 T66 roles[].p_fir 内表达）。
//!
//! 三种设计口径：
//! 1. combined：目标 = 平滑(总和的实测复响应)，5 个 F 全自由 —— 用其它执行器当 donor 填总响应的谷；
//! 2. only_i：目标 = sum_{j != i} H_j + 平滑(H_i)（只换通道 i 的 target，其它 preserve）——
//!    T51/T52 单点实验中"逐通道 fill"的对角线版本；联合求解、donor 参与；
//! 3. diag_self_i：仅 F_i 自由、F_j=1 —— 只用本通道自己的 +6 dB 帽能填多少（口径无关参考）。
//!
//! 诚实边界：
//! - SINGLE_POINT_NOT_SPATIAL：单点只能预测该点，其它听音位的空间副作用完全不可评估；
//! - GAUGE：t4b analysis 逐通道以各自 1 kHz 归一，H 跨执行器绝对幅度不可比；
//!   口径无关量（同通道比值、diag 可填、taps 需求）与口径相关量（donor/交叉路径）必须分开；
//! - PREDICTED：所有 fill dB 是设计侧预测（真实 FIR 在测量频点上的复响应），不是实测；
//! - 未做 apply / 声学复测 / 板端 / 声卡操作。

use std::fs;
use std::path::Path;

use crate::fir::{fir_response, FirBasis};
use crate::solve::{weighted_ridge_ls_capped_targets, Cx};

/// 单点填谷证据 schema。
pub const SINGLE_POINT_FILL_SCHEMA: &str = "tspi-mimo-single-point-fill/v1";
/// 单点结论标签（所有产物必须带）。
pub const SINGLE_POINT_CLAIM: &str = "SINGLE_POINT_NOT_SPATIAL / PREDICTED_ONLY / NO_MEASUREMENT";
/// 1/3 oct 名义中心（与 mimo-fill 证据 / spec 口径一致）。
pub const ONE_THIRD_OCT_CENTERS: [f64; 12] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0,
];
/// headroom 门覆盖下限（regularize IMPROVEMENT_BAND_HZ = 63–250 Hz）。
pub const HEADROOM_GATE_LO_HZ: f64 = 63.0;
/// 对角路径 +6 dB 帽（相对 identity）。
pub const DIAG_CAP_DB: f64 = 6.0;

/// 单点复传递函数集合：h[role][freq]，P(f)=sum_n h[n][k]·F_n[k]。
#[derive(Clone, Debug)]
pub struct SinglePointH {
    /// 升序频点（Hz）。
    pub freqs_hz: Vec<f64>,
    /// 通道/role 名（顺序与 h 一致）。
    pub roles: Vec<String>,
    /// h[role][freq]。
    pub h: Vec<Vec<Cx>>,
}

impl SinglePointH {
    /// 从 H_complex.csv（freq_hz,<ROLE>_re,<ROLE>_im...）加载；校验有限/升序/非空。
    pub fn load_csv(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("读取 H CSV 失败 {}: {error}", path.display()))?;
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let header = lines
            .next()
            .ok_or_else(|| "H CSV 为空（缺表头）".to_string())?;
        let columns: Vec<&str> = header.split(',').map(str::trim).collect();
        if columns.first() != Some(&"freq_hz") {
            return Err("H CSV 首列必须是 freq_hz".to_string());
        }
        let mut roles: Vec<String> = Vec::new();
        let mut index = 1usize;
        while index + 1 < columns.len() {
            let re = columns[index];
            let im = columns[index + 1];
            let Some(role) = re.strip_suffix("_re") else {
                return Err(format!("H CSV 列 {re:?} 不是 <ROLE>_re"));
            };
            if im != format!("{role}_im") {
                return Err(format!("H CSV 列 {im:?} 与 {role}_im 不匹配"));
            }
            roles.push(role.to_string());
            index += 2;
        }
        if roles.is_empty() {
            return Err("H CSV 没有通道列".to_string());
        }
        let mut freqs_hz = Vec::new();
        let mut h: Vec<Vec<Cx>> = vec![Vec::new(); roles.len()];
        for (line_number, line) in lines.enumerate() {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            if fields.len() != 1 + 2 * roles.len() {
                return Err(format!(
                    "H CSV 第 {} 行列数 {} != {}",
                    line_number + 2,
                    fields.len(),
                    1 + 2 * roles.len()
                ));
            }
            let parse = |text: &str, what: &str| -> Result<f64, String> {
                text.parse::<f64>()
                    .map_err(|error| {
                        format!(
                            "H CSV 第 {} 行 {what}={text:?} 非法：{error}",
                            line_number + 2
                        )
                    })
                    .and_then(|value| {
                        if value.is_finite() {
                            Ok(value)
                        } else {
                            Err(format!("H CSV 第 {} 行 {what} 非有限", line_number + 2))
                        }
                    })
            };
            let freq = parse(fields[0], "freq_hz")?;
            if let Some(previous) = freqs_hz.last() {
                if freq <= *previous {
                    return Err(format!(
                        "H CSV 频点必须严格升序（第 {} 行 {freq} <= {previous}）",
                        line_number + 2
                    ));
                }
            }
            freqs_hz.push(freq);
            for role_index in 0..roles.len() {
                let re = parse(fields[1 + 2 * role_index], "re")?;
                let im = parse(fields[2 + 2 * role_index], "im")?;
                h[role_index].push(Cx::new(re, im));
            }
        }
        if freqs_hz.is_empty() {
            return Err("H CSV 没有数据行".to_string());
        }
        Ok(Self {
            freqs_hz,
            roles,
            h,
        })
    }

    /// 通道数 M。
    pub fn n_sources(&self) -> usize {
        self.roles.len()
    }

    /// 频点数 K。
    pub fn len(&self) -> usize {
        self.freqs_hz.len()
    }

    /// 单位激励（F=1）下的单点压力 P0(f)=sum_n H_n(f)。
    pub fn identity_pressure(&self) -> Vec<Cx> {
        (0..self.len())
            .map(|k| {
                let mut acc = Cx::zero();
                for source in &self.h {
                    acc = acc.add(source[k]);
                }
                acc
            })
            .collect()
    }

    /// 给定每源复响应 F_n(f) 的单点压力。
    pub fn pressure(&self, f: &[Vec<Cx>]) -> Vec<Cx> {
        (0..self.len()).map(|k| self.pressure_at(f, k)).collect()
    }

    /// 单频点压力 P(f_k)。
    pub fn pressure_at(&self, f: &[Vec<Cx>], k: usize) -> Cx {
        let mut acc = Cx::zero();
        for (source, response) in self.h.iter().zip(f.iter()) {
            acc = acc.add(source[k].mul(response[k]));
        }
        acc
    }

    /// 通道 i 的“其它通道 preserve”总和：sum_{j != i} H_j(f)。
    pub fn others_pressure(&self, source_index: usize) -> Vec<Cx> {
        (0..self.len())
            .map(|k| {
                let mut acc = Cx::zero();
                for (index, source) in self.h.iter().enumerate() {
                    if index != source_index {
                        acc = acc.add(source[k]);
                    }
                }
                acc
            })
            .collect()
    }
}

/// dB（幅度）。
pub fn db_amp(value: f64) -> f64 {
    20.0 * value.abs().max(1e-30).log10()
}

/// 功率域 octave 平滑 + 功率加权圆均值相位（与 mimo-fill/build_targets.py 同口径）。
pub fn smooth_complex_power(signal: &[Cx], freqs_hz: &[f64], octave: f64) -> Vec<Cx> {
    let half = 2.0f64.powf(octave / 2.0);
    let mut out = Vec::with_capacity(signal.len());
    for (k, f0) in freqs_hz.iter().enumerate() {
        let mut power_sum = 0.0f64;
        let mut weighted_re = 0.0f64;
        let mut weighted_im = 0.0f64;
        let mut count = 0usize;
        for (j, fj) in freqs_hz.iter().enumerate() {
            if *fj >= f0 / half && *fj <= f0 * half {
                let power = signal[j].norm_sqr();
                power_sum += power;
                weighted_re += power * signal[j].re;
                weighted_im += power * signal[j].im;
                count += 1;
            }
        }
        if count == 0 || power_sum <= 0.0 {
            out.push(signal[k]);
            continue;
        }
        let magnitude = (power_sum / count as f64).sqrt();
        let norm = (weighted_re * weighted_re + weighted_im * weighted_im).sqrt();
        if norm <= 0.0 {
            out.push(Cx::new(magnitude, 0.0));
        } else {
            out.push(Cx::new(
                magnitude * weighted_re / norm,
                magnitude * weighted_im / norm,
            ));
        }
    }
    out
}

/// 1/3 oct 带内频点索引（边界 [c·2^(−1/6), c·2^(+1/6))）。
pub fn band_indices(freqs_hz: &[f64], center_hz: f64) -> Vec<usize> {
    let lo = center_hz / 2.0f64.powf(1.0 / 6.0);
    let hi = center_hz * 2.0f64.powf(1.0 / 6.0);
    freqs_hz
        .iter()
        .enumerate()
        .filter(|(_, f)| **f >= lo && **f < hi)
        .map(|(index, _)| index)
        .collect()
}

fn rms_amp_db(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let power: f64 = values.iter().map(|v| v * v).sum::<f64>() / values.len() as f64;
    10.0 * power.max(1e-60).log10()
}

/// 设计配置。
#[derive(Clone, Debug)]
pub struct FillConfig {
    /// 采样率。
    pub fs: f64,
    /// FIR taps（<= 4096）。
    pub taps: usize,
    /// 公共延迟（samples；P FIR 的 group delay）。
    pub delay_samples: usize,
    /// 岭正则（相对 mean|H|^2）。
    pub lambda_rel: f64,
    /// 每源幅值帽（绝对，相对 identity 的线性倍数；+6 dB = 2.0）。
    pub cap: f64,
    /// 目标平滑 octave（默认 1/6）。
    pub smooth_octave: f64,
    /// FIR dual 求解对角加载（相对 K）。
    pub diag_load_rel: f64,
}

impl FillConfig {
    /// 默认（D=taps/2、lambda=1e-6、帽 +6 dB、1/6 oct 平滑）。
    pub fn new(taps: usize) -> Self {
        Self {
            fs: 48_000.0,
            taps,
            delay_samples: taps / 2,
            lambda_rel: 1e-6,
            cap: 10.0f64.powf(DIAG_CAP_DB / 20.0),
            smooth_octave: 1.0 / 6.0,
            diag_load_rel: 1e-6,
        }
    }
}

/// 一次填谷 FIR 设计结果。
#[derive(Clone, Debug)]
pub struct FillDesign {
    /// taps。
    pub taps: usize,
    /// 公共延迟。
    pub delay_samples: usize,
    /// h[source][tap]（因果实系数）。
    pub h: Vec<Vec<f64>>,
    /// 实际 FIR 在测量频点的复响应 G_n(f)。
    pub g: Vec<Vec<Cx>>,
    /// 每源 per-bin 理想复响应 F_n(f)（设计目标，不含 FIR 实现误差）。
    pub f: Vec<Vec<Cx>>,
    /// sum_n sum_t h^2（effort）。
    pub effort: f64,
    /// 全带最大 |G| dB。
    pub max_gain_db: f64,
    /// 门带（>=63 Hz）最大 |G| dB。
    pub max_gain_gate_db: f64,
    /// 最大 |G| 所在频率。
    pub max_gain_freq_hz: f64,
    /// 每源最大 |G| dB。
    pub max_gain_per_source_db: Vec<f64>,
    /// 理想 F 相对 identity 的最大正增益 dB（D7 target FIR +3 dB 口）。
    pub max_f_positive_db: f64,
    /// 预测输出峰值（参考输入 0.5 * max|G|）。
    pub predicted_peak: f64,
}

/// 共享 FirBasis 的求解器（同一频点/taps 下重复设计时避免 O((2K)^2·taps) 重建）。
pub struct FillSolver {
    cfg: FillConfig,
    basis: FirBasis,
}

impl FillSolver {
    /// 构造（频点、taps 决定基矩阵）。
    pub fn new(freqs_hz: &[f64], cfg: FillConfig) -> Result<Self, String> {
        let basis = FirBasis::new(freqs_hz, cfg.fs, cfg.taps)?;
        Ok(Self { cfg, basis })
    }

    /// 配置。
    pub fn config(&self) -> &FillConfig {
        &self.cfg
    }

    /// 由每个频点的复目标总压力 A(f_k) 联合设计 M 个对角 FIR（每源 +6 dB 帽）。
    pub fn design(&self, host: &SinglePointH, target_total: &[Cx]) -> Result<FillDesign, String> {
        let cfg = &self.cfg;
        let k_count = host.len();
        if target_total.len() != k_count {
            return Err(format!(
                "design: 目标长度 {} != 频点数 {k_count}",
                target_total.len()
            ));
        }
        let mut f_resp: Vec<Vec<Cx>> = vec![Vec::with_capacity(k_count); host.n_sources()];
        for k in 0..k_count {
            let row: Vec<Cx> = host.h.iter().map(|source| source[k]).collect();
            let mean_power = row.iter().map(|h| h.norm_sqr()).sum::<f64>() / row.len() as f64;
            let lambda = (cfg.lambda_rel * mean_power).max(1e-18);
            let solved = weighted_ridge_ls_capped_targets(
                &[row],
                &[target_total[k]],
                &[1.0],
                lambda,
                cfg.cap,
            )?;
            for (source_index, value) in solved.into_iter().enumerate() {
                f_resp[source_index].push(value);
            }
        }
        let mut h: Vec<Vec<f64>> = Vec::with_capacity(host.n_sources());
        let mut g: Vec<Vec<Cx>> = Vec::with_capacity(host.n_sources());
        for source_index in 0..host.n_sources() {
            let mut c = Vec::with_capacity(k_count);
            for k in 0..k_count {
                let omega = std::f64::consts::TAU * host.freqs_hz[k] / cfg.fs;
                let rot = Cx::exp_neg_j(omega * cfg.delay_samples as f64);
                c.push(f_resp[source_index][k].mul(rot));
            }
            let coefficients = self.basis.solve(&c, cfg.diag_load_rel)?;
            let response = fir_response(&coefficients, &host.freqs_hz, cfg.fs);
            h.push(coefficients);
            g.push(response);
        }
        Ok(summarize_design(cfg, host, &f_resp, h, g))
    }

    /// 仅设计某一通道的 FIR（其它通道固定为 delta_D identity）；目标仍是单点总压力。
    pub fn design_single_source(
        &self,
        host: &SinglePointH,
        source_index: usize,
        target_total: &[Cx],
    ) -> Result<FillDesign, String> {
        let cfg = &self.cfg;
        let k_count = host.len();
        if source_index >= host.n_sources() {
            return Err(format!("design_single_source: 源 {source_index} 越界"));
        }
        if target_total.len() != k_count {
            return Err(format!(
                "design_single_source: 目标长度 {} != 频点数 {k_count}",
                target_total.len()
            ));
        }
        let others = host.others_pressure(source_index);
        let mut f_resp: Vec<Vec<Cx>> = vec![Vec::with_capacity(k_count); host.n_sources()];
        for k in 0..k_count {
            let h_i = host.h[source_index][k];
            let residual = target_total[k].sub(others[k]);
            let value = if h_i.norm_sqr() <= 1e-30 {
                Cx::zero()
            } else {
                let ideal = residual.mul(h_i.recip_h());
                let magnitude = ideal.abs();
                if magnitude > cfg.cap {
                    ideal.scale(cfg.cap / magnitude)
                } else {
                    ideal
                }
            };
            for (source_index_inner, row) in f_resp.iter_mut().enumerate() {
                row.push(if source_index_inner == source_index {
                    value
                } else {
                    Cx::one()
                });
            }
        }
        let mut h: Vec<Vec<f64>> = Vec::with_capacity(host.n_sources());
        let mut g: Vec<Vec<Cx>> = Vec::with_capacity(host.n_sources());
        for source_index_inner in 0..host.n_sources() {
            if source_index_inner != source_index {
                let identity = identity_fir(cfg.taps, cfg.delay_samples);
                let response = fir_response(&identity, &host.freqs_hz, cfg.fs);
                h.push(identity);
                g.push(response);
                continue;
            }
            let mut c = Vec::with_capacity(k_count);
            for k in 0..k_count {
                let omega = std::f64::consts::TAU * host.freqs_hz[k] / cfg.fs;
                let rot = Cx::exp_neg_j(omega * cfg.delay_samples as f64);
                c.push(f_resp[source_index][k].mul(rot));
            }
            let coefficients = self.basis.solve(&c, cfg.diag_load_rel)?;
            let response = fir_response(&coefficients, &host.freqs_hz, cfg.fs);
            h.push(coefficients);
            g.push(response);
        }
        Ok(summarize_design(cfg, host, &f_resp, h, g))
    }
}

impl Cx {
    fn recip_h(self) -> Cx {
        let norm = self.norm_sqr();
        Cx::new(self.re / norm, -self.im / norm)
    }
}

/// delta_D identity FIR。
pub fn identity_fir(taps: usize, delay_samples: usize) -> Vec<f64> {
    let mut h = vec![0.0f64; taps];
    if delay_samples < taps {
        h[delay_samples] = 1.0;
    }
    h
}

fn summarize_design(
    cfg: &FillConfig,
    host: &SinglePointH,
    f_resp: &[Vec<Cx>],
    h: Vec<Vec<f64>>,
    g: Vec<Vec<Cx>>,
) -> FillDesign {
    let mut effort = 0.0f64;
    let mut max_gain_db = f64::NEG_INFINITY;
    let mut max_gain_gate_db = f64::NEG_INFINITY;
    let mut max_gain_freq_hz = f64::NAN;
    let mut max_gain_per_source_db = Vec::with_capacity(host.n_sources());
    let mut max_f_positive_db = f64::NEG_INFINITY;
    for (source_index, coefficients) in h.iter().enumerate() {
        effort += coefficients.iter().map(|v| v * v).sum::<f64>();
        let mut source_max = f64::NEG_INFINITY;
        for (k, value) in g[source_index].iter().enumerate() {
            let level = db_amp(value.abs());
            if level > source_max {
                source_max = level;
            }
            if level > max_gain_db {
                max_gain_db = level;
                max_gain_freq_hz = host.freqs_hz[k];
            }
            if host.freqs_hz[k] >= HEADROOM_GATE_LO_HZ && level > max_gain_gate_db {
                max_gain_gate_db = level;
            }
        }
        max_gain_per_source_db.push(source_max);
    }
    for row in f_resp {
        for value in row {
            let level = db_amp(value.abs());
            if level > max_f_positive_db {
                max_f_positive_db = level;
            }
        }
    }
    let predicted_peak = 0.5 * 10.0f64.powf(max_gain_db / 20.0);
    FillDesign {
        taps: cfg.taps,
        delay_samples: cfg.delay_samples,
        h,
        g,
        f: f_resp.to_vec(),
        effort,
        max_gain_db,
        max_gain_gate_db,
        max_gain_freq_hz,
        max_gain_per_source_db,
        max_f_positive_db,
        predicted_peak,
    }
}

/// 单带填谷指标行。
#[derive(Clone, Debug)]
pub struct BandFillRow {
    /// 1/3 oct 中心。
    pub center_hz: f64,
    /// 带内频点数。
    pub n_points: usize,
    /// 带内 RMS 电平 before（dB）。
    pub before_rms_db: f64,
    /// 带内 RMS 电平 after（dB）。
    pub after_rms_db: f64,
    /// 带内 RMS 变化（dB；正=更响）。
    pub delta_rms_db: f64,
    /// valley 参考通道在带内的最低点频率。
    pub valley_freq_hz: f64,
    /// 参考通道带内最低点电平 before（dB）。
    pub before_min_db: f64,
    /// 该频点 after 总响应电平（dB）。
    pub after_at_valley_db: f64,
    /// 该频点填谷量（after-before，dB；正=填）。
    pub fill_at_valley_db: f64,
    /// 目标在谷点相对 before 的隐含提升（dB）。
    pub target_fill_at_valley_db: f64,
    /// before 谷深（带 RMS - 带内最低，dB）。
    pub depth_before_db: f64,
    /// after 谷深。
    pub depth_after_db: f64,
    /// 谷深减少量（正=变浅）。
    pub depth_reduction_db: f64,
}

/// 计算每带 before/after 指标；valley_ref 决定谷点位置（通道自身或总响应）。
pub fn evaluate_bands(
    freqs_hz: &[f64],
    before: &[Cx],
    after: &[Cx],
    target: Option<&[Cx]>,
    valley_ref: &[Cx],
) -> Vec<BandFillRow> {
    let mut rows = Vec::new();
    for center in ONE_THIRD_OCT_CENTERS {
        let indices = band_indices(freqs_hz, center);
        if indices.is_empty() {
            continue;
        }
        let before_levels: Vec<f64> = indices.iter().map(|k| db_amp(before[*k].abs())).collect();
        let after_levels: Vec<f64> = indices.iter().map(|k| db_amp(after[*k].abs())).collect();
        let ref_levels: Vec<f64> = indices.iter().map(|k| db_amp(valley_ref[*k].abs())).collect();
        let before_linear: Vec<f64> = before_levels.iter().map(|db| 10.0f64.powf(db / 20.0)).collect();
        let after_linear: Vec<f64> = after_levels.iter().map(|db| 10.0f64.powf(db / 20.0)).collect();
        let before_rms_db = rms_amp_db(&before_linear);
        let after_rms_db = rms_amp_db(&after_linear);
        let valley_position = ref_levels
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(position, _)| position)
            .unwrap_or(0);
        let k_valley = indices[valley_position];
        let before_min_db = before_levels[valley_position];
        let after_at_valley_db = after_levels[valley_position];
        let target_fill_at_valley_db = target
            .map(|values| db_amp(values[k_valley].abs()) - before_min_db)
            .unwrap_or(f64::NAN);
        rows.push(BandFillRow {
            center_hz: center,
            n_points: indices.len(),
            before_rms_db,
            after_rms_db,
            delta_rms_db: after_rms_db - before_rms_db,
            valley_freq_hz: freqs_hz[k_valley],
            before_min_db,
            after_at_valley_db,
            fill_at_valley_db: after_at_valley_db - before_min_db,
            target_fill_at_valley_db,
            depth_before_db: before_rms_db - before_min_db,
            depth_after_db: after_rms_db - after_levels[valley_position],
            depth_reduction_db: (before_rms_db - before_min_db)
                - (after_rms_db - after_levels[valley_position]),
        });
    }
    rows
}

/// donor 判据行（T51/T52 口径：单 donor 在 -6 dB 修正帽下需比被填通道带内 RMS 高 +6 dB）。
#[derive(Clone, Debug)]
pub struct DonorRow {
    pub role: String,
    pub center_hz: f64,
    pub valley_freq_hz: f64,
    pub level_rms_db: f64,
    pub valley_depth_db: f64,
    pub diag_cap_fill_db: f64,
    pub cross_path_needed_db: f64,
    pub best_donor: String,
    pub best_donor_level_db: f64,
    pub donor_margin_db: f64,
    pub donor_ok: bool,
    pub headroom_gate: &'static str,
    pub band_rms_rank: usize,
    pub verdict: String,
}

/// 计算 donor 判据（口径相关；调用方需显式传入所用 gauge 的 H）。
pub fn donor_criterion(host: &SinglePointH) -> Vec<DonorRow> {
    let mut rows = Vec::new();
    let mut overall = vec![0.0f64; host.n_sources()];
    let mut band_counts = 0usize;
    for center in ONE_THIRD_OCT_CENTERS {
        let indices = band_indices(&host.freqs_hz, center);
        if indices.is_empty() {
            continue;
        }
        band_counts += 1;
        for (source_index, source) in host.h.iter().enumerate() {
            let levels: Vec<f64> = indices.iter().map(|k| db_amp(source[*k].abs())).collect();
            let linear: Vec<f64> = levels.iter().map(|db| 10.0f64.powf(db / 20.0)).collect();
            overall[source_index] += rms_amp_db(&linear);
        }
    }
    let mut band_rms_rank = vec![0usize; host.n_sources()];
    let mut ranked: Vec<(usize, f64)> = overall
        .iter()
        .enumerate()
        .map(|(index, total)| (index, total / band_counts.max(1) as f64))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (rank, (source_index, _)) in ranked.iter().enumerate() {
        band_rms_rank[*source_index] = rank + 1;
    }
    for center in ONE_THIRD_OCT_CENTERS {
        let indices = band_indices(&host.freqs_hz, center);
        if indices.is_empty() {
            continue;
        }
        for (source_index, role) in host.roles.iter().enumerate() {
            let levels: Vec<f64> = indices
                .iter()
                .map(|k| db_amp(host.h[source_index][*k].abs()))
                .collect();
            let linear: Vec<f64> = levels.iter().map(|db| 10.0f64.powf(db / 20.0)).collect();
            let rms_db = rms_amp_db(&linear);
            let valley_position = levels
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(position, _)| position)
                .unwrap_or(0);
            let k_valley = indices[valley_position];
            let depth = rms_db - levels[valley_position];
            let mut best = (String::new(), f64::NEG_INFINITY);
            for (other_index, other) in host.h.iter().enumerate() {
                if other_index == source_index {
                    continue;
                }
                let level = db_amp(other[k_valley].abs());
                if level > best.1 {
                    best = (host.roles[other_index].clone(), level);
                }
            }
            let donor_margin_db = best.1 - rms_db;
            let donor_ok = donor_margin_db >= 6.0;
            let gate = if center >= HEADROOM_GATE_LO_HZ {
                "covered(>=63Hz)"
            } else {
                "BLIND(<63Hz)"
            };
            let verdict = if depth <= DIAG_CAP_DB {
                "DIAG_ONLY_FILL_OK(<=+6dB)".to_string()
            } else if donor_ok {
                "CROSS_PATH_FEASIBLE_BY_DONOR".to_string()
            } else if band_rms_rank[source_index] == 1 {
                "LOUDEST_CHANNEL_NO_DONOR".to_string()
            } else {
                "OVER_CAP_OR_NO_DONOR".to_string()
            };
            rows.push(DonorRow {
                role: role.clone(),
                center_hz: center,
                valley_freq_hz: host.freqs_hz[k_valley],
                level_rms_db: rms_db,
                valley_depth_db: depth,
                diag_cap_fill_db: depth.min(DIAG_CAP_DB),
                cross_path_needed_db: (depth - DIAG_CAP_DB).max(0.0),
                best_donor: best.0,
                best_donor_level_db: best.1,
                donor_margin_db,
                donor_ok,
                headroom_gate: gate,
                band_rms_rank: band_rms_rank[source_index],
                verdict,
            });
        }
    }
    rows
}

/// taps 需求行。
#[derive(Clone, Debug)]
pub struct TapsRow {
    pub role: String,
    pub center_hz: f64,
    pub valley_freq_hz: f64,
    pub valley_depth_db: f64,
    pub width_m3db_hz: f64,
    pub taps_needed_pointwise: u64,
    pub pointwise_within_l_g_4096: bool,
    pub taps_needed_band_level: u64,
    pub band_level_within_l_g_4096: bool,
}

/// 点频（谷宽）taps 需求：taps 约等于 fs / 谷宽（-3 dB 连续区），并给出 1/3 oct 带级需求。
pub fn taps_requirements(host: &SinglePointH) -> Vec<TapsRow> {
    let mut rows = Vec::new();
    for center in ONE_THIRD_OCT_CENTERS {
        let indices = band_indices(&host.freqs_hz, center);
        if indices.is_empty() {
            continue;
        }
        let band_width = center * (2.0f64.powf(1.0 / 6.0) - 2.0f64.powf(-1.0 / 6.0));
        let band_taps = (48_000.0 / band_width).ceil() as u64;
        for (source_index, role) in host.roles.iter().enumerate() {
            let levels: Vec<f64> = indices
                .iter()
                .map(|k| db_amp(host.h[source_index][*k].abs()))
                .collect();
            let linear: Vec<f64> = levels.iter().map(|db| 10.0f64.powf(db / 20.0)).collect();
            let rms_db = rms_amp_db(&linear);
            let valley_position = levels
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(position, _)| position)
                .unwrap_or(0);
            let depth = rms_db - levels[valley_position];
            if depth <= 3.0 {
                continue;
            }
            let deficit: Vec<f64> = levels.iter().map(|level| rms_db - level).collect();
            let mut lo = valley_position;
            while lo > 0 && deficit[lo - 1] > 3.0 {
                lo -= 1;
            }
            let mut hi = valley_position;
            while hi + 1 < indices.len() && deficit[hi + 1] > 3.0 {
                hi += 1;
            }
            let lo_index = indices[lo].saturating_sub(1);
            let hi_index = (indices[hi] + 1).min(host.freqs_hz.len() - 1);
            let width_hz = (host.freqs_hz[hi_index] - host.freqs_hz[lo_index]).max(1e-9);
            let point_taps = (48_000.0 / width_hz).ceil() as u64;
            rows.push(TapsRow {
                role: role.clone(),
                center_hz: center,
                valley_freq_hz: host.freqs_hz[indices[valley_position]],
                valley_depth_db: depth,
                width_m3db_hz: width_hz,
                taps_needed_pointwise: point_taps,
                pointwise_within_l_g_4096: point_taps <= 4096,
                taps_needed_band_level: band_taps,
                band_level_within_l_g_4096: band_taps <= 4096,
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn host_from(freqs: Vec<f64>, h: Vec<Vec<(f64, f64)>>) -> SinglePointH {
        SinglePointH {
            freqs_hz: freqs,
            roles: (0..h.len()).map(|i| format!("S{i}")).collect(),
            h: h
                .into_iter()
                .map(|row| row.into_iter().map(|(re, im)| Cx::new(re, im)).collect())
                .collect(),
        }
    }

    #[test]
    fn smooth_flat_signal_is_identity() {
        let freqs = vec![20.0, 40.0, 80.0, 160.0];
        let signal: Vec<Cx> = freqs.iter().map(|_| Cx::new(1.0, 0.5)).collect();
        let smoothed = smooth_complex_power(&signal, &freqs, 1.0 / 6.0);
        for value in smoothed {
            assert!(approx(value.re, 1.0, 1e-12));
            assert!(approx(value.im, 0.5, 1e-12));
        }
    }

    #[test]
    fn band_indices_one_third_octave() {
        let freqs = vec![10.0, 20.0, 22.0, 25.0, 50.0];
        assert_eq!(band_indices(&freqs, 20.0), vec![1, 2]);
        assert!(band_indices(&freqs, 250.0).is_empty());
    }

    #[test]
    fn joint_design_matches_reachable_target_at_measurement_bins() {
        let freqs = vec![100.0, 200.0, 400.0];
        let host = host_from(
            freqs.clone(),
            vec![
                vec![(1.0, 0.0), (1.0, 0.0), (1.0, 0.0)],
                vec![(1.0, 0.0), (0.0, 1.0), (0.5, 0.0)],
            ],
        );
        let target: Vec<Cx> = vec![Cx::new(2.0, 0.0), Cx::new(2.0, 0.0), Cx::new(2.0, 0.0)];
        let mut cfg = FillConfig::new(64);
        cfg.delay_samples = 32;
        cfg.cap = 10.0;
        let solver = FillSolver::new(&freqs, cfg).unwrap();
        let design = solver.design(&host, &target).unwrap();
        let after = host.pressure(&design.g);
        for k in 0..freqs.len() {
            assert!(
                approx(after[k].abs(), 2.0, 0.35),
                "k={k} |P|={}",
                after[k].abs()
            );
        }
    }

    #[test]
    fn single_source_design_only_changes_that_source() {
        let freqs = vec![100.0, 200.0, 400.0];
        let host = host_from(
            freqs.clone(),
            vec![
                vec![(1.0, 0.0), (1.0, 0.0), (1.0, 0.0)],
                vec![(1.0, 0.0), (0.0, 1.0), (0.5, 0.0)],
            ],
        );
        let target: Vec<Cx> = vec![Cx::new(1.0, 0.0), Cx::new(2.0, 0.0), Cx::new(1.0, 0.0)];
        let cfg = FillConfig::new(64);
        let solver = FillSolver::new(&freqs, cfg).unwrap();
        let design = solver.design_single_source(&host, 0, &target).unwrap();
        assert_eq!(design.h.len(), 2);
        assert!(approx(design.h[1][32], 1.0, 1e-15));
        let after = host.pressure(&design.g);
        assert!(approx(after[1].abs(), 2.0, 0.35), "|P|={}", after[1].abs());
    }

    #[test]
    fn donor_rows_select_louder_neighbour() {
        let freqs = vec![20.0, 21.0, 22.0];
        let host = host_from(
            freqs,
            vec![
                vec![(1.0, 0.0), (0.5, 0.0), (1.0, 0.0)],
                vec![(2.0, 0.0), (2.0, 0.0), (2.0, 0.0)],
            ],
        );
        let rows = donor_criterion(&host);
        let row = rows.iter().find(|r| r.role == "S0").unwrap();
        assert_eq!(row.best_donor, "S1");
        assert!(row.donor_ok, "margin {}", row.donor_margin_db);
    }

    #[test]
    fn identity_fir_places_delta_at_delay() {
        let h = identity_fir(8, 3);
        assert_eq!(h[3], 1.0);
        assert_eq!(h.iter().sum::<f64>(), 1.0);
    }
}
