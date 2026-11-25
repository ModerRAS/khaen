//! min_fast 最小相位快滚降族（T33）——升余弦幅度目标 + 同态最小相位谱分解。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/fir-toolbox.md §3.2（v1.2-draft，含 fs/2 裁决）：参数、参考构造、规范性判据 1–5；
//! - 同文件 §1.2/§1.3/§1.4/§1.5/§1.6、§2.2/§2.4、§3.1 判据 3–6、§7.2（运行准入）；
//! - rt-stack/evidence/T04/recompute-vectors.mjs V17：可复现参考构造与期望值（本文件测试复算）；
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T33 行与 §1/§6/§7（写域/证据纪律）。
//!
//! 家族语义（spec §3.2）：
//! 1. 幅度目标 |H|=1（f<=f_p），[f_p,f_s] 升余弦（或 linear）落到 floor=10^(mag_floor_db/20)，
//!    f>=f_s 取 floor；
//! 2. 用对称线性相位合成（N=prototype.taps，D=(L-1)/2，Kaiser β=8.96，DC 归一）得原型 h_proto；
//! 3. 同态最小相位谱分解（补零到 cepstrum_fft，实倒谱折叠 c[0]/2c[1..N/2-1]/c[N/2]，
//!    exp(FFT) 后 IFFT，取前 L 点；不做峰值归一化）。
//!
//! **min_fast 不是零延迟**：本家族不引入 IIR（传递函数分母为 1，极点全部位于 z=0，无条件稳定），
//! 但群延迟频变（低频约 1.6 samples、过渡带内快速上升），必须报 GD min/max/mean/@1kHz、onset、
//! gd_max，禁止声称“零延迟/零群延迟”（spec §3.2 判据 3；§1.3 亦禁止把支撑长度说成固有群延迟）。
//!
//! T33 leaf 范围：本文件只实现 min_fast 一族的设计/复算/规范判据报告；WAV+manifest 落盘、
//! 20/20k 端点报告的最终编排归 T55；资源准入/融合归 T12/T56。私有测试在
//! crates/designer/tests/T33/min_fast_cases.rs，通过文末 include! 在 #[cfg(test)] 下
//! 编译执行（不改 integrator 独占的 public_registration.rs，见 tests/T33/README.md）。
//!
//! 状态：T33 实现 + 真实 cargo test -p tspi-fir-designer min_fast 执行（见 evidence/T33/）。
//! 未做：板端/ALSA/RT 实测、WAV 落盘导出、roots 分解、零点半径检查（spec MAY）、T55 报告（NOT_RUN）。

use std::fmt;

/// 家族标识（manifest family，spec §2.2/§1.7）。
pub const FAMILY: &str = "min_fast";
/// 家族中文名（spec §1.7）。
pub const FAMILY_CN: &str = "最小相位快滚降";
/// 系数产物 schema（本 leaf 不落盘，仅报告该常量供 T55 使用；spec §2.2）。
pub const FIR_SCHEMA: &str = "tspi-fir/v1";

/// 契约默认采样率（spec §2.2：manifest fs 只允许 48000/96000）。
pub const DEFAULT_FS: f64 = 48_000.0;
/// 参考构造的 Kaiser β（spec §3.2 参考构造；V17）。
pub const DEFAULT_KAISER_BETA: f64 = 8.96;
/// 参考构造的幅度 floor（−120 dB，spec §3.2）。
pub const DEFAULT_MAG_FLOOR_DB: f64 = -120.0;
/// 参考构造的原型合成网格 N（spec §3.2：32768）。
pub const DEFAULT_PROTOTYPE_TAPS: usize = 32_768;
/// 参考构造的倒谱 FFT 尺寸（spec §3.2：32768）。
pub const DEFAULT_CEPSTRUM_FFT: usize = 32_768;

/// 支撑下限（spec §3.1 判据 8：L ∈ [8,16384]）。
pub const MIN_TAPS: usize = 8;
/// 常规/扩展准入上限（spec §7.2：>16384 仅 offline 设计工具产物，runtime 不放行）。
pub const MAX_TAPS: usize = 16_384;
/// 测量网格/通带下界（spec §1.2：20 Hz）。
pub const MIN_FREQ_HZ: f64 = 20.0;
/// 声明阻带衰减下限（spec §3.2 参数：attenuation_db >= 60）。
pub const MIN_ATTENUATION_DB: f64 = 60.0;
/// 幅度 floor 下限（本 leaf 实现界限；比 spec 默认 −120 dB 更深的 floor 无实际意义）。
pub const MIN_MAG_FLOOR_DB: f64 = -240.0;
/// 幅度 floor 上限（spec 默认 −120；−60 以上会把通带/阻带要求压得过松，拒绝）。
pub const MAX_MAG_FLOOR_DB: f64 = -60.0;
/// 过渡带最小宽度（spec §3.1 判据 8 共用：f_s−f_p >= 30 Hz）。
pub const MIN_TRANSITION_HZ: f64 = 30.0;
/// 家族边界 1（spec §3.2 判据 4：f_s/f_p <= 1.5）。
pub const FAMILY_EDGE_RATIO: f64 = 1.5;
/// 家族边界 2（spec §3.2 判据 4：f_s <= 0.45*fs）。
pub const FAMILY_EDGE_NYQUIST: f64 = 0.45;
/// 频率上限系数（spec §1.2/§3.1：<= 0.499*fs）。
pub const MAX_FREQ_NYQUIST: f64 = 0.499;
/// 通带纹波参数上限（本 leaf 实现界限：>6 dB 不再是“可用的目标型”设计）。
pub const MAX_RIPPLE_DB: f64 = 6.0;
/// 原型网格/倒谱 FFT 的合法下限（2 的幂；spec 参考构造 32768）。
pub const MIN_GRID_SIZE: usize = 4_096;
/// 原型网格/倒谱 FFT 的合法上限（2 的幂）。
pub const MAX_GRID_SIZE: usize = 262_144;
/// 响应/群延迟复算的 FFT 尺寸（spec §1.2 测量程序；>= max(8192, 8L) 对 L<=16384 足够）。
pub const RESPONSE_FFT: usize = 65_536;
/// 最小相位验证 M2 的 FFT 尺寸（spec §3.2 判据 2：N=65536）。
pub const MIN_PHASE_FFT: usize = 65_536;
/// M2 相位–Hilbert 一致性上限（spec §3.2 判据 2：<= 2.0°）。
pub const MIN_PHASE_MAX_DEG: f64 = 2.0;
/// M2 log|H| 下限（spec §3.2 判据 2：−120 dB）。
pub const MIN_PHASE_LOG_FLOOR_DB: f64 = -120.0;
/// M2 比较频带下界（spec §3.2 判据 2：20 Hz）。
pub const MIN_PHASE_BAND_LO_HZ: f64 = 20.0;
/// M2 比较频带上界系数（spec §3.2 判据 2：0.45fs）。
pub const MIN_PHASE_BAND_HI_RATIO: f64 = 0.45;
/// 有效支撑 eps（spec §1.3：|h[n]| > 1e-6*max|h|）。
pub const EFFECTIVE_SUPPORT_EPS: f64 = 1e-6;
/// 振铃排除窗（spec §1.4：ceil(0.000333*fs)）。
pub const RING_WINDOW_SEC: f64 = 0.000333;
/// 参考归一化容差（spec §1.5/§3.1 判据 6：<= 0.01 dB）。
pub const REFERENCE_TOL_DB: f64 = 0.01;
/// 通带纹波判据附加容差（spec §3.1 判据 3：+0.05 dB）。
pub const RIPPLE_TOL_DB: f64 = 0.05;
/// 阻带衰减判据附加容差（spec §3.1 判据 4：−(A_s−0.5)）。
pub const ATTENUATION_TOL_DB: f64 = 0.5;
/// 20 kHz 端点（spec §2.4）。
pub const ENDPOINT_20K_HZ: f64 = 20_000.0;
/// 参考点：DC（spec §1.5：hz=0 表示 DC）。
pub const REFERENCE_HZ_DC: f64 = 0.0;

// ---------------------------------------------------------------------------
// 错误码（spec §2.5 E_FIR_* 集中表；本 leaf 只产生与设计阶段相关的子集）
// ---------------------------------------------------------------------------

/// 参数/manifest 结构非法（网格尺寸非 2 的幂等；spec §2.5）。
pub const E_FIR_SCHEMA: &str = "E_FIR_SCHEMA";
/// 频带参数非法（顺序/越界/过渡带过窄；spec §2.5）。
pub const E_FIR_BAND: &str = "E_FIR_BAND";
/// 家族自洽失败（min_fast 边界 f_s/f_p>1.5 或 f_s>0.45fs；未实现分解方法；spec §2.5）。
pub const E_FIR_FAMILY: &str = "E_FIR_FAMILY";
/// 支撑/taps 或参数组合 INFEASIBLE（不静默截断；spec §2.5）。
pub const E_FIR_BUDGET: &str = "E_FIR_BUDGET";
/// 数值非有限 / max|h|=0（spec §2.5）。
pub const E_FIR_NUMERIC: &str = "E_FIR_NUMERIC";
/// taps 越界（spec §2.5）。
pub const E_FIR_TAPS: &str = "E_FIR_TAPS";
/// 参考归一化超出 ±0.01 dB（spec §2.5）。
pub const E_FIR_REF: &str = "E_FIR_REF";
/// 最小相位验证 M2 失败（spec §2.5）。
pub const E_FIR_MINPHASE: &str = "E_FIR_MINPHASE";
/// 端点缺失/status 非法（spec §2.5）。
pub const E_FIR_ENDPOINT: &str = "E_FIR_ENDPOINT";
/// 延迟声明与家族规则不符（spec §2.5）。
pub const E_FIR_DELAY: &str = "E_FIR_DELAY";

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// 设计器错误：稳定错误码（spec §2.5 的 E_FIR_* 子集）+ 可读消息。
#[derive(Debug, Clone, PartialEq)]
pub struct MinFastError {
    pub code: &'static str,
    pub message: String,
}

