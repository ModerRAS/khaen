//! min_slow 最小相位慢滚降族（T34）——升余弦「f_p → Nyquist」平滑衰减 + 同态最小相位谱分解。
//!
//! 权威契约（只读输入）：
//! - rt-stack/spec/fir-toolbox.md §3.3（v1.2-draft）：参数、参考构造、规范性判据 1–6；
//! - 同文件 §1.2/§1.3/§1.4/§1.5/§1.6、§2.2/§2.4、§3.8（家族互斥）与 §9 V17（可复现参考构造）；
//! - 同文件 §3.2 判据 2/3（min_* 共用的最小相位 M2 与延迟报告口径）；
//! - rt-stack/evidence/T04/recompute-vectors.mjs V17 与 vector-results.json（可复现参考值）；
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T34 行与 §1/§7（写域/证据纪律）。
//!
//! 与 min_fast（T33）的家族区别（spec §3.3 判据 6 / §3.8）：
//! 1. min_slow **没有独立阻带边 f_s**：幅度在 [20, f_p] 内为 1，从 f_p 到 fs/2 用升余弦平滑
//!    衰减到 floor（默认 −120 dB），即 Nyquist 附近「平缓滚降」，不是 fast 的近 Nyquist 锐截止；
//! 2. 独立参数是 `nyquist_attenuation_db`（20..60 dB，Nyquist 带 [0.49fs, 0.499fs] 的声明衰减），
//!    不是 fast 的 `stopband_hz` / `attenuation_db`；
//! 3. 家族边界为 f_p <= 0.42*fs（fast 用 f_s/f_p <= 1.5 且 f_s <= 0.45fs）；
//! 4. 互斥判据：测得 M(0.45fs) 必须 > −60 dB（否则应改用 min_fast，E_FIR_FAMILY）；报告必须
//!    给出 −60 dB 穿越点与 M(0.45fs)（本文件 FamilyDistinctionReport）。
//!
//! **min_slow 不是零延迟**：FIR 无反馈（传递函数分母为 1，极点全部在 z=0，无条件稳定），
//! 但群延迟频变（低频约 0.23 samples，Nyquist 附近上升），必须报 GD min/max/mean/@1 kHz、
//! onset、gd_max，禁止声称「零延迟/零群延迟」（spec §3.3 判据 5 / §3.2 判据 3 / §1.3）。
//!
//! T34 leaf 范围：本文件只实现 min_slow 一族的设计/复算/规范判据报告；WAV+manifest 落盘、
//! 20/20k 端点报告的最终编排归 T55；资源准入/融合归 T12/T56。私有测试在
//! crates/designer/tests/T34/min_slow_cases.rs，通过文末 include! 在 #[cfg(test)] 下编译执行
//! （不改 integrator 独占的 public_registration.rs，见 tests/T34/README.md）。
//!
//! 状态：T34 实现 + 真实 cargo test -p tspi-fir-designer min_slow 执行（见 evidence/T34/）。
//! 未做：板端/ALSA/RT 实测、WAV 落盘导出、roots 分解、零点半径检查（spec MAY）、T55 报告（NOT_RUN）。

use std::fmt;

/// 家族标识（manifest family，spec §2.2/§1.7）。
pub const FAMILY: &str = "min_slow";
/// 家族中文名（spec §1.7）。
pub const FAMILY_CN: &str = "最小相位慢滚降";
/// 系数产物 schema（本 leaf 不落盘，仅报告该常量供 T55 使用；spec §2.2）。
pub const FIR_SCHEMA: &str = "tspi-fir/v1";

/// 契约默认采样率（spec §2.2：manifest fs 只允许 48000/96000）。
pub const DEFAULT_FS: f64 = 48_000.0;
/// 参考构造的 Kaiser β（spec §3.3 参考构造；V17）。
pub const DEFAULT_KAISER_BETA: f64 = 8.96;
/// 参考构造的幅度 floor（−120 dB，spec §3.3）。
pub const DEFAULT_MAG_FLOOR_DB: f64 = -120.0;
/// 参考构造的原型合成网格 N（spec §3.3：32768）。
pub const DEFAULT_PROTOTYPE_TAPS: usize = 32_768;
/// 参考构造的倒谱 FFT 尺寸（spec §3.3：32768）。
pub const DEFAULT_CEPSTRUM_FFT: usize = 32_768;
/// V17 参考构造的支撑长度 L（spec §3.3 判据 7：L=1024）。
pub const DEFAULT_TAPS: usize = 1_024;
/// 默认通带上界 f_p（spec §3.3 判据 7：f_p=16 kHz；且 f_p <= 0.42*fs）。
pub const DEFAULT_PASSBAND_HI_HZ: f64 = 16_000.0;
/// Nyquist 带声明衰减默认值（spec §3.3 参数：20..60 dB，默认 30）。
pub const DEFAULT_NYQUIST_ATTENUATION_DB: f64 = 30.0;
/// Nyquist 带声明衰减下限（spec §3.3 参数：20 dB）。
pub const MIN_NYQUIST_ATTENUATION_DB: f64 = 20.0;
/// Nyquist 带声明衰减上限（spec §3.3 参数：60 dB）。
pub const MAX_NYQUIST_ATTENUATION_DB: f64 = 60.0;
/// Nyquist 带判据保留余量（spec §3.3 判据 2：<= −(declared−1) dB）。
pub const NYQUIST_ATTENUATION_TOL_DB: f64 = 1.0;