impl MinFastError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for MinFastError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MinFastError {}

fn err<T>(code: &'static str, message: impl Into<String>) -> Result<T, MinFastError> {
    Err(MinFastError::new(code, message))
}

// ---------------------------------------------------------------------------
// 小工具（纯函数，全部离线 f64）
// ---------------------------------------------------------------------------

fn is_pow2(n: usize) -> bool {
    n != 0 && (n & (n - 1)) == 0
}

/// 第一类修正 Bessel 函数 I0（Kaiser 窗；级数展开，x<=20 时 120 项足够）。
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0f64;
    let mut term = 1.0f64;
    let q = (x * 0.5) * (x * 0.5);
    for k in 1..=120u32 {
        let kf = f64::from(k);
        term *= q / (kf * kf);
        sum += term;
    }
    sum
}

/// Kaiser 窗（长度 len，β；与 spec §3.2/§9 参考构造一致）。
fn kaiser_window(len: usize, beta: f64) -> Vec<f64> {
    if len == 0 {
        return Vec::new();
    }
    let m = (len - 1) as f64;
    let denom = bessel_i0(beta);
    (0..len)
        .map(|n| {
            let t = if m == 0.0 {
                0.0
            } else {
                2.0 * n as f64 / m - 1.0
            };
            let arg = beta * (1.0 - t * t).max(0.0).sqrt();
            bessel_i0(arg) / denom
        })
        .collect()
}

fn db_from_power(p: f64) -> f64 {
    10.0 * p.max(1e-24).log10()
}

// ---------------------------------------------------------------------------
// 迭代式 radix-2 Cooley–Tukey FFT（f64，离线设计工具内部实现；无第三方依赖）
// ---------------------------------------------------------------------------

/// 固定长度 radix-2 FFT 计划（bit-reversal 表 + 逐级旋转因子）。
#[derive(Debug)]
struct Fft {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<Vec<(f64, f64)>>,
}

impl Fft {
    fn new(n: usize) -> Result<Self, MinFastError> {
        if !is_pow2(n) || n < 2 {
            return err(
                E_FIR_SCHEMA,
                format!("FFT 尺寸 {n} 非法：必须是 2 的幂且 >= 2"),
            );
        }
        let bits = n.trailing_zeros();
        let mut rev = vec![0u32; n];
        for (i, r) in rev.iter_mut().enumerate() {
            *r = (i as u32).reverse_bits() >> (32 - bits);
        }
        let mut tw = Vec::with_capacity(bits as usize);
        let mut len = 2usize;
        while len <= n {
            let half = len / 2;
            let mut stage = Vec::with_capacity(half);
            for k in 0..half {
                let ang = -2.0 * std::f64::consts::PI * (k as f64) / (len as f64);
                stage.push((ang.cos(), ang.sin()));
            }
            tw.push(stage);
            len <<= 1;
        }
        Ok(Self { n, rev, tw })
    }

    fn forward(&self, re: &mut [f64], im: &mut [f64]) {
        debug_assert_eq!(re.len(), self.n);
        debug_assert_eq!(im.len(), self.n);
        for i in 0..self.n {
            let j = self.rev[i] as usize;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2usize;
        for stage in &self.tw {
            let half = len / 2;
            for start in (0..self.n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = stage[k];
                    let a = start + k;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }

    fn inverse(&self, re: &mut [f64], im: &mut [f64]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.forward(re, im);
        let inv = 1.0 / self.n as f64;
        for v in re.iter_mut() {
            *v *= inv;
        }
        for v in im.iter_mut() {
            *v = -*v * inv;
        }
    }
}

// ---------------------------------------------------------------------------
// 直接响应复算（O(L)，相位递推避免逐点三角函数；用于端点/特征频点/过渡带二分）
// ---------------------------------------------------------------------------

/// 直接计算复频响 H(f)（(re, im)；f64，零填充线性相位定义，spec §1.2）。
pub fn frequency_response(h: &[f64], fs: f64, f_hz: f64) -> (f64, f64) {
    let w = -2.0 * std::f64::consts::PI * f_hz / fs;
    let (dw_r, dw_i) = (w.cos(), w.sin());
    let (mut cr, mut ci) = (1.0f64, 0.0f64);
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for &hn in h {
        re += hn * cr;
        im += hn * ci;
        let nr = cr * dw_r - ci * dw_i;
        let ni = cr * dw_i + ci * dw_r;
        cr = nr;
        ci = ni;
    }
    (re, im)
}

/// 幅度（dB，20log10；|H| 下限 1e-12 即 −240 dB，spec §1.2）。
pub fn magnitude_db(h: &[f64], fs: f64, f_hz: f64) -> f64 {
    let (re, im) = frequency_response(h, fs, f_hz);
    db_from_power((re * re + im * im).max(1e-24))
}

/// 相位（度，atan2 分支；spec §1.2）。
pub fn phase_deg(h: &[f64], fs: f64, f_hz: f64) -> f64 {
    let (re, im) = frequency_response(h, fs, f_hz);
    im.atan2(re).to_degrees()
}

/// 解析群延迟（samples）：GD(f) = Re(G(f)/H(f))，G(f)=Σ n·h[n]·e^{−jωn}
/// （等价于 −(1/2π)·dφ/df，无差分步长误差；spec §1.2 允许中心差分实现）。
pub fn group_delay_samples_exact(h: &[f64], fs: f64, f_hz: f64) -> f64 {
    let w = -2.0 * std::f64::consts::PI * f_hz / fs;
    let (dw_r, dw_i) = (w.cos(), w.sin());
    let (mut cr, mut ci) = (1.0f64, 0.0f64);
    let (mut hr, mut hi) = (0.0f64, 0.0f64);
    let (mut gr, mut gi) = (0.0f64, 0.0f64);
    for (n, &hn) in h.iter().enumerate() {
        hr += hn * cr;
        hi += hn * ci;
        let nf = n as f64;
        gr += nf * hn * cr;
        gi += nf * hn * ci;
        let nr = cr * dw_r - ci * dw_i;
        let ni = cr * dw_i + ci * dw_r;
        cr = nr;
        ci = ni;
    }
    let denom = hr * hr + hi * hi;
    if denom < 1e-30 {
        return f64::NAN;
    }
    (gr * hr + gi * hi) / denom
}

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

/// 原型幅度类型（spec §3.2 参数 prototype.magnitude）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrototypeMagnitude {
    /// 升余弦（spec §3.2 参考构造；推荐）。
    RaisedCosine,
    /// 线性幅度过渡（spec §3.2 允许的替代；实测阻带/过渡通常差于升余弦）。
    Linear,
}

impl PrototypeMagnitude {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RaisedCosine => "raised_cosine",
            Self::Linear => "linear",
        }
    }
}

/// 最小相位谱分解方法（spec §3.2 参数 factorization）。
///
/// 规范允许 cepstrum（参考构造）或等价的根配对；本 leaf 实现 cepstrum，
/// Roots 以 E_FIR_FAMILY 显式拒绝（不静默降级、不冒充已实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Factorization {
    Cepstrum,
    Roots,
}

impl Factorization {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cepstrum => "cepstrum",
            Self::Roots => "roots",
        }
    }
}

/// 原型合成参数（spec §3.2 prototype: {magnitude, taps, window: kaiser, beta}）。
#[derive(Debug, Clone, PartialEq)]
pub struct PrototypeSpec {
    pub magnitude: PrototypeMagnitude,
    /// 原型合成网格 N（2 的幂；spec 参考构造 32768）。
    pub taps: usize,
    /// Kaiser β（窗固定 kaiser；spec 参考构造 8.96）。
    pub beta: f64,
}

impl Default for PrototypeSpec {
    fn default() -> Self {
        Self {
            magnitude: PrototypeMagnitude::RaisedCosine,
            taps: DEFAULT_PROTOTYPE_TAPS,
            beta: DEFAULT_KAISER_BETA,
        }
    }
}

/// min_fast 设计参数（spec §3.2）。
#[derive(Debug, Clone, PartialEq)]
pub struct MinFastParams {
    /// 运行采样率 Hz（manifest 只允许 48000/96000；本 leaf 接受 8000..=192000 并警告）。
    pub fs: f64,
    /// 系数支撑长度 L（spec §3.1 判据 8：8..=16384）。
    pub taps: usize,
    /// 通带 [lo, f_p]（Hz；lo >= 20，f_p > 20）。
    pub passband_hz: [f64; 2],
    /// 阻带 [f_s, hi]（Hz；hi <= 0.499fs；f_s 用于家族边界与幅度目标）。
    pub stopband_hz: [f64; 2],
    /// 通带纹波 p-p 声明（dB，spec §3.2 默认 0.5）。
    pub ripple_db: f64,
    /// 阻带衰减声明（dB，>= 60；<= −mag_floor_db）。
    pub attenuation_db: f64,
    /// 原型合成参数。
    pub prototype: PrototypeSpec,
    /// 最小相位分解方法（本 leaf 仅 cepstrum）。
    pub factorization: Factorization,
    /// 幅度 floor（dB，默认 −120）。
    pub mag_floor_db: f64,
    /// 倒谱 FFT 尺寸（2 的幂，默认 32768）。
    pub cepstrum_fft: usize,
}