/// 慢滚降家族边界：f_p <= 0.42*fs（spec §3.3 参考构造/判据 6）。
pub const FAMILY_EDGE_FP_RATIO: f64 = 0.42;
/// fast/slow 互斥边界系数：0.45*fs（spec §3.3 判据 6 / §3.8）。
pub const FAST_BOUNDARY_RATIO: f64 = 0.45;
/// fast/slow 互斥判据的 −60 dB 门（spec §3.3 判据 6）。
pub const FAST_BOUNDARY_ATTENUATION_DB: f64 = -60.0;
/// Nyquist 带下界系数（spec §3.3 判据 2：[0.49fs, 0.499fs]）。
pub const NYQUIST_BAND_LO_RATIO: f64 = 0.49;
/// 慢滚降单调性允许的最大上翘（spec §3.3 判据 3：<= 0.25 dB）。
pub const MONOTONIC_MAX_RISE_DB: f64 = 0.25;

/// 支撑下限（spec §3.1 判据 8：L ∈ [8,16384]）。
pub const MIN_TAPS: usize = 8;
/// 常规/扩展准入上限（spec §7.2：>16384 仅 offline 设计工具产物，runtime 不放行）。
pub const MAX_TAPS: usize = 16_384;
/// 测量网格/通带下界（spec §1.2：20 Hz）。
pub const MIN_FREQ_HZ: f64 = 20.0;
/// 频率上限系数（spec §1.2/§3.1：<= 0.499*fs）。
pub const MAX_FREQ_NYQUIST: f64 = 0.499;
/// 通带纹波参数上限（本 leaf 实现界限：>6 dB 不再是「可用的目标型」设计）。
pub const MAX_RIPPLE_DB: f64 = 6.0;
/// 幅度 floor 下限（本 leaf 实现界限；比 spec 默认 −120 dB 更深的 floor 无实际意义）。
pub const MIN_MAG_FLOOR_DB: f64 = -240.0;
/// 幅度 floor 上限（spec 默认 −120；−60 以上会把滚降要求压得过松，拒绝）。
pub const MAX_MAG_FLOOR_DB: f64 = -60.0;
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
/// 20 kHz 端点（spec §2.4）。
pub const ENDPOINT_20K_HZ: f64 = 20_000.0;
/// 参考点：DC（spec §1.5：hz=0 表示 DC）。
pub const REFERENCE_HZ_DC: f64 = 0.0;

// ---------------------------------------------------------------------------
// 错误码（spec §2.5 E_FIR_* 集中表；本 leaf 只产生与设计阶段相关的子集）
// ---------------------------------------------------------------------------

/// 参数/manifest 结构非法（网格尺寸非 2 的幂、原型 family/transition 非法等；spec §2.5）。
pub const E_FIR_SCHEMA: &str = "E_FIR_SCHEMA";
/// 频带参数非法（顺序/越界/过渡参数；spec §2.5）。
pub const E_FIR_BAND: &str = "E_FIR_BAND";
/// 家族自洽失败（f_p>0.42fs、未实现分解方法、slow/fast 互斥判据失败；spec §2.5）。
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
pub struct MinSlowError {
    pub code: &'static str,
    pub message: String,
}

impl MinSlowError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for MinSlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for MinSlowError {}