impl Default for MinFastParams {
    fn default() -> Self {
        Self {
            fs: DEFAULT_FS,
            taps: 2_048,
            passband_hz: [MIN_FREQ_HZ, 16_000.0],
            stopband_hz: [19_000.0, MAX_FREQ_NYQUIST * DEFAULT_FS],
            ripple_db: 0.5,
            attenuation_db: 80.0,
            prototype: PrototypeSpec::default(),
            factorization: Factorization::Cepstrum,
            mag_floor_db: DEFAULT_MAG_FLOOR_DB,
            cepstrum_fft: DEFAULT_CEPSTRUM_FFT,
        }
    }
}

impl MinFastParams {
    /// V17 参考构造（spec §9 V17 / evidence/T04）：L=2048、f_p=16k、f_s=19k、β=8.96、
    /// N=32768；额外声明 attenuation_db=80（用于规范判据）。
    pub fn v17_reference() -> Self {
        Self::default()
    }

    /// 参数校验（spec §3.2 参数 + 判据 4/5 + §3.1 判据 8/10 + §7.2 支撑边界）。
    pub fn validate(&self) -> Result<(), MinFastError> {
        validate_params(self)
    }
}

/// 参数校验（公开函数；design() 会先调用）。
pub fn validate_params(p: &MinFastParams) -> Result<(), MinFastError> {
    if !p.fs.is_finite() || p.fs < 8_000.0 || p.fs > 192_000.0 {
        return err(
            E_FIR_BAND,
            format!(
                "fs={} 非法：需有限且 8000..=192000 Hz（spec §1.1/§2.2）",
                p.fs
            ),
        );
    }
    if p.taps < MIN_TAPS {
        return err(
            E_FIR_TAPS,
            format!(
                "taps={} 非法：min_fast 支撑下限 {MIN_TAPS}（spec §3.1 判据 8）",
                p.taps
            ),
        );
    }
    if p.taps > MAX_TAPS {
        return err(
            E_FIR_BUDGET,
            format!(
                "taps={} 超准入上限 {MAX_TAPS}：>16384 仅 offline 设计工具产物，runtime 不放行（spec §7.2）",
                p.taps
            ),
        );
    }
    let [lo, fp] = p.passband_hz;
    let [fstop_lo, fstop_hi] = p.stopband_hz;
    for (name, v) in [
        ("passband.lo", lo),
        ("passband.f_p", fp),
        ("stopband.lo", fstop_lo),
        ("stopband.hi", fstop_hi),
    ] {
        if !v.is_finite() {
            return err(E_FIR_BAND, format!("{name}={v} 非有限数"));
        }
    }
    if lo < MIN_FREQ_HZ {
        return err(
            E_FIR_BAND,
            format!("passband.lo={lo} < {MIN_FREQ_HZ} Hz（spec §1.2 测量下界）"),
        );
    }
    if fp <= MIN_FREQ_HZ || fp <= lo {
        return err(
            E_FIR_BAND,
            format!(
                "passband=[{lo},{fp}] 非法：需 {MIN_FREQ_HZ} < f_p 且 lo < f_p（spec §3.1 判据 8）"
            ),
        );
    }
    if fstop_lo <= fp {
        return err(
            E_FIR_BAND,
            format!("stopband.lo={fstop_lo} <= f_p={fp}：过渡带次序非法（spec §3.1 判据 8）"),
        );
    }
    if fstop_lo - fp < MIN_TRANSITION_HZ {
        return err(
            E_FIR_BAND,
            format!(
                "过渡带 f_s−f_p={} < {MIN_TRANSITION_HZ} Hz（spec §3.1 判据 8）",
                fstop_lo - fp
            ),
        );
    }
    let f_max = MAX_FREQ_NYQUIST * p.fs;
    if fstop_lo > f_max {
        return err(
            E_FIR_BAND,
            format!("stopband.lo={fstop_lo} > 0.499*fs={f_max:.3}（spec §1.2/§3.1）"),
        );
    }
    if fstop_hi < fstop_lo || fstop_hi > f_max {
        return err(
            E_FIR_BAND,
            format!("stopband.hi={fstop_hi} 非法：需 f_s <= hi <= 0.499*fs={f_max:.3}"),
        );
    }
    let ratio = fstop_lo / fp;
    if ratio > FAMILY_EDGE_RATIO {
        return err(
            E_FIR_FAMILY,
            format!(
                "f_s/f_p={ratio:.6} > {FAMILY_EDGE_RATIO}：不属于 min_fast 家族边界，应改 linear/brickwall（spec §3.2 判据 4）"
            ),
        );
    }
    if fstop_lo > FAMILY_EDGE_NYQUIST * p.fs {
        return err(
            E_FIR_FAMILY,
            format!(
                "f_s={fstop_lo} > 0.45*fs={:.3}：不属于 min_fast 家族边界（spec §3.2 判据 4）",
                FAMILY_EDGE_NYQUIST * p.fs
            ),
        );
    }
    if !p.ripple_db.is_finite() || p.ripple_db <= 0.0 || p.ripple_db > MAX_RIPPLE_DB {
        return err(
            E_FIR_BAND,
            format!(
                "ripple_db={} 非法：需 (0, {MAX_RIPPLE_DB}] dB（spec §3.2 默认 0.5）",
                p.ripple_db
            ),
        );
    }
    if !p.mag_floor_db.is_finite()
        || p.mag_floor_db < MIN_MAG_FLOOR_DB
        || p.mag_floor_db > MAX_MAG_FLOOR_DB
    {
        return err(
            E_FIR_BAND,
            format!(
                "mag_floor_db={} 非法：需 [{MIN_MAG_FLOOR_DB}, {MAX_MAG_FLOOR_DB}] dB（spec §3.2 默认 −120）",
                p.mag_floor_db
            ),
        );
    }
    let max_att = -p.mag_floor_db;
    if !p.attenuation_db.is_finite()
        || p.attenuation_db < MIN_ATTENUATION_DB
        || p.attenuation_db > max_att
    {
        return err(
            E_FIR_BAND,
            format!(
                "attenuation_db={} 非法：需 [{MIN_ATTENUATION_DB}, {max_att}] dB（>=60；<= −mag_floor_db）",
                p.attenuation_db
            ),
        );
    }
    if !is_pow2(p.prototype.taps)
        || p.prototype.taps < MIN_GRID_SIZE
        || p.prototype.taps > MAX_GRID_SIZE
    {
        return err(
            E_FIR_SCHEMA,
            format!(
                "prototype.taps={} 非法：需 2 的幂且 [{MIN_GRID_SIZE}, {MAX_GRID_SIZE}]（spec §3.2 N=32768）",
                p.prototype.taps
            ),
        );
    }
    if p.prototype.taps < 2 * p.taps {
        return err(
            E_FIR_BUDGET,
            format!(
                "prototype.taps={} < 2*taps={}：原型合成网格不足，别名会破坏幅度目标（增大 prototype.taps）",
                p.prototype.taps,
                2 * p.taps
            ),
        );
    }
    if !p.prototype.beta.is_finite() || p.prototype.beta < 0.0 || p.prototype.beta > 20.0 {
        return err(
            E_FIR_BAND,
            format!(
                "prototype.beta={} 非法：需 [0, 20]（spec §3.2 参考 beta=8.96）",
                p.prototype.beta
            ),
        );
    }
    if p.factorization != Factorization::Cepstrum {
        return err(
            E_FIR_FAMILY,
            format!(
                "factorization={}：本 leaf 只实现 cepstrum（spec §3.2 参考构造；roots 为 MAY 的等价方法，未实现即拒绝）",
                p.factorization.as_str()
            ),
        );
    }
    if !is_pow2(p.cepstrum_fft) || p.cepstrum_fft < MIN_GRID_SIZE || p.cepstrum_fft > MAX_GRID_SIZE
    {
        return err(
            E_FIR_SCHEMA,
            format!(
                "cepstrum_fft={} 非法：需 2 的幂且 [{MIN_GRID_SIZE}, {MAX_GRID_SIZE}]（spec §3.2 N=32768）",
                p.cepstrum_fft
            ),
        );
    }
    if p.cepstrum_fft < 2 * p.taps {
        return err(
            E_FIR_BUDGET,
            format!(
                "cepstrum_fft={} < 2*taps={}：倒谱会截断/混叠系数支撑（增大 cepstrum_fft）",
                p.cepstrum_fft,
                2 * p.taps
            ),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 幅度目标与原型合成（spec §3.2 参考构造步骤 1–2）
// ---------------------------------------------------------------------------

/// 目标幅度（线性幅度，非 dB）：[lo,f_p] 为 1，[f_p,f_s] 按原型类型过渡到 floor，>=f_s 为 floor。
pub fn prototype_target_magnitude(p: &MinFastParams, f_hz: f64) -> f64 {
    let fp = p.passband_hz[1];
    let fst = p.stopband_hz[0];
    let floor = 10f64.powf(p.mag_floor_db / 20.0);
    if f_hz <= fp {
        1.0
    } else if f_hz >= fst {
        floor
    } else {
        let t = (f_hz - fp) / (fst - fp);
        match p.prototype.magnitude {
            PrototypeMagnitude::RaisedCosine => {
                floor.max(0.5 * (1.0 + (std::f64::consts::PI * t).cos()))
            }
            PrototypeMagnitude::Linear => 1.0 + (floor - 1.0) * t,
        }
    }
}

/// 对称线性相位原型合成（spec §3.2 步骤 2：N 网格、D=(L−1)/2、Kaiser β、DC 归一）。
pub fn symmetric_prototype(p: &MinFastParams) -> Result<Vec<f64>, MinFastError> {
    validate_params(p)?;
    let n = p.prototype.taps;
    let l = p.taps;
    let d = (l as f64 - 1.0) / 2.0;
    let fft = Fft::new(n)?;
    let half = n / 2;
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    for k in 0..=half {
        let f = k as f64 * p.fs / n as f64;
        let m = prototype_target_magnitude(p, f);
        if k == 0 || k == half {
            // DC 与 Nyquist 必须是实 bin（Nyquist 只取 cos(π(n−D)) 分量）。
            re[k] = if k == 0 {
                m
            } else {
                m * (std::f64::consts::PI * d).cos()
            };
            im[k] = 0.0;
        } else {
            let ph = -2.0 * std::f64::consts::PI * (k as f64) * d / n as f64;
            re[k] = m * ph.cos();
            im[k] = m * ph.sin();
            re[n - k] = re[k];
            im[n - k] = -im[k];
        }
    }
    fft.inverse(&mut re, &mut im);
    let mut h: Vec<f64> = re[..l].to_vec();
    let win = kaiser_window(l, p.prototype.beta);
    for (hv, wv) in h.iter_mut().zip(win.iter()) {
        *hv *= *wv;
    }
    let dc: f64 = h.iter().sum();
    if !dc.is_finite() || dc.abs() < 1e-15 {
        return err(
            E_FIR_NUMERIC,
            format!("原型 DC 归一失败：sum(h_proto)={dc}（幅度目标/窗参数异常）"),
        );
    }
    for hv in h.iter_mut() {
        *hv /= dc;
    }
    Ok(h)
}

// ---------------------------------------------------------------------------
// 同态最小相位谱分解（spec §3.2 步骤 3）与最小相位验证 M2（判据 2）
// ---------------------------------------------------------------------------

fn fold_real_cepstrum(c: &[f64], n: usize) -> Vec<f64> {
    let mut f = vec![0.0f64; n];
    f[0] = c[0];
    for k in 1..n / 2 {
        f[k] = 2.0 * c[k];
    }
    f[n / 2] = c[n / 2];
    f
}

/// 由线性相位原型做同态最小相位谱分解（spec §3.2 步骤 3），返回前 proto.len() 点。
pub fn min_phase_coefficients(
    prototype: &[f64],
    cepstrum_fft: usize,
    mag_floor_db: f64,
) -> Result<Vec<f64>, MinFastError> {
    if prototype.is_empty() {
        return err(E_FIR_NUMERIC, "原型为空，无法做最小相位谱分解");
    }
    if !is_pow2(cepstrum_fft) || cepstrum_fft < 2 * prototype.len() {
        return err(
            E_FIR_BUDGET,
            format!(
                "cepstrum_fft={cepstrum_fft} 非法：需 2 的幂且 >= 2*len(prototype)={}（避免倒谱截断）",
                2 * prototype.len()
            ),
        );
    }
    for (i, v) in prototype.iter().enumerate() {
        if !v.is_finite() {
            return err(E_FIR_NUMERIC, format!("原型系数 h_proto[{i}]={v} 非有限数"));
        }
    }
    let n = cepstrum_fft;
    let fft = Fft::new(n)?;
    let floor_amp = 10f64.powf(mag_floor_db / 20.0);
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    re[..prototype.len()].copy_from_slice(prototype);
    fft.forward(&mut re, &mut im);
    // 实倒谱：log|H|（下限 floor）→ IFFT
    let mut cr: Vec<f64> = (0..n)
        .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt().max(floor_amp).ln())
        .collect();
    let mut ci = vec![0.0f64; n];
    fft.inverse(&mut cr, &mut ci);
    // 折叠：c[0], 2c[1..N/2−1], c[N/2]
    let mut fr = fold_real_cepstrum(&cr, n);
    let mut fi = vec![0.0f64; n];
    fft.forward(&mut fr, &mut fi);
    // exp(复杂度谱) → 最小相位谱 → IFFT，取前 L 点；不做峰值归一化（spec §3.2 步骤 3）
    let mut sr = vec![0.0f64; n];
    let mut si = vec![0.0f64; n];
    for k in 0..n {
        let e = fr[k].exp();
        sr[k] = e * fi[k].cos();
        si[k] = e * fi[k].sin();
    }
    fft.inverse(&mut sr, &mut si);
    Ok(sr[..prototype.len()].to_vec())
}

/// 由已算好的 H(k) 计算 M2 相位–Hilbert 一致性（度）。re/im 为 h 补零到 N 的 FFT。
fn min_phase_consistency_from_spectrum(
    re: &[f64],
    im: &[f64],
    fs: f64,
    fft: &Fft,
) -> Result<f64, MinFastError> {
    let n = re.len();
    if n < 16 || !is_pow2(n) {
        return err(E_FIR_SCHEMA, format!("M2 FFT 尺寸 {n} 非法"));
    }
    let floor_amp = 10f64.powf(MIN_PHASE_LOG_FLOOR_DB / 20.0);
    let mut lm = vec![0.0f64; n];
    let mut ph = vec![0.0f64; n];
    for k in 0..n {
        lm[k] = (re[k] * re[k] + im[k] * im[k]).sqrt().max(floor_amp).ln();
        ph[k] = im[k].atan2(re[k]);
    }
    // 相位展开（spec §1.2：连续分支）
    for k in 1..n {
        let mut d = ph[k] - ph[k - 1];
        while d > std::f64::consts::PI {
            d -= 2.0 * std::f64::consts::PI;
        }
        while d < -std::f64::consts::PI {
            d += 2.0 * std::f64::consts::PI;
        }
        ph[k] = ph[k - 1] + d;
    }
    // 实倒谱折叠 → FFT 的虚部即最小相位谱的相位（spec §3.2 判据 2）
    let mut cr = lm;
    let mut ci = vec![0.0f64; n];
    fft.inverse(&mut cr, &mut ci);
    let mut fr = fold_real_cepstrum(&cr, n);
    let mut fi = vec![0.0f64; n];
    fft.forward(&mut fr, &mut fi);
    let mut max_deg = 0.0f64;
    for k in 1..n / 2 {
        let f = k as f64 * fs / n as f64;
        if f < MIN_PHASE_BAND_LO_HZ || f > MIN_PHASE_BAND_HI_RATIO * fs {
            continue;
        }
        let mut d = ph[k] - fi[k];
        while d > std::f64::consts::PI {
            d -= 2.0 * std::f64::consts::PI;
        }
        while d < -std::f64::consts::PI {
            d += 2.0 * std::f64::consts::PI;
        }
        max_deg = max_deg.max(d.abs().to_degrees());
    }
    if !max_deg.is_finite() {
        return err(E_FIR_MINPHASE, "M2 一致性计算结果非有限数");
    }
    Ok(max_deg)
}

/// 公开 M2 实现：对系数 h 补零到 n_fft，比较 H(k) 展开相位与 log|H| 倒谱折叠得到的
/// 最小相位（spec §3.2 判据 2；参考构造默认 N=65536）。
pub fn min_phase_consistency_deg(h: &[f64], fs: f64, n_fft: usize) -> Result<f64, MinFastError> {
    if h.is_empty() {
        return err(E_FIR_NUMERIC, "h 为空");
    }
    if !fs.is_finite() || fs <= 0.0 {
        return err(E_FIR_BAND, format!("fs={fs} 非法"));
    }
    if !is_pow2(n_fft) || n_fft < 2 * h.len() {
        return err(
            E_FIR_BUDGET,
            format!(
                "n_fft={n_fft} 非法：需 2 的幂且 >= 2*len(h)={}（避免截断/适配混叠）",
                2 * h.len()
            ),
        );
    }
    for (i, v) in h.iter().enumerate() {
        if !v.is_finite() {
            return err(E_FIR_NUMERIC, format!("h[{i}]={v} 非有限数"));
        }
    }
    let fft = Fft::new(n_fft)?;
    let mut re = vec![0.0f64; n_fft];
    let mut im = vec![0.0f64; n_fft];
    re[..h.len()].copy_from_slice(h);
    fft.forward(&mut re, &mut im);
    min_phase_consistency_from_spectrum(&re, &im, fs, &fft)
}

/// 逆因果性诊断（spec §3.2 判据 2：MAY，仅诊断、不作 PASS/FAIL 门）。
pub fn inverse_causality_diagnostic_db(
    h: &[f64],
    n_fft: usize,
    floor_db: f64,
) -> Result<f64, MinFastError> {
    if !is_pow2(n_fft) || n_fft < 2 * h.len() {
        return err(
            E_FIR_BUDGET,
            format!("n_fft={n_fft} 非法（需 2 的幂且 >= 2*len(h)）"),
        );
    }
    let fft = Fft::new(n_fft)?;
    let mut re = vec![0.0f64; n_fft];
    let mut im = vec![0.0f64; n_fft];
    re[..h.len()].copy_from_slice(h);
    fft.forward(&mut re, &mut im);
    let mut max_h = 0.0f64;
    for k in 0..n_fft {
        max_h = max_h.max((re[k] * re[k] + im[k] * im[k]).sqrt());
    }
    if max_h <= 0.0 {
        return err(E_FIR_NUMERIC, "max|H|=0，无法计算逆滤波诊断");
    }
    let floor = max_h * 10f64.powf(floor_db / 20.0);
    let mut gr = vec![0.0f64; n_fft];
    let mut gi = vec![0.0f64; n_fft];
    for k in 0..n_fft {
        let m2 = (re[k] * re[k] + im[k] * im[k]).max(floor * floor);
        gr[k] = re[k] / m2;
        gi[k] = -im[k] / m2;
    }
    fft.inverse(&mut gr, &mut gi);
    let mut causal = 0.0f64;
    let mut anticausal = 0.0f64;
    for n in 1..=n_fft / 2 {
        causal = causal.max((gr[n] * gr[n] + gi[n] * gi[n]).sqrt());
    }
    for n in n_fft / 2 + 1..n_fft {
        anticausal = anticausal.max((gr[n] * gr[n] + gi[n] * gi[n]).sqrt());
    }
    Ok(20.0 * ((anticausal + 1e-300) / (causal + 1e-300)).log10())
}

// ---------------------------------------------------------------------------
// 响应谱（FFT 网格；幅度/群延迟，供测量网格与报告使用）
// ---------------------------------------------------------------------------

/// 补零 FFT 响应谱：幅度 dB、展开相位（仅内部）、中心差分群延迟，按 bin 线性插值。
#[derive(Debug, Clone)]
struct ResponseSpectrum {
    fs: f64,
    n: usize,
    half: usize,
    mag_db: Vec<f64>,
    gd: Vec<f64>,
}

impl ResponseSpectrum {
    fn new(h: &[f64], fs: f64, n: usize) -> Result<Self, MinFastError> {
        if !is_pow2(n) || n < 2 * h.len() {
            return err(
                E_FIR_BUDGET,
                format!(
                    "响应 FFT 尺寸 {n} 非法：需 2 的幂且 >= 2*len(h)={}",
                    2 * h.len()
                ),
            );
        }
        let fft = Fft::new(n)?;
        let mut re = vec![0.0f64; n];
        let mut im = vec![0.0f64; n];
        re[..h.len()].copy_from_slice(h);
        fft.forward(&mut re, &mut im);
        let half = n / 2;
        let mut mag_db = vec![0.0f64; half + 1];
        let mut ph = vec![0.0f64; n];
        for k in 0..=half {
            mag_db[k] = db_from_power(re[k] * re[k] + im[k] * im[k]);
            ph[k] = im[k].atan2(re[k]);
        }
        for k in 1..=half {
            let mut d = ph[k] - ph[k - 1];
            while d > std::f64::consts::PI {
                d -= 2.0 * std::f64::consts::PI;
            }
            while d < -std::f64::consts::PI {
                d += 2.0 * std::f64::consts::PI;
            }
            ph[k] = ph[k - 1] + d;
        }
        let mut gd = vec![0.0f64; half + 1];
        for k in 1..half {
            gd[k] = -(ph[k + 1] - ph[k - 1]) * n as f64 / (4.0 * std::f64::consts::PI);
        }
        gd[0] = gd[1];
        gd[half] = gd[half - 1];
        Ok(Self {
            fs,
            n,
            half,
            mag_db,
            gd,
        })
    }

    fn bin_pos(&self, f_hz: f64) -> (usize, f64) {
        let x = f_hz * self.n as f64 / self.fs;
        if x <= 0.0 {
            return (0, 0.0);
        }
        let k = x.floor();
        if k >= self.half as f64 {
            return (self.half, 0.0);
        }
        let i = k as usize;
        (i, x - k)
    }

    fn mag_db_at(&self, f_hz: f64) -> f64 {
        let (i, t) = self.bin_pos(f_hz);
        if i >= self.half {
            return self.mag_db[self.half];
        }
        self.mag_db[i] + (self.mag_db[i + 1] - self.mag_db[i]) * t
    }

    fn gd_at(&self, f_hz: f64) -> f64 {
        let (i, t) = self.bin_pos(f_hz);
        if i >= self.half {
            return self.gd[self.half];
        }
        self.gd[i] + (self.gd[i + 1] - self.gd[i]) * t
    }

    /// 阻带内 bin 的幅度最大值（dB；真正 H(k) 采样，不依赖插值）。
    fn max_mag_db_bins(&self, lo_hz: f64, hi_hz: f64) -> f64 {
        let df = self.fs / self.n as f64;
        let k0 = (lo_hz / df).ceil().max(0.0) as usize;
        let k1 = ((hi_hz / df).floor() as usize).min(self.half);
        let mut mx = f64::NEG_INFINITY;
        for k in k0..=k1 {
            mx = mx.max(self.mag_db[k]);
        }
        mx
    }
}

// ---------------------------------------------------------------------------
// 报告结构
// ---------------------------------------------------------------------------

/// 有效支撑（spec §1.3）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SupportReport {
    pub first_effective: usize,
    pub last_effective: usize,
    pub effective_taps: usize,
    pub max_abs: f64,
    pub peak_index: usize,
}

/// 振铃报告（spec §1.4；None = n/a，因主峰贴近支撑边界）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RingingReport {
    pub exclusion_samples: usize,
    pub pre_db: Option<f64>,
    pub post_db: Option<f64>,
    pub pre_energy_db: Option<f64>,
    pub post_energy_db: Option<f64>,
}