fn err<T>(code: &'static str, message: impl Into<String>) -> Result<T, MinSlowError> {
    Err(MinSlowError::new(code, message))
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

/// Kaiser 窗（长度 len，β；与 spec §3.3/§9 V17 参考构造一致）。
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
    fn new(n: usize) -> Result<Self, MinSlowError> {
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
// 直接响应复算（O(L)，相位递推避免逐点三角函数；用于端点/特征频点/穿越二分）
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

/// 最小相位谱分解方法（spec §3.3 参数 factorization）。
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

/// 原型构造说明（spec §3.3 参数 prototype: {family:"smooth", transition:"cosine_to_nyquist"}）。
///
/// 用字符串字段是为了把规范里的非法取值显式拒收（E_FIR_FAMILY），而不是靠类型静默丢失：
/// family 必须为 "smooth"、transition 必须为 "cosine_to_nyquist"；fast 的 raised_cosine→f_s
/// 构造不属于本家族（fast/slow 家族互斥，spec §3.8）。
#[derive(Debug, Clone, PartialEq)]
pub struct SlowPrototypeSpec {
    /// 必须为 "smooth"（spec §3.3 参数）。
    pub family: &'static str,
    /// 必须为 "cosine_to_nyquist"（spec §3.3 参数）。
    pub transition: &'static str,
    /// 原型合成网格 N（2 的幂；spec 参考构造 32768）。
    pub taps: usize,
    /// Kaiser β（窗固定 kaiser；spec 参考构造 8.96）。
    pub beta: f64,
}

impl Default for SlowPrototypeSpec {
    fn default() -> Self {
        Self {
            family: "smooth",
            transition: "cosine_to_nyquist",
            taps: DEFAULT_PROTOTYPE_TAPS,
            beta: DEFAULT_KAISER_BETA,
        }
    }
}

/// min_slow 设计参数（spec §3.3；与 min_fast 的独立参数：没有 stopband_hz/attenuation_db，
/// 而是 f_p + nyquist_attenuation_db）。
#[derive(Debug, Clone, PartialEq)]
pub struct MinSlowParams {
    /// 运行采样率 Hz（manifest 只允许 48000/96000；本 leaf 接受 8000..=192000 并警告）。
    pub fs: f64,
    /// 系数支撑长度 L（spec §3.1 判据 8：8..=16384）。
    pub taps: usize,
    /// 通带 [lo, f_p]（Hz；lo >= 20，f_p > 20，f_p <= 0.42*fs）。
    pub passband_hz: [f64; 2],
    /// Nyquist 带 [0.49fs, 0.499fs] 的声明衰减（dB；spec 参数 20..60，默认 30）。
    pub nyquist_attenuation_db: f64,
    /// 通带纹波 p-p 声明（dB，spec §3.3 默认 0.5）。
    pub ripple_db: f64,
    /// 原型合成参数（family=smooth、transition=cosine_to_nyquist）。
    pub prototype: SlowPrototypeSpec,
    /// 最小相位分解方法（本 leaf 仅 cepstrum）。
    pub factorization: Factorization,
    /// 幅度 floor（dB，默认 −120；滚降目标在 fs/2 处到达 floor）。
    pub mag_floor_db: f64,
    /// 倒谱 FFT 尺寸（2 的幂，默认 32768）。
    pub cepstrum_fft: usize,
}

impl Default for MinSlowParams {
    fn default() -> Self {
        Self {
            fs: DEFAULT_FS,
            taps: DEFAULT_TAPS,
            passband_hz: [MIN_FREQ_HZ, DEFAULT_PASSBAND_HI_HZ],
            nyquist_attenuation_db: DEFAULT_NYQUIST_ATTENUATION_DB,
            ripple_db: 0.5,
            prototype: SlowPrototypeSpec::default(),
            factorization: Factorization::Cepstrum,
            mag_floor_db: DEFAULT_MAG_FLOOR_DB,
            cepstrum_fft: DEFAULT_CEPSTRUM_FFT,
        }
    }
}

impl MinSlowParams {
    /// V17 参考构造（spec §9 V17 / evidence/T04）：L=1024、f_p=16k、升余弦到 Nyquist、
    /// floor −120 dB、β=8.96、N=32768（默认值即该参考构造；另声明 nyquist_attenuation_db=30）。
    pub fn v17_reference() -> Self {
        Self::default()
    }

    /// 参数校验（spec §3.3 参数 + 判据 6 + §3.1 判据 8 + §7.2 支撑边界）。
    pub fn validate(&self) -> Result<(), MinSlowError> {
        validate_params(self)
    }
}

/// 参数校验（公开函数；design() 会先调用）。
pub fn validate_params(p: &MinSlowParams) -> Result<(), MinSlowError> {
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
                "taps={} 非法：min_slow 支撑下限 {MIN_TAPS}（spec §3.1 判据 8）",
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
    for (name, v) in [("passband.lo", lo), ("passband.f_p", fp)] {
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
    let fp_limit = FAMILY_EDGE_FP_RATIO * p.fs;
    if fp > fp_limit {
        return err(
            E_FIR_FAMILY,
            format!(
                "f_p={fp} > 0.42*fs={fp_limit:.3}：不属于 min_slow 家族边界（spec §3.3 判据 6/§3.8），应改 min_fast/linear"
            ),
        );
    }
    if !p.nyquist_attenuation_db.is_finite()
        || p.nyquist_attenuation_db < MIN_NYQUIST_ATTENUATION_DB
        || p.nyquist_attenuation_db > MAX_NYQUIST_ATTENUATION_DB
    {
        return err(
            E_FIR_BAND,
            format!(
                "nyquist_attenuation_db={} 非法：需 [{MIN_NYQUIST_ATTENUATION_DB}, {MAX_NYQUIST_ATTENUATION_DB}] dB（spec §3.3 参数，默认 30）",
                p.nyquist_attenuation_db
            ),
        );
    }
    if !p.ripple_db.is_finite() || p.ripple_db <= 0.0 || p.ripple_db > MAX_RIPPLE_DB {
        return err(
            E_FIR_BAND,
            format!(
                "ripple_db={} 非法：需 (0, {MAX_RIPPLE_DB}] dB（spec §3.3 默认 0.5）",
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
                "mag_floor_db={} 非法：需 [{MIN_MAG_FLOOR_DB}, {MAX_MAG_FLOOR_DB}] dB（spec §3.3 默认 −120）",
                p.mag_floor_db
            ),
        );
    }
    if p.prototype.family != "smooth" {
        return err(
            E_FIR_FAMILY,
            format!(
                "prototype.family={:?} 非法：min_slow 只允许 \"smooth\"（spec §3.3 参数；fast 的 raised_cosine→f_s 属 min_fast）",
                p.prototype.family
            ),
        );
    }
    if p.prototype.transition != "cosine_to_nyquist" {
        return err(
            E_FIR_FAMILY,
            format!(
                "prototype.transition={:?} 非法：min_slow 只允许 \"cosine_to_nyquist\"（spec §3.3 参数）",
                p.prototype.transition
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
                "prototype.taps={} 非法：需 2 的幂且 [{MIN_GRID_SIZE}, {MAX_GRID_SIZE}]（spec §3.3 N=32768）",
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
                "prototype.beta={} 非法：需 [0, 20]（spec §3.3 参考 beta=8.96）",
                p.prototype.beta
            ),
        );
    }
    if p.factorization != Factorization::Cepstrum {
        return err(
            E_FIR_FAMILY,
            format!(
                "factorization={}：本 leaf 只实现 cepstrum（spec §3.3 参考构造；roots 为 MAY 的等价方法，未实现即拒绝）",
                p.factorization.as_str()
            ),
        );
    }
    if !is_pow2(p.cepstrum_fft) || p.cepstrum_fft < MIN_GRID_SIZE || p.cepstrum_fft > MAX_GRID_SIZE
    {
        return err(
            E_FIR_SCHEMA,
            format!(
                "cepstrum_fft={} 非法：需 2 的幂且 [{MIN_GRID_SIZE}, {MAX_GRID_SIZE}]（spec §3.3 N=32768）",
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
// 幅度目标与原型合成（spec §3.3 参考构造步骤 1–2）
// ---------------------------------------------------------------------------

/// 目标幅度（线性幅度，非 dB）：
/// [20,f_p] 为 1；[f_p, fs/2] 用升余弦从 1 平滑衰减到 floor=10^(mag_floor_db/20)；>=fs/2 取 floor。
///
/// 这是 min_slow 与 min_fast 的核心区别（spec §3.3）：没有独立阻带边 f_s，滚降一直延伸到
/// Nyquist 才到达 floor（slow rolloff），而不是在 f_s 处陡降。
pub fn prototype_target_magnitude(p: &MinSlowParams, f_hz: f64) -> f64 {
    let fp = p.passband_hz[1];
    let nyq = 0.5 * p.fs;
    let floor = 10f64.powf(p.mag_floor_db / 20.0);
    if f_hz <= fp {
        1.0
    } else if f_hz >= nyq {
        floor
    } else {
        let t = (f_hz - fp) / (nyq - fp);
        (floor + (1.0 - floor) * 0.5 * (1.0 + (std::f64::consts::PI * t).cos())).clamp(floor, 1.0)
    }
}

/// 对称线性相位原型合成（spec §3.3 参考构造：N 网格、D=(L−1)/2、Kaiser β、DC 归一）。
pub fn symmetric_prototype(p: &MinSlowParams) -> Result<Vec<f64>, MinSlowError> {
    validate_params(p)?;
    symmetric_prototype_with(p, |f| prototype_target_magnitude(p, f))
}

/// 用任意幅度函数做对称线性相位原型合成（等价 FFT 实现；公开供独立复算/家族对照使用）。
///
/// mag_at(f) 返回线性幅度（非 dB）；合成窗口长度 = params.taps、Kaiser β=params.prototype.beta，
/// 结果做 DC 归一（sum(h_proto)=1，spec §3.3 参考构造步骤 2）。
pub fn symmetric_prototype_with<F>(p: &MinSlowParams, mag_at: F) -> Result<Vec<f64>, MinSlowError>
where
    F: Fn(f64) -> f64,
{
    if p.taps < MIN_TAPS {
        return err(E_FIR_TAPS, format!("taps={} < {MIN_TAPS}", p.taps));
    }
    let n = p.prototype.taps;
    let l = p.taps;
    let d = (l as f64 - 1.0) / 2.0;
    let fft = Fft::new(n)?;
    let half = n / 2;
    let mut re = vec![0.0f64; n];
    let mut im = vec![0.0f64; n];
    for k in 0..=half {
        let f = k as f64 * p.fs / n as f64;
        let m = mag_at(f);
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
// 同态最小相位谱分解（spec §3.3 参考构造步骤 3）与最小相位验证 M2（判据 4）
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

/// 由线性相位原型做同态最小相位谱分解（spec §3.3 参考构造步骤 3），返回前 proto.len() 点。
pub fn min_phase_coefficients(
    prototype: &[f64],
    cepstrum_fft: usize,
    mag_floor_db: f64,
) -> Result<Vec<f64>, MinSlowError> {
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
    // exp(复杂度谱) → 最小相位谱 → IFFT，取前 L 点；不做峰值归一化（spec §3.3 步骤 3）
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
) -> Result<f64, MinSlowError> {
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
pub fn min_phase_consistency_deg(h: &[f64], fs: f64, n_fft: usize) -> Result<f64, MinSlowError> {
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
) -> Result<f64, MinSlowError> {
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
// 家族互斥指标（spec §3.3 判据 6 / §3.8：slow vs fast 的数值边界）
// ---------------------------------------------------------------------------

/// slow/fast 家族边界指标：M(0.45fs)、−60 dB 穿越点、0.45fs 门。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FamilyBoundaryMetrics {
    /// 0.45*fs（spec §3.3 判据 6 的边界频点）。
    pub fast_boundary_hz: f64,
    /// 测得 M(0.45fs)（dB）。
    pub mag_db_at_045fs: f64,
    /// 从通带侧首次跌到 −60 dB 的频点（Hz）；若在 0.499fs 之前未穿越则为 None。
    pub crossing_minus60db_hz: Option<f64>,
    /// 是否满足 min_slow 家族互斥判据（M(0.45fs) > −60 dB）。
    pub slow_like: bool,
}

impl FamilyBoundaryMetrics {
    /// 互斥判据的完整判定：M(0.45fs) > −60 dB，且 −60 dB 穿越点（如有）> 0.45fs。
    pub fn passes_slow_gate(&self) -> bool {
        self.slow_like
            && self
                .crossing_minus60db_hz
                .map_or(true, |f| f > self.fast_boundary_hz)
    }
}

/// 计算 slow/fast 互斥边界指标（spec §3.3 判据 6 / §3.8）。
///
/// −60 dB 穿越点用 1024 点对数粗扫 + 直接 DFT 二分（60 次）定位；穿越点位于 0.45fs 之上
/// （或整个测量带内未穿越）才能继续称为 min_slow，否则应改用 min_fast（E_FIR_FAMILY）。
pub fn family_boundary_metrics(h: &[f64], fs: f64) -> Result<FamilyBoundaryMetrics, MinSlowError> {
    if h.is_empty() {
        return err(E_FIR_NUMERIC, "h 为空");
    }
    if !fs.is_finite() || fs < 8_000.0 {
        return err(E_FIR_BAND, format!("fs={fs} 非法：需有限且 >= 8000 Hz"));
    }
    for (i, v) in h.iter().enumerate() {
        if !v.is_finite() {
            return err(E_FIR_NUMERIC, format!("h[{i}]={v} 非有限数"));
        }
    }
    let f045 = FAST_BOUNDARY_RATIO * fs;
    let fmax = MAX_FREQ_NYQUIST * fs;
    let mag045 = magnitude_db(h, fs, f045);
    let points = 1_024usize;
    let mut prev_f = MIN_FREQ_HZ;
    let mut prev_m = magnitude_db(h, fs, prev_f);
    let mut crossing = None;
    for i in 1..=points {
        let f = MIN_FREQ_HZ * (fmax / MIN_FREQ_HZ).powf(i as f64 / points as f64);
        let m = magnitude_db(h, fs, f);
        if prev_m > FAST_BOUNDARY_ATTENUATION_DB && m <= FAST_BOUNDARY_ATTENUATION_DB {
            let mut a = prev_f;
            let mut b = f;
            for _ in 0..60 {
                let mid = 0.5 * (a + b);
                if magnitude_db(h, fs, mid) > FAST_BOUNDARY_ATTENUATION_DB {
                    a = mid;
                } else {
                    b = mid;
                }
            }
            crossing = Some(0.5 * (a + b));
            break;
        }
        prev_f = f;
        prev_m = m;
    }
    Ok(FamilyBoundaryMetrics {
        fast_boundary_hz: f045,
        mag_db_at_045fs: mag045,
        crossing_minus60db_hz: crossing,
        slow_like: mag045 > FAST_BOUNDARY_ATTENUATION_DB,
    })
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
    fn new(h: &[f64], fs: f64, n: usize) -> Result<Self, MinSlowError> {
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

    /// 频带内 bin 的幅度最大值（dB；真正 H(k) 采样，不依赖插值）。
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

/// 通带幅度（spec §3.3 判据 1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PassbandReport {
    pub lo_hz: f64,
    pub hi_hz: f64,
    pub min_db: f64,
    pub max_db: f64,
    pub ripple_db: f64,
}

/// Nyquist 带幅度（spec §3.3 判据 2：[0.49fs, 0.499fs] 的声明衰减）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NyquistBandReport {
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// 带内 max|H|（dB）。
    pub max_db: f64,
    /// 实测衰减 = −max_db（dB）。
    pub measured_attenuation_db: f64,
    /// 声明衰减（dB；判据 <= −(declared−1)）。
    pub declared_attenuation_db: f64,
}

/// 慢滚降单调性（spec §3.3 判据 3：f > f_p 后 |H|dB 不得上翘超过 0.25 dB）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonotonicityReport {
    pub from_hz: f64,
    pub to_hz: f64,
    /// 频带内相对运行最小值的最大上翘（dB）。
    pub max_rise_db: f64,
    pub limit_db: f64,
}

/// slow/fast 家族互斥报告（spec §3.3 判据 6 / §3.8）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FamilyDistinctionReport {
    pub fast_boundary_hz: f64,
    /// 测得 M(0.45fs)（dB）；必须 > −60。
    pub mag_db_at_045fs: f64,
    /// 测得 −60 dB 穿越点；必须位于 0.45fs 之上（None = 测量带内未穿越）。
    pub crossing_minus60db_hz: Option<f64>,
    pub slow_like: bool,
}

impl FamilyDistinctionReport {
    /// 判据通过：M(0.45fs) > −60 dB 且（如有）穿越点在 0.45fs 之上。
    pub fn passes(&self) -> bool {
        self.slow_like
            && self
                .crossing_minus60db_hz
                .map_or(true, |f| f > self.fast_boundary_hz)
    }
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
    pub nyquist_band: NyquistBandReport,
    pub monotonicity: MonotonicityReport,
    pub family_distinction: FamilyDistinctionReport,
    pub endpoints: EndpointsReport,
    pub ringing: RingingReport,
    pub reference: ReferenceReport,
}

/// 群延迟报告（spec §3.3 判据 5 / §3.2 判据 3：频变 GD，禁止零延迟声明）。
#[derive(Debug, Clone, PartialEq)]
pub struct DelayReport {
    pub class: &'static str,
    /// 声明体延迟 D（samples）。min_slow 无体延迟，D=0；实际延迟看频变 GD/onset。
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
    /// 规范测量带 [20 Hz, min(20000, 0.499fs)] 的 GD 最大值（含过渡带）。
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

/// 最小相位验证报告（spec §3.3 判据 4 / §3.2 判据 2：M2 相位–Hilbert）。
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

/// 单条规范判据结果（spec §3.3 判据 1–6 + §3.1 判据 3–6 + §1.3/§1.4/§2.4）。
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
pub struct MinSlowDesign {
    pub params: MinSlowParams,
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

impl MinSlowDesign {
    pub fn family(&self) -> &'static str {
        FAMILY
    }

    pub fn accepted(&self) -> bool {
        self.criteria.all_pass()
    }

    pub fn failed_criteria(&self) -> Vec<&'static str> {
        self.criteria.failed_ids()
    }

    /// 延迟声明（spec §3.3 判据 5 / §3.2 判据 3：必须频变 GD 明示，不得声称零延迟）。
    pub fn delay_statement(&self) -> String {
        format!(
            "min_slow: frequency-varying group delay (NOT zero-delay); declared bulk D={:.1} samples; GD(1kHz)={:.4} samples ({:.6} ms); onset={} samples ({:.6} ms); gd_max={:.2} samples ({:.4} ms); M(0.45fs)={:.3} dB; -60dB crossing={:?} Hz",
            self.delay.declared_bulk_samples,
            self.delay.at_1khz_samples,
            self.delay.latency_ms_1khz,
            self.delay.onset_samples,
            self.delay.onset_ms,
            self.delay.band_max_samples,
            self.delay.gd_max_ms,
            self.response.family_distinction.mag_db_at_045fs,
            self.response.family_distinction.crossing_minus60db_hz,
        )
    }

    /// 人类可读摘要（证据日志/报告用；每行一条）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "family={} fs={:.1} taps={} passband=[{:.1},{:.1}] nyquist_attenuation_db={:.1} ripple_db={:.3}",
            FAMILY,
            self.params.fs,
            self.params.taps,
            self.response.passband.lo_hz,
            self.response.passband.hi_hz,
            self.params.nyquist_attenuation_db,
            self.params.ripple_db
        ));
        out.push(format!(
            "prototype: family={} transition={} taps={} beta={:.4} factorization={} cepstrum_fft={} mag_floor_db={:.1}",
            self.params.prototype.family,
            self.params.prototype.transition,
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
            "nyquist_band: [{:.1},{:.1}] max={:.3} dB measured_att={:.3} dB declared={:.1} dB (limit {:.3})",
            self.response.nyquist_band.lo_hz,
            self.response.nyquist_band.hi_hz,
            self.response.nyquist_band.max_db,
            self.response.nyquist_band.measured_attenuation_db,
            self.response.nyquist_band.declared_attenuation_db,
            -(self.params.nyquist_attenuation_db - NYQUIST_ATTENUATION_TOL_DB)
        ));
        out.push(format!(
            "rolloff: slow (raised-cosine f_p->Nyquist), NOT min_fast sharp cutoff; M(0.45fs)={:.3} dB; -60dB crossing={:?} Hz (spec §3.3 判据 6)",
            self.response.family_distinction.mag_db_at_045fs,
            self.response.family_distinction.crossing_minus60db_hz
        ));
        out.push(format!(
            "monotonicity: f>f_p max rise={:.6} dB (limit {:.2} dB)",
            self.response.monotonicity.max_rise_db, MONOTONIC_MAX_RISE_DB
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
// 设计主流程（spec §3.3 参考构造 1–3 + 规范性判据 1–6）
// ---------------------------------------------------------------------------

fn measurement_grid(fs: f64, points: usize) -> Vec<f64> {
    let hi = ENDPOINT_20K_HZ.min(MAX_FREQ_NYQUIST * fs);
    let lo = MIN_FREQ_HZ;
    if points < 2 || hi <= lo {
        return vec![lo];
    }
    let ratio = hi / lo;
    (0..points)
        .map(|i| lo * ratio.powf(i as f64 / (points - 1) as f64))
        .collect()
}

fn support_report(h: &[f64]) -> Result<SupportReport, MinSlowError> {
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

/// 慢滚降单调性测量（spec §3.3 判据 3）：f_p..0.499fs 内相对运行最小值的最大上翘。
fn monotonicity_report(p: &MinSlowParams, spectrum: &ResponseSpectrum) -> MonotonicityReport {
    let fp = p.passband_hz[1];
    let hi = MAX_FREQ_NYQUIST * p.fs;
    let points = 4_096usize;
    let mut run_min = spectrum.mag_db_at(fp);
    let mut max_rise = 0.0f64;
    for i in 1..=points {
        let f = fp * (hi / fp).powf(i as f64 / points as f64);
        let v = spectrum.mag_db_at(f);
        max_rise = max_rise.max(v - run_min);
        run_min = run_min.min(v);
    }
    MonotonicityReport {
        from_hz: fp,
        to_hz: hi,
        max_rise_db: max_rise.max(0.0),
        limit_db: MONOTONIC_MAX_RISE_DB,
    }
}

/// Nyquist 带幅度测量（spec §3.3 判据 2：[0.49fs, 0.499fs] 的 max|H|，取 bin 采样 + 密网插值较大者）。
fn nyquist_band_report(p: &MinSlowParams, spectrum: &ResponseSpectrum) -> NyquistBandReport {
    let lo = NYQUIST_BAND_LO_RATIO * p.fs;
    let hi = MAX_FREQ_NYQUIST * p.fs;
    let mut max_db = spectrum.max_mag_db_bins(lo, hi);
    let points = 1_024usize;
    for i in 0..=points {
        let f = lo + (hi - lo) * i as f64 / points as f64;
        max_db = max_db.max(spectrum.mag_db_at(f));
    }
    NyquistBandReport {
        lo_hz: lo,
        hi_hz: hi,
        max_db,
        measured_attenuation_db: -max_db,
        declared_attenuation_db: p.nyquist_attenuation_db,
    }
}

/// 设计 min_slow 系数并给出规范判据报告（spec §3.3）。
///
/// 返回 Ok(design) 表示参数合法且构造完成；是否满足全部规范判据看
/// MinSlowDesign::accepted / failed_criteria（判据失败表示该参数组合 INFEASIBLE，
/// 不是静默放行）。需要“必须通过”的调用方用 design_strict。
pub fn design(params: &MinSlowParams) -> Result<MinSlowDesign, MinSlowError> {
    validate_params(params)?;
    let mut warnings = Vec::new();

    // 1) 对称线性相位原型（spec §3.3 步骤 1–2）
    let prototype = symmetric_prototype(params)?;
    // 2) 同态最小相位谱分解（spec §3.3 步骤 3）
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

    // 4) 最小相位验证 M2（spec §3.3 判据 4 / §3.2 判据 2，N=65536）
    let m2_deg = min_phase_consistency_from_spectrum(&re, &im, params.fs, &response_fft)?;
    let inverse_causality_db =
        inverse_causality_diagnostic_db(&coefficients, RESPONSE_FFT, MIN_PHASE_LOG_FLOOR_DB)
            .unwrap_or(f64::NAN);
    let m2_passed = m2_deg <= MIN_PHASE_MAX_DEG;
    if !m2_passed {
        warnings.push(format!(
            "M2 相位–Hilbert 一致性 {m2_deg:.6}° > {MIN_PHASE_MAX_DEG}°（spec §3.3 判据 4）：截断支撑下最小相位不成立 → 报告 FAIL（不静默放行）"
        ));
    } else if m2_deg > 0.5 {
        warnings.push(format!(
            "M2 一致性 {m2_deg:.4}° 接近/高于工程典型值 0.5°（限 {MIN_PHASE_MAX_DEG}°），建议增大 cepstrum_fft 或 prototype.taps"
        ));
    }

    // 5) 测量网格与通带（spec §1.2/§3.3 判据 1）
    let grid_points = (8 * params.taps).max(8_192);
    let grid = measurement_grid(params.fs, grid_points);
    let [pb_lo, pb_hi] = params.passband_hz;
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

    // 6) Nyquist 带（spec §3.3 判据 2）与慢滚降单调性（判据 3）
    let nyquist_band = nyquist_band_report(params, &spectrum);
    let monotonicity = monotonicity_report(params, &spectrum);

    // 7) slow/fast 家族互斥（spec §3.3 判据 6 / §3.8）
    let boundary = family_boundary_metrics(&coefficients, params.fs)?;
    let family_distinction = FamilyDistinctionReport {
        fast_boundary_hz: boundary.fast_boundary_hz,
        mag_db_at_045fs: boundary.mag_db_at_045fs,
        crossing_minus60db_hz: boundary.crossing_minus60db_hz,
        slow_like: boundary.slow_like,
    };
    if let Some(c) = family_distinction.crossing_minus60db_hz {
        if c <= family_distinction.fast_boundary_hz {
            warnings.push(format!(
                "测得 −60 dB 穿越点 {c:.3} Hz <= 0.45*fs={:.3} Hz：该设计更像 min_fast，min_slow 家族互斥判据 FAIL（spec §3.3 判据 6）",
                family_distinction.fast_boundary_hz
            ));
        }
    }

    // 8) 端点（spec §2.4）
    let endpoints = EndpointsReport {
        f20: endpoint_at(&coefficients, params.fs, MIN_FREQ_HZ),
        f20k: endpoint_at(&coefficients, params.fs, ENDPOINT_20K_HZ),
    };

    // 9) 振铃（spec §1.4）
    let ringing = ringing_report(&coefficients, params.fs, &support);

    // 10) 参考归一化（spec §1.5：DC 声明 0 dB；无隐藏增益）
    let dc_sum: f64 = coefficients.iter().sum();
    let measured_gain_db = db_from_power(dc_sum * dc_sum);
    let reference = ReferenceReport {
        hz: REFERENCE_HZ_DC,
        gain_db: 0.0,
        measured_gain_db,
    };

    // 11) 群延迟报告（spec §3.3 判据 5 / §3.2 判据 3；频变 GD + onset，不冒称零延迟）
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
    let at_1khz = if 1_000.0 <= MAX_FREQ_NYQUIST * params.fs {
        spectrum.gd_at(1_000.0)
    } else {
        f64::NAN
    };
    let f045 = family_distinction.fast_boundary_hz;
    let mut characteristic_freqs = vec![
        MIN_FREQ_HZ,
        50.0,
        100.0,
        1_000.0,
        5_000.0,
        10_000.0,
        15_000.0,
        pb_hi,
        20_000.0,
        22_000.0,
        f045,
    ];
    characteristic_freqs
        .retain(|f| f.is_finite() && *f > 0.0 && *f <= MAX_FREQ_NYQUIST * params.fs);
    characteristic_freqs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    characteristic_freqs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let characteristic: Vec<(f64, f64)> = characteristic_freqs
        .into_iter()
        .map(|f| (f, spectrum.gd_at(f)))
        .collect();
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

    // 12) 极点/稳定性边界（spec §3.2 判据 5）：FIR 分母为 1，极点全在 z=0
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

    // 13) 规范判据（spec §3.3 判据 1–6 + §3.1 判据 3–6 + §1.3/§1.4/§2.4）
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
        "通带 [20,f_p] p-p 纹波 <= ripple_db+0.05（spec §3.3 判据 1 / §3.1 判据 3）",
        Some(passband_ripple),
        Some(params.ripple_db + RIPPLE_TOL_DB),
        "dB",
        passband_ripple <= params.ripple_db + RIPPLE_TOL_DB,
    ));
    let nyq_limit = -(params.nyquist_attenuation_db - NYQUIST_ATTENUATION_TOL_DB);
    criteria.push(criterion(
        "nyquist_attenuation",
        "Nyquist 带 [0.49fs,0.499fs] max|H| <= -(declared-1)（spec §3.3 判据 2）",
        Some(nyquist_band.max_db),
        Some(nyq_limit),
        "dB",
        nyquist_band.max_db <= nyq_limit,
    ));
    criteria.push(criterion(
        "monotonic_rolloff",
        "f>f_p 后 |H|dB 相对运行最小值上翘 <= 0.25 dB（spec §3.3 判据 3）",
        Some(monotonicity.max_rise_db),
        Some(MONOTONIC_MAX_RISE_DB),
        "dB",
        monotonicity.max_rise_db <= MONOTONIC_MAX_RISE_DB,
    ));
    criteria.push(criterion(
        "min_phase_m2",
        "M2 相位–Hilbert 一致性 <= 2.0°（spec §3.3 判据 4 / §3.2 判据 2）",
        Some(m2_deg),
        Some(MIN_PHASE_MAX_DEG),
        "deg",
        m2_passed,
    ));
    criteria.push(criterion(
        "delay_report",
        "频变 GD/onset/gd_max 已报告且 zero_delay_claim=false（spec §3.3 判据 5：禁止声称零延迟）",
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
        "f_p <= 0.42*fs（spec §3.3 参考构造/判据 6）",
        Some(pb_hi / params.fs),
        Some(FAMILY_EDGE_FP_RATIO),
        "ratio",
        pb_hi <= FAMILY_EDGE_FP_RATIO * params.fs,
    ));
    criteria.push(criterion(
        "family_distinction_slow_vs_fast",
        "M(0.45fs) > -60 dB 且 -60dB 穿越点 > 0.45fs（否则应改 min_fast；spec §3.3 判据 6 / §3.8）",
        Some(family_distinction.mag_db_at_045fs),
        Some(FAST_BOUNDARY_ATTENUATION_DB),
        "dB",
        family_distinction.passes(),
    ));
    criteria.push(criterion(
        "reference_gain",
        "|H(DC)|dB 与声明 gain_db 差 <= 0.01 dB（spec §1.5/§3.1 判据 6）",
        Some((measured_gain_db - reference.gain_db).abs()),
        Some(REFERENCE_TOL_DB),
        "dB",
        (measured_gain_db - reference.gain_db).abs() <= REFERENCE_TOL_DB,
    ));

    // 14) 警告（工程提示；不影响判据）
    if params.cepstrum_fft < 8 * params.taps {
        warnings.push(format!(
            "cepstrum_fft={} < 8*taps={}：倒谱分辨率偏低，建议增大（spec 参考构造 32768 对 1024 taps）",
            params.cepstrum_fft,
            8 * params.taps
        ));
    }
    if params.prototype.taps < 8 * params.taps {
        warnings.push(format!(
            "prototype.taps={} < 8*taps={}：原型合成网格偏小，别名风险（spec 参考构造 32768 对 1024 taps）",
            params.prototype.taps,
            8 * params.taps
        ));
    }
    if params.nyquist_attenuation_db > 45.0 {
        warnings.push(format!(
            "nyquist_attenuation_db={:.1} > 45 dB：升余弦到 Nyquist 的构造通常不可达（建议降低声明或改 min_fast）",
            params.nyquist_attenuation_db
        ));
    }
    let nyq_margin = nyq_limit - nyquist_band.max_db;
    if nyq_margin < 1.0 {
        warnings.push(format!(
            "Nyquist 带衰减余量仅 {nyq_margin:.3} dB（<1 dB）：建议降低 f_p 或重新评估声明"
        ));
    }
    let ripple_margin = params.ripple_db + RIPPLE_TOL_DB - passband_ripple;
    if ripple_margin < 0.01 {
        warnings.push(format!(
            "通带纹波余量仅 {ripple_margin:.6} dB（<0.01 dB）：建议增大 taps"
        ));
    }
    if pb_hi > 0.9 * FAMILY_EDGE_FP_RATIO * params.fs {
        warnings.push(format!(
            "f_p={pb_hi} 接近 min_slow 家族上界 0.42*fs={:.3}：过渡带变窄，Nyquist 带衰减会变浅",
            FAMILY_EDGE_FP_RATIO * params.fs
        ));
    }
    if params.fs != DEFAULT_FS && params.fs != 96_000.0 {
        warnings.push(format!(
            "fs={} 不在 manifest 允许的 48000/96000（spec §2.2）：本设计仅工具层有效",
            params.fs
        ));
    }

    Ok(MinSlowDesign {
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
            nyquist_band,
            monotonicity,
            family_distinction,
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

/// 严格设计：构造完成且全部规范判据通过才返回 Ok；否则按失败类别返回稳定错误码：
/// - slow/fast 家族互斥失败 → E_FIR_FAMILY（spec §3.3 判据 6）
/// - 最小相位 M2 失败 → E_FIR_MINPHASE（spec §2.5）
/// - 其余不可达/预算类失败 → E_FIR_BUDGET（INFEASIBLE，不静默截断/放行）
pub fn design_strict(params: &MinSlowParams) -> Result<MinSlowDesign, MinSlowError> {
    let d = design(params)?;
    if d.accepted() {
        return Ok(d);
    }
    let failed = d.failed_criteria();
    let code = if failed.contains(&"family_distinction_slow_vs_fast")
        || failed.contains(&"family_boundary")
    {
        E_FIR_FAMILY
    } else if failed.contains(&"min_phase_m2") {
        E_FIR_MINPHASE
    } else {
        E_FIR_BUDGET
    };
    err(
        code,
        format!(
            "INFEASIBLE（不是静默放行）：未通过判据 {}",
            failed.join(",")
        ),
    )
}

// ---------------------------------------------------------------------------
// 私有测试（tests/T34 通过 include! 进入本模块的 #[cfg(test)] 子模块）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod min_slow_tests {
    use super::*;

    include!("../tests/T34/min_slow_cases.rs");
}