/// 通带幅度（spec §3.1 判据 3）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassbandReport {
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub min_db: f64,
    pub max_db: f64,
    pub ripple_db: f64,
}

/// 阻带幅度（spec §3.1 判据 4）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StopbandReport {
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub max_db: f64,
    pub attenuation_db: f64,
}

/// 过渡带（spec §3.1 判据 5）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionReport {
    pub target_attenuation_db: f64,
    pub edge_hz_at_attenuation: Option<f64>,
    pub width_hz: Option<f64>,
}

/// 端点联合类型（spec §2.4；20 kHz 在 0.499fs 不可达时写 not_applicable_at_fs）。
#[derive(Debug, Clone, PartialEq)]
pub enum Endpoint {
    Measured {
        freq_hz: f64,
        mag_db: f64,
        phase_deg: f64,
    },
    NotApplicableAtFs {
        freq_hz: f64,
        reason: &'static str,
    },
}

impl Endpoint {
    pub fn is_measured(&self) -> bool {
        matches!(self, Self::Measured { .. })
    }

    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Measured { .. } => "measured",
            Self::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }
}

/// 端点报告（spec §2.4：f20 与 f20k 必须单列）。
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointsReport {
    pub f20: Endpoint,
    pub f20k: Endpoint,
}

impl EndpointsReport {
    /// 两个端点都合法（measured 或 not_applicable_at_fs；spec §2.4 禁裸字符串/缺失）。
    pub fn is_complete(&self) -> bool {
        let ok = |e: &Endpoint| match e {
            Endpoint::Measured { .. } => true,
            Endpoint::NotApplicableAtFs { .. } => true,
        };
        ok(&self.f20) && ok(&self.f20k)
    }
}

/// 参考归一化（spec §1.5；DC 声明 0 dB，实测须在 ±0.01 dB 内）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReferenceReport {
    pub hz: f64,
    pub gain_db: f64,
    pub measured_gain_db: f64,
}

/// 幅度/相位/振铃/端点响应报告（对应 spec §2.2 response 子集，T55 消费）。
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseReport {
    pub grid_points: usize,
    pub passband: PassbandReport,
    pub stopband: StopbandReport,
    pub transition: TransitionReport,
    pub endpoints: EndpointsReport,
    pub ringing: RingingReport,
    pub reference: ReferenceReport,
}

/// 群延迟报告（spec §3.2 判据 3：频变 GD，禁止零延迟声明）。
#[derive(Debug, Clone, PartialEq)]
pub struct DelayReport {
    pub class: &'static str,
    /// 声明体延迟 D（samples）。min_fast 无体延迟，D=0；实际延迟看频变 GD/onset。
    pub declared_bulk_samples: f64,
    pub declared_bulk_ms: f64,
    /// 通带内 GD 最小值（samples）。
    pub passband_min_samples: f64,
    pub passband_max_samples: f64,
    pub passband_mean_samples: f64,
    /// GD(1 kHz)（spec §3.2 判据 3：latency_ms 用它）。
    pub at_1khz_samples: f64,
    /// latency_ms = GD(1 kHz)/fs*1000（spec §3.2 判据 3）。
    pub latency_ms_1khz: f64,
    /// 全测量带 [20 Hz, min(20000, 0.499fs)] 的 GD 最大值（含过渡带）。
    pub band_max_samples: f64,
    pub gd_max_ms: f64,
    /// onset latency = first_effective/fs*1000（spec §1.3：单列）。
    pub onset_samples: usize,
    pub onset_ms: f64,
    /// 特征频点 (Hz, samples)。
    pub characteristic_hz_samples: Vec<(f64, f64)>,
    /// 本家族恒为 false（spec §3.2 判据 3：禁止声称零延迟）。
    pub zero_delay_claim: bool,
}

/// 最小相位验证报告（spec §3.2 判据 2：M2 相位–Hilbert）。
#[derive(Debug, Clone, PartialEq)]
pub struct MinPhaseReport {
    pub method: &'static str,
    pub fft_size: usize,
    pub band_hz: [f64; 2],
    pub max_deviation_deg: f64,
    pub limit_deg: f64,
    pub passed: bool,
    /// MAY 诊断（不作 PASS/FAIL 门；spec §3.2 判据 2）。
    pub inverse_causality_diagnostic_db: f64,
    /// 零点半径检查（spec MAY）：本 leaf 未实现，明确登记 not_run。
    pub zero_radius_check: &'static str,
}

/// 极点/稳定性边界报告（spec §3.2 判据 5：FIR 不引入 IIR、本身稳定、检测 NaN/溢出）。
#[derive(Debug, Clone, PartialEq)]
pub struct StabilityReport {
    pub filter_class: &'static str,
    pub denominator_degree: usize,
    pub pole_count_at_origin: usize,
    pub max_pole_radius: f64,
    pub pole_stability_boundary: &'static str,
    pub unconditionally_stable: bool,
    pub iir_poles_introduced: bool,
    pub all_finite: bool,
    pub max_abs_coefficient: f64,
    pub dc_gain_linear: f64,
    pub numeric_violations: usize,
}

/// 单条规范判据结果（spec §3.2 判据 1–5 + §3.1 判据 3–6 + §1.3/§1.4/§2.4）。
#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    pub id: &'static str,
    pub statement: &'static str,
    pub measured: Option<f64>,
    pub limit: Option<f64>,
    pub unit: &'static str,
    pub pass: bool,
}

/// 规范判据汇总。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NormativeReport {
    pub criteria: Vec<Criterion>,
}

impl NormativeReport {
    pub fn all_pass(&self) -> bool {
        self.criteria.iter().all(|c| c.pass)
    }

    pub fn failed_ids(&self) -> Vec<&'static str> {
        self.criteria
            .iter()
            .filter(|c| !c.pass)
            .map(|c| c.id)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&Criterion> {
        self.criteria.iter().find(|c| c.id == id)
    }
}

/// 设计产物（系数 + 规范判据报告；WAV/manifest 导出由 T55 完成）。
#[derive(Debug, Clone, PartialEq)]
pub struct MinFastDesign {
    pub params: MinFastParams,
    /// 最小相位系数（长度 = taps；f64；不做峰值归一化）。
    pub coefficients: Vec<f64>,
    /// 线性相位原型系数（长度 = taps；供独立复算/证据使用）。
    pub prototype: Vec<f64>,
    pub support: SupportReport,
    pub response: ResponseReport,
    pub delay: DelayReport,
    pub min_phase: MinPhaseReport,
    pub stability: StabilityReport,
    pub criteria: NormativeReport,
    pub warnings: Vec<String>,
}

impl MinFastDesign {
    pub fn family(&self) -> &'static str {
        FAMILY
    }

    pub fn accepted(&self) -> bool {
        self.criteria.all_pass()
    }

    pub fn failed_criteria(&self) -> Vec<&'static str> {
        self.criteria.failed_ids()
    }

    /// 延迟声明（spec §3.2 判据 3：必须频变 GD 明示，不得声称零延迟）。
    pub fn delay_statement(&self) -> String {
        format!(
            "min_fast: frequency-varying group delay (NOT zero-delay); declared bulk D={:.1} samples; \
             GD(1kHz)={:.4} samples ({:.6} ms); onset={} samples ({:.6} ms); gd_max={:.2} samples ({:.4} ms)",
            self.delay.declared_bulk_samples,
            self.delay.at_1khz_samples,
            self.delay.latency_ms_1khz,
            self.delay.onset_samples,
            self.delay.onset_ms,
            self.delay.band_max_samples,
            self.delay.gd_max_ms,
        )
    }

    /// 人类可读摘要（证据日志/报告用；每行一条）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "family={} fs={:.1} taps={} passband=[{:.1},{:.1}] stopband=[{:.1},{:.1}] ripple_db={:.3} attenuation_db={:.1}",
            FAMILY,
            self.params.fs,
            self.params.taps,
            self.response.passband.lo_hz,
            self.response.passband.hi_hz,
            self.response.stopband.lo_hz,
            self.response.stopband.hi_hz,
            self.params.ripple_db,
            self.params.attenuation_db
        ));
        out.push(format!(
            "prototype: magnitude={} taps={} beta={:.4} factorization={} cepstrum_fft={} mag_floor_db={:.1}",
            self.params.prototype.magnitude.as_str(),
            self.params.prototype.taps,
            self.params.prototype.beta,
            self.params.factorization.as_str(),
            self.params.cepstrum_fft,
            self.params.mag_floor_db
        ));
        out.push(format!(
            "support: first={} last={} effective_taps={} peak_index={} max|h|={:.6}",
            self.support.first_effective,
            self.support.last_effective,
            self.support.effective_taps,
            self.support.peak_index,
            self.support.max_abs
        ));
        out.push(format!(
            "reference: hz={:.1} declared={:.4} dB measured={:.6} dB",
            self.response.reference.hz,
            self.response.reference.gain_db,
            self.response.reference.measured_gain_db
        ));
        out.push(format!(
            "passband: min={:.6} max={:.6} ripple={:.6} dB (limit {:.3})",
            self.response.passband.min_db,
            self.response.passband.max_db,
            self.response.passband.ripple_db,
            self.params.ripple_db + RIPPLE_TOL_DB
        ));
        out.push(format!(
            "stopband: max={:.3} dB attenuation={:.3} dB (limit {:.3})",
            self.response.stopband.max_db,
            self.response.stopband.attenuation_db,
            -(self.params.attenuation_db - ATTENUATION_TOL_DB)
        ));
        out.push(format!(
            "transition: edge@{} dB = {:?} Hz width={:?} Hz (f_s={:.1})",
            self.params.attenuation_db,
            self.response.transition.edge_hz_at_attenuation,
            self.response.transition.width_hz,
            self.response.stopband.lo_hz
        ));
        out.push(format!(
            "min_phase(M2): {} max_dev={:.6} deg limit={:.1} passed={} inverse_causality_diag={:.2} dB zero_radius_check={}",
            self.min_phase.method,
            self.min_phase.max_deviation_deg,
            self.min_phase.limit_deg,
            self.min_phase.passed,
            self.min_phase.inverse_causality_diagnostic_db,
            self.min_phase.zero_radius_check
        ));
        out.push(self.delay_statement());
        for (f, gd) in &self.delay.characteristic_hz_samples {
            out.push(format!("  GD({f:.0} Hz) = {gd:.6} samples"));
        }
        out.push(format!(
            "stability: class={} denom_deg={} poles@origin={} max|p|={:.1} stable={} iir_poles={} finite={}",
            self.stability.filter_class,
            self.stability.denominator_degree,
            self.stability.pole_count_at_origin,
            self.stability.max_pole_radius,
            self.stability.unconditionally_stable,
            self.stability.iir_poles_introduced,
            self.stability.all_finite
        ));
        out.push(format!(
            "endpoints: f20={} f20k={}",
            self.response.endpoints.f20.status_str(),
            self.response.endpoints.f20k.status_str()
        ));
        for c in &self.criteria.criteria {
            let measured = c
                .measured
                .map(|v| format!("{v:.6}"))
                .unwrap_or_else(|| "n/a".to_string());
            let limit = c
                .limit
                .map(|v| format!("{v:.6}"))
                .unwrap_or_else(|| "n/a".to_string());
            out.push(format!(
                "criterion {}: {} measured={} {} limit={} {}",
                c.id,
                if c.pass { "PASS" } else { "FAIL" },
                measured,
                c.unit,
                limit,
                if c.pass { "" } else { "  <-- INFEASIBLE" }
            ));
        }
        for w in &self.warnings {
            out.push(format!("warning: {w}"));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// 设计主流程（spec §3.2 参考构造 1–3 + 规范性判据 1–5）
// ---------------------------------------------------------------------------

fn measurement_grid(fs: f64, points: usize) -> Vec<f64> {
    let hi = ENDPOINT_20K_HZ.min(0.499 * fs);
    let lo = MIN_FREQ_HZ;
    if points < 2 || hi <= lo {
        return vec![lo];
    }
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

fn support_report(h: &[f64]) -> Result<SupportReport, MinFastError> {
    if h.is_empty() {
        return err(E_FIR_NUMERIC, "系数为空");
    }
    let mut max_abs = 0.0f64;
    let mut peak_index = 0usize;
    for (i, &v) in h.iter().enumerate() {
        if !v.is_finite() {
            return err(E_FIR_NUMERIC, format!("h[{i}]={v} 非有限数"));
        }
        let a = v.abs();
        if a > max_abs {
            max_abs = a;
            peak_index = i;
        }
    }
    if max_abs <= 0.0 {
        return err(E_FIR_NUMERIC, "max|h|=0");
    }
    let eps = EFFECTIVE_SUPPORT_EPS * max_abs;
    let first = h.iter().position(|v| v.abs() > eps).unwrap_or(0);
    let last = h.iter().rposition(|v| v.abs() > eps).unwrap_or(h.len() - 1);
    Ok(SupportReport {
        first_effective: first,
        last_effective: last,
        effective_taps: last - first + 1,
        max_abs,
        peak_index,
    })
}

fn ringing_report(h: &[f64], fs: f64, support: &SupportReport) -> RingingReport {
    let w = (RING_WINDOW_SEC * fs).ceil() as usize;
    let p = support.peak_index;
    let total_energy: f64 = h.iter().map(|v| v * v).sum();
    let pre = if p > w {
        let seg = &h[..p - w];
        let mx = seg.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let en: f64 = seg.iter().map(|v| v * v).sum();
        Some((mx, en))
    } else {
        None
    };
    let post = if p + w + 1 < h.len() {
        let seg = &h[p + w + 1..];
        let mx = seg.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        let en: f64 = seg.iter().map(|v| v * v).sum();
        Some((mx, en))
    } else {
        None
    };
    let to_db = |v: f64| 20.0 * (v / support.max_abs).max(1e-300).log10();
    let energy_db = |v: f64| 10.0 * (v / total_energy.max(1e-300)).max(1e-300).log10();
    RingingReport {
        exclusion_samples: w,
        pre_db: pre.map(|(mx, _)| to_db(mx)),
        post_db: post.map(|(mx, _)| to_db(mx)),
        pre_energy_db: pre.map(|(_, en)| energy_db(en)),
        post_energy_db: post.map(|(_, en)| energy_db(en)),
    }
}

fn endpoint_at(h: &[f64], fs: f64, freq_hz: f64) -> Endpoint {
    if freq_hz > MAX_FREQ_NYQUIST * fs {
        return Endpoint::NotApplicableAtFs {
            freq_hz,
            reason: "freq > 0.499*fs：低于当前采样率的可测上限",
        };
    }
    let (re, im) = frequency_response(h, fs, freq_hz);
    Endpoint::Measured {
        freq_hz,
        mag_db: db_from_power(re * re + im * im),
        phase_deg: im.atan2(re).to_degrees(),
    }
}

fn criterion(
    id: &'static str,
    statement: &'static str,
    measured: Option<f64>,
    limit: Option<f64>,
    unit: &'static str,
    pass: bool,
) -> Criterion {
    Criterion {
        id,
        statement,
        measured,
        limit,
        unit,
        pass,
    }
}

/// 设计 min_fast 系数并给出规范判据报告（spec §3.2）。
///
/// 返回 Ok(design) 表示参数合法且构造完成；是否满足全部规范判据看
/// MinFastDesign::accepted / failed_criteria（判据失败表示该参数组合 INFEASIBLE，
/// 不是静默放行）。需要“必须通过”的调用方用 design_strict。
pub fn design(params: &MinFastParams) -> Result<MinFastDesign, MinFastError> {
    validate_params(params)?;
    let mut warnings = Vec::new();

    // 1) 对称线性相位原型（spec §3.2 步骤 1–2）
    let prototype = symmetric_prototype(params)?;
    // 2) 同态最小相位谱分解（spec §3.2 步骤 3）
    let coefficients =
        min_phase_coefficients(&prototype, params.cepstrum_fft, params.mag_floor_db)?;
    if coefficients.len() != params.taps {
        return err(
            E_FIR_TAPS,
            format!("系数长度 {} != taps {}", coefficients.len(), params.taps),
        );
    }
    let support = support_report(&coefficients)?;

    // 3) 响应谱（65536 点；幅度/群延迟 / M2 / 逆因果诊断共用一次前向 FFT）
    let response_fft = Fft::new(RESPONSE_FFT)?;
    let mut re = vec![0.0f64; RESPONSE_FFT];
    let mut im = vec![0.0f64; RESPONSE_FFT];
    re[..coefficients.len()].copy_from_slice(&coefficients);
    response_fft.forward(&mut re, &mut im);
    let spectrum = ResponseSpectrum::new(&coefficients, params.fs, RESPONSE_FFT)?;

    // 4) 最小相位验证 M2（spec §3.2 判据 2，N=65536）
    let m2_deg = min_phase_consistency_from_spectrum(&re, &im, params.fs, &response_fft)?;
    let inverse_causality_db =
        inverse_causality_diagnostic_db(&coefficients, RESPONSE_FFT, -120.0).unwrap_or(f64::NAN);
    let m2_passed = m2_deg <= MIN_PHASE_MAX_DEG;
    if !m2_passed {
        return err(
            E_FIR_MINPHASE,
            format!(
                "M2 相位–Hilbert 一致性 {m2_deg:.6}° > {MIN_PHASE_MAX_DEG}°（spec §3.2 判据 2）：构造未得到最小相位或数值异常"
            ),
        );
    }
    if m2_deg > 0.5 {
        warnings.push(format!(
            "M2 一致性 {m2_deg:.4}° 接近/高于工程典型值 0.5°（限 {MIN_PHASE_MAX_DEG}°），建议增大 cepstrum_fft 或 prototype.taps"
        ));
    }

    // 5) 测量网格与通带/阻带/过渡带（spec §1.2/§3.1 判据 3–5）
    let grid_points = (8 * params.taps).max(8_192);
    let grid = measurement_grid(params.fs, grid_points);
    let [pb_lo, pb_hi] = params.passband_hz;
    let [sb_lo, sb_hi_raw] = params.stopband_hz;
    let sb_hi = sb_hi_raw.min(MAX_FREQ_NYQUIST * params.fs);
    let mut pb_min = f64::INFINITY;
    let mut pb_max = f64::NEG_INFINITY;
    for &f in grid.iter().filter(|f| **f >= pb_lo && **f <= pb_hi) {
        let v = spectrum.mag_db_at(f);
        pb_min = pb_min.min(v);
        pb_max = pb_max.max(v);
    }
    for f in [pb_lo, pb_hi] {
        let v = spectrum.mag_db_at(f);
        pb_min = pb_min.min(v);
        pb_max = pb_max.max(v);
    }
    if !pb_min.is_finite() || !pb_max.is_finite() {
        return err(E_FIR_BAND, "通带内没有可测量的网格点，检查 passband_hz");
    }
    let passband_ripple = pb_max - pb_min;
    // 阻带：网格插值 + bin 采样取较大者（保守、不乐观）
    let mut sb_max = f64::NEG_INFINITY;
    for &f in grid.iter().filter(|f| **f >= sb_lo && **f <= sb_hi) {
        sb_max = sb_max.max(spectrum.mag_db_at(f));
    }
    for f in [sb_lo, sb_hi] {
        sb_max = sb_max.max(spectrum.mag_db_at(f));
    }
    sb_max = sb_max.max(spectrum.max_mag_db_bins(sb_lo, sb_hi));
    let attenuation_measured = -sb_max;
    // 过渡带：首次达到 −A_s 的频点（先谱扫描取括号，再直接 DFT 二分到高精度）
    let cross_lo = pb_hi;
    let cross_hi = MAX_FREQ_NYQUIST * params.fs;
    let scan: Vec<f64> = (0..=512)
        .map(|i| cross_lo + (cross_hi - cross_lo) * i as f64 / 512.0)
        .collect();
    let mut crossing: Option<f64> = None;
    let mut prev_f = cross_lo;
    let mut prev_v = magnitude_db(&coefficients, params.fs, cross_lo);
    for &f in scan.iter().skip(1) {
        let v = spectrum.mag_db_at(f);
        if v <= -params.attenuation_db && prev_v > -params.attenuation_db {
            let mut a = prev_f;
            let mut b = f;
            for _ in 0..60 {
                let m = 0.5 * (a + b);
                if magnitude_db(&coefficients, params.fs, m) > -params.attenuation_db {
                    a = m;
                } else {
                    b = m;
                }
            }
            crossing = Some(0.5 * (a + b));
            break;
        }
        prev_f = f;
        prev_v = spectrum.mag_db_at(f);
    }
    let transition = TransitionReport {
        target_attenuation_db: params.attenuation_db,
        edge_hz_at_attenuation: crossing,
        width_hz: crossing.map(|c| c - pb_hi),
    };

    // 6) 端点（spec §2.4）
    let endpoints = EndpointsReport {
        f20: endpoint_at(&coefficients, params.fs, MIN_FREQ_HZ),
        f20k: endpoint_at(&coefficients, params.fs, ENDPOINT_20K_HZ),
    };

    // 7) 振铃（spec §1.4）
    let ringing = ringing_report(&coefficients, params.fs, &support);

    // 8) 参考归一化（spec §1.5：DC 声明 0 dB；无隐藏增益）
    let dc_sum: f64 = coefficients.iter().sum();
    let measured_gain_db = db_from_power(dc_sum * dc_sum);
    let reference = ReferenceReport {
        hz: REFERENCE_HZ_DC,
        gain_db: 0.0,
        measured_gain_db,
    };

    // 9) 群延迟报告（spec §3.2 判据 3；频变 GD + onset，不冒称零延迟）
    let mut gd_pb_min = f64::INFINITY;
    let mut gd_pb_max = f64::NEG_INFINITY;
    let mut gd_pb_sum = 0.0f64;
    let mut gd_pb_n = 0usize;
    for &f in grid.iter().filter(|f| **f >= pb_lo && **f <= pb_hi) {
        let g = spectrum.gd_at(f);
        gd_pb_min = gd_pb_min.min(g);
        gd_pb_max = gd_pb_max.max(g);
        gd_pb_sum += g;
        gd_pb_n += 1;
    }
    if gd_pb_n == 0 {
        return err(E_FIR_DELAY, "通带内没有群延迟测量点");
    }
    let band_max_samples = grid
        .iter()
        .fold(f64::NEG_INFINITY, |m, &f| m.max(spectrum.gd_at(f)));
    let at_1khz = if 1_000.0 <= 0.499 * params.fs {
        spectrum.gd_at(1_000.0)
    } else {
        f64::NAN
    };
    let mut characteristic = vec![
        (MIN_FREQ_HZ, spectrum.gd_at(MIN_FREQ_HZ)),
        (50.0, spectrum.gd_at(50.0)),
        (100.0, spectrum.gd_at(100.0)),
        (1_000.0, at_1khz),
        (5_000.0, spectrum.gd_at(5_000.0)),
        (10_000.0, spectrum.gd_at(10_000.0)),
        (15_000.0, spectrum.gd_at(15_000.0)),
        (pb_hi, spectrum.gd_at(pb_hi)),
        (0.9 * sb_lo, spectrum.gd_at(0.9 * sb_lo)),
        (sb_lo, spectrum.gd_at(sb_lo)),
    ];
    characteristic.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let delay = DelayReport {
        class: "frequency_varying",
        declared_bulk_samples: 0.0,
        declared_bulk_ms: 0.0,
        passband_min_samples: gd_pb_min,
        passband_max_samples: gd_pb_max,
        passband_mean_samples: gd_pb_sum / gd_pb_n as f64,
        at_1khz_samples: at_1khz,
        latency_ms_1khz: at_1khz / params.fs * 1000.0,
        band_max_samples,
        gd_max_ms: band_max_samples / params.fs * 1000.0,
        onset_samples: support.first_effective,
        onset_ms: support.first_effective as f64 / params.fs * 1000.0,
        characteristic_hz_samples: characteristic,
        zero_delay_claim: false,
    };

    // 10) 极点/稳定性边界（spec §3.2 判据 5）：FIR 分母为 1，极点全在 z=0
    let stability = StabilityReport {
        filter_class: "fir",
        denominator_degree: 0,
        pole_count_at_origin: coefficients.len(),
        max_pole_radius: 0.0,
        pole_stability_boundary: "FIR: transfer denominator = 1, all poles at z=0 (|p|=0 < 1), unconditionally stable; no IIR poles introduced",
        unconditionally_stable: true,
        iir_poles_introduced: false,
        all_finite: coefficients.iter().all(|v| v.is_finite()),
        max_abs_coefficient: support.max_abs,
        dc_gain_linear: dc_sum,
        numeric_violations: coefficients.iter().filter(|v| !v.is_finite()).count(),
    };

    // 11) 规范判据（spec §3.2 判据 1–5 + §3.1 判据 3–6 + §1.3/§1.4/§2.4）
    let mut criteria = Vec::new();
    criteria.push(criterion(
        "finite_support",
        "有限支撑：len(h)=taps、全为有限值、max|h|>0（spec §1.3/§2.2 规则 1/3）",
        Some(support.effective_taps as f64),
        Some(params.taps as f64),
        "taps",
        coefficients.len() == params.taps
            && stability.all_finite
            && support.max_abs > 0.0
            && support.last_effective < params.taps,
    ));
    criteria.push(criterion(
        "passband_ripple",
        "通带 p-p 纹波 <= ripple_db+0.05（spec §3.1 判据 3 / §3.2 判据 1）",
        Some(passband_ripple),
        Some(params.ripple_db + RIPPLE_TOL_DB),
        "dB",
        passband_ripple <= params.ripple_db + RIPPLE_TOL_DB,
    ));
    criteria.push(criterion(
        "stopband_attenuation",
        "阻带 max|H| <= −(A_s−0.5)（spec §3.1 判据 4 / §3.2 判据 1）",
        Some(sb_max),
        Some(-(params.attenuation_db - ATTENUATION_TOL_DB)),
        "dB",
        sb_max <= -(params.attenuation_db - ATTENUATION_TOL_DB),
    ));
    criteria.push(criterion(
        "transition_edge",
        "首次达到 −A_s 的频点 <= f_s（spec §3.1 判据 5 / §3.2 判据 1）",
        crossing,
        Some(sb_lo),
        "Hz",
        crossing.is_some_and(|c| c <= sb_lo),
    ));
    criteria.push(criterion(
        "reference_gain",
        "|H(DC)|dB 与声明 gain_db 差 <= 0.01 dB（spec §1.5/§3.1 判据 6）",
        Some((measured_gain_db - reference.gain_db).abs()),
        Some(REFERENCE_TOL_DB),
        "dB",
        (measured_gain_db - reference.gain_db).abs() <= REFERENCE_TOL_DB,
    ));
    criteria.push(criterion(
        "min_phase_m2",
        "M2 相位–Hilbert 一致性 <= 2.0°（spec §3.2 判据 2）",
        Some(m2_deg),
        Some(MIN_PHASE_MAX_DEG),
        "deg",
        m2_passed,
    ));
    criteria.push(criterion(
        "delay_report",
        "频变 GD/onset/gd_max 已报告且 zero_delay_claim=false（spec §3.2 判据 3：禁止声称零延迟）",
        Some(at_1khz),
        None,
        "samples",
        at_1khz.is_finite()
            && delay.class == "frequency_varying"
            && !delay.zero_delay_claim
            && delay.onset_samples < params.taps,
    ));
    criteria.push(criterion(
        "endpoints",
        "20 Hz 与 20 kHz 端点均单列（measured 或 not_applicable_at_fs；spec §2.4）",
        None,
        None,
        "endpoint",
        endpoints.is_complete(),
    ));
    criteria.push(criterion(
        "stability_boundary",
        "FIR 极点在 z=0、无条件稳定、不引入 IIR 极点、无 NaN/Inf（spec §3.2 判据 5）",
        Some(support.max_abs),
        None,
        "max|h|",
        stability.unconditionally_stable
            && !stability.iir_poles_introduced
            && stability.all_finite
            && stability.numeric_violations == 0,
    ));
    criteria.push(criterion(
        "family_boundary",
        "f_s/f_p <= 1.5 且 f_s <= 0.45fs（spec §3.2 判据 4）",
        Some(sb_lo / pb_hi),
        Some(FAMILY_EDGE_RATIO),
        "ratio",
        sb_lo / pb_hi <= FAMILY_EDGE_RATIO && sb_lo <= FAMILY_EDGE_NYQUIST * params.fs,
    ));

    // 12) 警告（工程提示；不影响判据）
    if params.cepstrum_fft < 8 * params.taps {
        warnings.push(format!(
            "cepstrum_fft={} < 8*taps={}：倒谱分辨率偏低，建议增大（spec 参考构造 32768 对 2048 taps）",
            params.cepstrum_fft,
            8 * params.taps
        ));
    }
    if params.prototype.taps < 8 * params.taps {
        warnings.push(format!(
            "prototype.taps={} < 8*taps={}：原型合成网格偏小，别名风险（spec 参考构造 32768 对 2048 taps）",
            params.prototype.taps,
            8 * params.taps
        ));
    }
    if params.prototype.magnitude == PrototypeMagnitude::Linear {
        warnings.push(
            "prototype.magnitude=linear：线性幅度过渡的阻带/过渡实测通常差于升余弦（spec §3.2 参考构造为 raised_cosine）；判据以实测为准，不静默放宽".to_string(),
        );
    }
    if params.attenuation_db > 96.0 {
        warnings.push(format!(
            "attenuation_db={:.1} > 96 dB：该 taps/过渡带组合可能不可达，若判据失败应增大 taps、放宽过渡带或降低 attenuation",
            params.attenuation_db
        ));
    }
    let stopband_margin = -(params.attenuation_db - ATTENUATION_TOL_DB) - sb_max;
    if stopband_margin < 1.0 {
        warnings.push(format!(
            "阻带余量仅 {stopband_margin:.3} dB（<1 dB）：建议增大 taps 或放宽过渡带"
        ));
    }
    let ripple_margin = params.ripple_db + RIPPLE_TOL_DB - passband_ripple;
    if ripple_margin < 0.01 {
        warnings.push(format!(
            "通带纹波余量仅 {ripple_margin:.6} dB（<0.01 dB）：建议增大 taps"
        ));
    }
    if params.fs != DEFAULT_FS && params.fs != 96_000.0 {
        warnings.push(format!(
            "fs={} 不在 manifest 允许的 48000/96000（spec §2.2）：本设计仅工具层有效",
            params.fs
        ));
    }

    Ok(MinFastDesign {
        params: params.clone(),
        coefficients,
        prototype,
        support,
        response: ResponseReport {
            grid_points,
            passband: PassbandReport {
                lo_hz: pb_lo,
                hi_hz: pb_hi,
                min_db: pb_min,
                max_db: pb_max,
                ripple_db: passband_ripple,
            },
            stopband: StopbandReport {
                lo_hz: sb_lo,
                hi_hz: sb_hi,
                max_db: sb_max,
                attenuation_db: attenuation_measured,
            },
            transition,
            endpoints,
            ringing,
            reference,
        },
        delay,
        min_phase: MinPhaseReport {
            method: "phase_hilbert_m2",
            fft_size: RESPONSE_FFT,
            band_hz: [MIN_PHASE_BAND_LO_HZ, MIN_PHASE_BAND_HI_RATIO * params.fs],
            max_deviation_deg: m2_deg,
            limit_deg: MIN_PHASE_MAX_DEG,
            passed: m2_passed,
            inverse_causality_diagnostic_db: inverse_causality_db,
            zero_radius_check: "not_run（spec §3.2 判据 2 的 MAY 根检查未实现；M2 为规范性门）",
        },
        stability,
        criteria: NormativeReport { criteria },
        warnings,
    })
}

/// 严格设计：构造完成且全部规范判据通过才返回 Ok；否则 E_FIR_BUDGET（INFEASIBLE）。
pub fn design_strict(params: &MinFastParams) -> Result<MinFastDesign, MinFastError> {
    let d = design(params)?;
    if d.accepted() {
        Ok(d)
    } else {
        err(
            E_FIR_BUDGET,
            format!(
                "INFEASIBLE（不是静默放行）：未通过判据 {}",
                d.failed_criteria().join(",")
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T33 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod min_fast_tests {
    use super::*;

    include!("../tests/T33/min_fast_cases.rs");
}
