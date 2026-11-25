//! [NONRT] phase / delay / polarity FIR 工具（WBS T40）—— T40 写域。
//!
//! 本模块只实现 **phase / delay / polarity** 这一个相邻工具（spec/fir-toolbox.md v1.2-draft §4.2），
//! 不把七家族（linear/min_fast/min_slow/apodizing/brickwall/corrected/nos）或 crossover/modal PEQ
//! 藏进本文件：那些各有独立 leaf。
//!
//! 权威契约：
//! - rt-stack/spec/fir-toolbox.md v1.2-draft §4.2（整数 delay / 分数 delay / 极性 / 作用方向 / 因果）、
//!   §1.2（响应测量程序）、§1.3（支撑/有效支撑/延迟 D 与 onset）、§1.4（振铃排除窗口径）、
//!   §1.5（参考归一化 ±0.01 dB）、§2.2（tspi-fir/v1 manifest 字段与规则 2/3/4）、§2.4（20 Hz/20 kHz
//!   Endpoint 联合类型）、§2.5（E_FIR_* 失败码）、§7.1（P/G 行列方向与零填充线性卷积）、
//!   §9 V10（零填充必要性）/V11（63-tap Kaiser beta 8.96、d=0.5 分数延迟参考值）；
//! - scratch/lead/planner/DSP-HOST-DECISION.md §3（P=diag(p_j)、输入 delay/polarity 作用列、
//!   输出作用行、phase 乘谱要 zero padding 实现线性卷积、不循环混叠）；
//! - scratch/lead/planner/TSPI-RT-STACK-WBS.md T40 行（phase/delay/polarity FIR 工具，输入/输出方向、
//!   整数/分数 delay/taps 支撑与因果，独立解析向量）；
//! - calibration-spec/CONTRACT.md §3.2（校准 invert 与设计极性 XOR，不得双乘）。
//!
//! 能力：
//! 1. 整数延迟：h[n]=delta[n−D]，D 为整数 >=0；默认最小支撑 L=D+1，可显式声明更长的 taps 做尾部零填充
//!    （禁止缩短到 <D+1）；报告 D、delay_ms、onset、有效支撑与尾部零填充事实。
//! 2. 分数延迟：窗函数 sinc（Kaiser/Hann/Blackman-Harris，窗长 Lw >=63 且奇数、总延迟 D >= (Lw−1)/2
//!    才因果）或 Lagrange（order 1..=15，总延迟 D ∈ [order/2, order/2+1)）。窗函数 sinc 用
//!    h[shift+k]=sinc(k−c−frac)·w[k]，支撑 L=floor(D)−c+Lw（可表达任意整数+分数总延迟，同时保持
//!    taps 支撑明确）。分数延迟按 §4.2 判据在 [20 Hz, 0.9·fs/2] 内验证 |H|dB 与 0 dB 差 <=0.1 dB、
//!    GD 与声明 D 差 <=0.02 samples；作者参考 V11（63-tap Kaiser beta 8.96、d=0.5、D=31.5）可复算。
//!    Lagrange 属显式低带方法：默认全带判据会 INFEASIBLE（E_FIR_BUDGET），调用方必须显式声明较窄
//!    verify band；报告写 spec_band_compliant=false，绝不静默放宽判据。
//! 3. 极性：invert = 系数整体乘 −1（精确），报告 polarity；与校准 invert 是 XOR 语义
//!    （CONTRACT §3.2），同一反相只能在设计系数或校准两处之一应用，报告 effective 极性。
//! 4. 输入/输出方向：ApplySide::InputColumns 表示 delay/polarity 作用于列（A/G 的源输入），
//!    ApplySide::OutputRows 表示作用于行（P_post 的输出）；提供矩阵路由合成 apply_directional_phase，
//!    非对称矩阵下两者不可交换（LTI 整体相同 op 且矩阵对称才可交换）。
//! 5. 因果：任何负的/提前 delay 必须由 bulk target delay D 吸收（plan_causal_delay），
//!    否则 E_FIR_NONCAUSAL；分数延迟窗函数中心在 support_start+c，不把“支撑长度”说成“固有延迟”。
//! 6. 独立验证：直接 DTFT（逐点复指数求和，非 FFT）复算幅度/相位，解析群延迟
//!    GD=Re(B/H)（B=Σn·h[n]e^{-jωn}，不是有限差分）；规范测量网格 N>=max(8192,8·L)；20 Hz/20 kHz
//!    Endpoint 联合类型、振铃（W=ceil(0.000333·fs)）、参考归一化、确定性（两次设计逐位一致）。
//! 7. 预算事实：taps/常规 4096/extended 16384/>16384 offline、f32 内存、FIR delay_ms 与
//!    15/25/40 ms 整链预算的“事实”字段；准入判定归 T56/T55，本模块不宣称满足整链预算。
//!
//! 明确非目标（不得由本模块冒充）：
//! - 不写 WAV/manifest 文件、不生成 sha256（T55）；不做 target 导入（T11）、不做 P/G 融合（T12）、
//!   不做 crossover（T39）、不做 modal PEQ（T41）、不做 IIR、不做 limiter/clip（T21/T23）、
//!   不做 profile 资源准入（T56）；
//! - 不碰板端/ALSA/CueMix/服务/模式/S1；不 git commit；不改 Cargo.toml/lib.rs/public_registration.rs。
//!
//! 登记（契约边界，不私自扩张 manifest schema）：
//! - spec §1.7 的 family 只有七类，phase/delay/polarity 是 §4.2 相邻工具而非 family；本模块提供
//!   FAMILY_NOTE / PhaseManifestDeclaration.family_note，T55 落 tspi-fir/v1 时必须把它写进
//!   design/report 段（或先由 T04/Lead 裁决新增字段），本模块不把 phase 冒充成七家族之一。
//! - §4.2 的分数延迟判据文字覆盖 Kaiser/Hann 与 Lagrange；实测 Hann 63-tap 在 0.9·fs/2 的 GD 偏差
//!   0.045 samples 超 0.02（Hann 127-tap 才 0.013），故本模块：
//!   (a) 窗函数 sinc 路径按 §4.2 全带判据强制，Hann 低 taps 会验证失败并由 design_checked 报 INFEASIBLE；
//!   (b) Lagrange 明确标注为“低带/短支撑补充方法”，只有显式声明 verify_band_hz 才可能通过，报告
//!       写 spec_band_compliant=false + warning；不把这种情形写成 §4.2 全带合规。
//!
//! 精度/确定性：设计 f64；同一输入同一版本两次设计系数逐位一致（无时间戳/随机数/未排序 map）。
//!
//! 私有测试：tests/T40/phase_cases.rs 由本文件末尾 #[cfg(test)] mod phase_tests 通过 include! 编译
//! 执行（cargo 不自动发现 tests/<Txx>/ 子目录；公共注册属 integrator，不改 public_registration.rs）。

use std::f64::consts::PI;
use std::fmt;

// ---------------------------------------------------------------------------
// 常量（契约来源见各行注释）
// ---------------------------------------------------------------------------

/// 工具标识：phase/delay/polarity 是 spec §4.2 相邻工具，不是七家族之一。
pub const TOOL_KIND: &str = "phase-delay-polarity";

/// 工具版本（同输入同版本确定性；不进系数哈希的 created_unix 由 T55 处理）。
pub const TOOL_VERSION: &str = "0.1.0";

/// §1.7 七家族之外的登记说明（T55 落 manifest 时不得冒充 family）。
pub const FAMILY_NOTE: &str = "phase/delay/polarity 是 spec §4.2 相邻工具，不是七家族之一；tspi-fir/v1 manifest 的 family 字段未列此工具，T55 必须放入 design/report 段或先经 T04/Lead 裁决新增字段";

/// 分数延迟窗函数最小窗长（§4.2：taps >= 63）。
pub const MIN_WINDOW_TAPS: usize = 63;

/// V11 作者参考的 Kaiser beta（§4.2/§9 V11）。
pub const DEFAULT_KAISER_BETA: f64 = 8.96;

/// V11 作者参考的窗长（63-tap）。
pub const V11_WINDOW_TAPS: usize = MIN_WINDOW_TAPS;

/// V11 作者参考的分数延迟 d。
pub const V11_FRACTIONAL_DELAY: f64 = 0.5;

/// V11 作者参考的总延迟 D（31.5 samples）。
pub const V11_TOTAL_DELAY_SAMPLES: f64 = 31.5;

/// V11 记录的 20 Hz–20 kHz 最大 |H| 偏差（dB，§9 V11：0.00977）。
pub const V11_MAX_MAG_DEVIATION_DB: f64 = 0.009_77;

/// V11 记录的 DC 偏差（dB，§9 V11：DC −0.00954）。
pub const V11_DC_DEVIATION_DB: f64 = -0.009_54;

/// V11 记录的 GD−31 容差（samples，§9 V11：0.500 ± 0.0002）。
pub const V11_GD_MINUS_31_TOLERANCE: f64 = 0.0002;

/// §4.2 分数延迟幅度判据：|H|dB 与 0 dB 差 <= 0.1 dB。
pub const FRACTIONAL_MAG_TOL_DB: f64 = 0.1;

/// §4.2 分数延迟群延迟判据：GD 与声明 D 差 <= 0.02 samples。
pub const FRACTIONAL_GD_TOL_SAMPLES: f64 = 0.02;

/// §4.2 分数延迟判据频带上界比例：0.9·fs/2。
pub const FRACTIONAL_BAND_HI_FRACTION: f64 = 0.9;

/// §1.2 测量网格下界（Hz）。
pub const MEASUREMENT_LO_HZ: f64 = 20.0;

/// §2.4 20 kHz 端点频点（Hz）。
pub const ENDPOINT_HI_HZ: f64 = 20_000.0;

/// §1.2/§2.4 Nyquist 护栏比例 0.499·fs。
pub const NYQUIST_GUARD_FRACTION: f64 = 0.499;

/// §1.5 参考归一化容差（dB）。
pub const REFERENCE_TOLERANCE_DB: f64 = 0.01;

/// §1.2 |H| 下限截到 1e-12（−240 dB）。
pub const MAGNITUDE_FLOOR: f64 = 1e-12;

/// §1.3 有效支撑 eps（相对 max|h|）。
pub const SUPPORT_EPS: f64 = 1e-6;

/// §1.4 振铃排除窗口比例（W=ceil(0.000333·fs)，48k=16）。
pub const RINGING_EXCLUSION_SECONDS: f64 = 0.000_333;

/// §2.1 设计 taps 上限（>16384 只能由其它离线工具处理）。
pub const MAX_TAPS: usize = 16_384;

/// §7.2 常规 profile 每路由有效支撑上限（事实常量，准入归 T56）。
pub const REGULAR_PROFILE_TAPS_CAP: usize = 4_096;

/// §7.2 extended profile 上限（8192/16384 需准入；本模块不判定准入）。
pub const EXTENDED_PROFILE_TAPS_CAP: usize = 16_384;

/// 每个 f32 系数字节数（§7.2 内存估算 4·taps bytes/路由）。
pub const F32_BYTES: usize = 4;

/// 设计模块工程采样率范围下限（Hz；典型 48k/96k，8k 为语音域下限）。
pub const FS_ENGINEERING_MIN_HZ: f64 = 8_000.0;

/// 设计模块工程采样率范围上限（Hz）。
pub const FS_ENGINEERING_MAX_HZ: f64 = 384_000.0;

/// Lagrange 允许的最大阶（短支撑低带方法；再高数值不稳定且与 §4.2 全带判据无意义）。
pub const MAX_LAGRANGE_ORDER: usize = 15;

/// 延迟报告残差容差（samples；§3.6 目标延迟残差口径 <=0.01，本工具沿用）。
pub const DELAY_RESIDUAL_TOLERANCE: f64 = 0.01;

/// delay_ms 报告一致性容差（ms）。
pub const DELAY_MS_TOLERANCE: f64 = 1e-6;

/// 整链附加延迟预算候选（ms；§1.1/主文：15/25/40，只报 FIR 事实，整链判定归 T55/T56）。
pub const DELAY_BUDGET_MS: [f64; 3] = [15.0, 25.0, 40.0];

/// 极性 invert 的精确符号。
pub const POLARITY_INVERT_SIGN: f64 = -1.0;

/// §1.2 规范测量网格最小点数。
pub const MIN_MEASUREMENT_GRID_POINTS: usize = 8_192;

/// §1.2 测量网格点数与支撑关系的系数：N >= max(8192, 8·L)。
pub const MEASUREMENT_GRID_FACTOR: usize = 8;

/// 输入侧方向说明（§4.2/§7.1）。
pub const INPUT_SIDE_STATEMENT: &str =
    "输入侧 delay/polarity 作用于列（A/G 的源输入）：G 的每一列是某输入源到全部输出行，逐列延迟/反相后再进矩阵";

/// 输出侧方向说明（§4.2/§7.1）。
pub const OUTPUT_SIDE_STATEMENT: &str =
    "输出侧 delay/polarity 作用于行（P_post 的输出）：G 的每一行是某输出角色，逐行延迟/反相后再 scatter";

/// 方向不可交换说明（§4.2/§7.1）。
pub const COMMUTATIVITY_STATEMENT: &str =
    "G/P 矩阵非对称时输入侧与输出侧 delay/polarity 不可交换；仅当全部通道的 delay/polarity 相同（LTI 公共时移/符号）时才可交换，报告必须写清作用侧";

/// 与校准 invert 的 XOR 语义（§4.2；CONTRACT §3.2）。
pub const CALIBRATION_XOR_STATEMENT: &str =
    "极性 invert 与校准 invert 是 XOR 语义（CONTRACT §3.2）：同一反相只能在设计系数或校准 invert 一处应用，两处都 invert 等于不反相，禁止双乘";

/// 因果说明（§4.2）。
pub const CAUSALITY_STATEMENT: &str =
    "任何负的/提前的延迟必须由 bulk target_delay_samples/D 吸收（total=bulk+relative>=0），否则 E_FIR_NONCAUSAL；因果 FIR 不得把支撑长度当固有延迟";

/// 规范链序说明（§5.1）。
pub const CANONICAL_CHAIN_STATEMENT: &str =
    "规范链序：输入选择 A_map → G → P_post（crossover HP/LP → 模态 PEQ → 输出目标 FIR → gain → delay → polarity）→ 保护/limiter → scatter；本工具的 delay/polarity 默认位于 P_post 的输出行";

/// 错误码（§2.5）：taps 非法。
pub const E_FIR_TAPS: &str = "E_FIR_TAPS";
/// 错误码（§2.5）：delay 规则/ ms 不一致。
pub const E_FIR_DELAY: &str = "E_FIR_DELAY";
/// 错误码（§2.5）：非有限/全零。
pub const E_FIR_NUMERIC: &str = "E_FIR_NUMERIC";
/// 错误码（§2.5）：频带非法。
pub const E_FIR_BAND: &str = "E_FIR_BAND";
/// 错误码（§2.5）：支撑/可行性/延迟超预算（INFEASIBLE）。
pub const E_FIR_BUDGET: &str = "E_FIR_BUDGET";
/// 错误码（§2.5）：不可因果。
pub const E_FIR_NONCAUSAL: &str = "E_FIR_NONCAUSAL";
/// 错误码（§2.5）：参考归一化。
pub const E_FIR_REF: &str = "E_FIR_REF";
/// 本模块 tool API 参数误用（不是 tspi-fir/v1 manifest 失败码）。
pub const E_PHASE_ARG: &str = "E_PHASE_ARG";

/// §2.5 失败码集中表（本模块使用子集 + E_PHASE_ARG）。
pub const PHASE_ERROR_CODE_TABLE: &[(&str, &str)] = &[
    (E_FIR_TAPS, "taps/窗长/support 非法、与长度不符或 < D+1"),
    (
        E_FIR_DELAY,
        "delay_samples 与规则不符、delay_ms 不一致、目标残差 >0.01 sample",
    ),
    (E_FIR_NUMERIC, "输入含 NaN/Inf 或系数全零"),
    (E_FIR_BAND, "采样率/验证频带非法或越出 [20, 0.9·fs/2]"),
    (
        E_FIR_BUDGET,
        "支撑/延迟超 16384 taps 或分数延迟判据 INFEASIBLE",
    ),
    (
        E_FIR_NONCAUSAL,
        "负/提前延迟未被 bulk D 吸收，或窗函数中心 < 0",
    ),
    (E_FIR_REF, "参考归一化超出 ±0.01 dB"),
    (
        E_PHASE_ARG,
        "本工具 API 参数误用（非 manifest 码）：side/op 数量/矩阵 shape 非法",
    ),
];

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// 本模块错误（code + 中文 detail；code 取 §2.5 子集或 E_PHASE_ARG）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseError {
    code: &'static str,
    detail: String,
}

impl PhaseError {
    /// 构造错误。
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        PhaseError {
            code,
            detail: detail.into(),
        }
    }

    /// §2.5 失败码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 中文细节。
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// E_FIR_TAPS。
    pub fn taps(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_TAPS, detail)
    }

    /// E_FIR_DELAY。
    pub fn delay(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_DELAY, detail)
    }

    /// E_FIR_NUMERIC。
    pub fn numeric(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_NUMERIC, detail)
    }

    /// E_FIR_BAND。
    pub fn band(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_BAND, detail)
    }

    /// E_FIR_BUDGET。
    pub fn budget(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_BUDGET, detail)
    }

    /// E_FIR_NONCAUSAL。
    pub fn noncausal(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_NONCAUSAL, detail)
    }

    /// E_FIR_REF。
    pub fn reference(detail: impl Into<String>) -> Self {
        Self::new(E_FIR_REF, detail)
    }

    /// E_PHASE_ARG（本工具 API 误用，不是 manifest 码）。
    pub fn arg(detail: impl Into<String>) -> Self {
        Self::new(E_PHASE_ARG, detail)
    }
}

impl fmt::Display for PhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for PhaseError {}

// ---------------------------------------------------------------------------
// 极性 / 方向 / 窗 / 方法
// ---------------------------------------------------------------------------

/// 极性（§4.2：invert = 系数整体乘 −1 或运行期符号，两者等价；报告 polarity 字段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// 正常极性（+1）。
    Normal,
    /// 反相（−1）。
    Invert,
}

impl Polarity {
    /// 精确符号（+1.0 / −1.0）。
    pub fn sign(self) -> f64 {
        match self {
            Polarity::Normal => 1.0,
            Polarity::Invert => POLARITY_INVERT_SIGN,
        }
    }

    /// 取反。
    pub fn inverted(self) -> Self {
        match self {
            Polarity::Normal => Polarity::Invert,
            Polarity::Invert => Polarity::Normal,
        }
    }

    /// XOR 组合（校准 invert 语义，CONTRACT §3.2）。
    pub fn xor(self, other: Self) -> Self {
        match (self, other) {
            (Polarity::Normal, Polarity::Normal) => Polarity::Normal,
            (Polarity::Normal, Polarity::Invert) => Polarity::Invert,
            (Polarity::Invert, Polarity::Normal) => Polarity::Invert,
            (Polarity::Invert, Polarity::Invert) => Polarity::Normal,
        }
    }

    /// schema 字符串（normal / invert）。
    pub fn as_str(self) -> &'static str {
        match self {
            Polarity::Normal => "normal",
            Polarity::Invert => "invert",
        }
    }

    /// 由 ±1 精确符号反推（其它值返回 None，不猜）。
    pub fn from_sign(sign: f64) -> Option<Self> {
        if sign == 1.0 {
            Some(Polarity::Normal)
        } else if sign == POLARITY_INVERT_SIGN {
            Some(Polarity::Invert)
        } else {
            None
        }
    }
}

/// delay/polarity 的作用侧（§4.2/§7.1：输入侧作用列、输出侧作用行，不可交换）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplySide {
    /// 输入侧：作用于列（A/G 的源输入）。
    InputColumns,
    /// 输出侧：作用于行（P_post 的输出）——规范链序默认位置。
    OutputRows,
}

impl ApplySide {
    /// 默认作用侧：P_post 输出行（§5.1 规范链序）。
    pub fn default_side() -> Self {
        ApplySide::OutputRows
    }

    /// schema 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            ApplySide::InputColumns => "input_columns",
            ApplySide::OutputRows => "output_rows",
        }
    }

    /// 维度文字：行/列。
    pub fn dimension(self) -> &'static str {
        match self {
            ApplySide::InputColumns => "列",
            ApplySide::OutputRows => "行",
        }
    }

    /// 作用对象说明（A/G 源输入 / P 的输出）。
    pub fn scope(self) -> &'static str {
        match self {
            ApplySide::InputColumns => "A/G 的源输入（输入选择与低带矩阵之前）",
            ApplySide::OutputRows => {
                "P_post 的输出（crossover/PEQ/目标 FIR 之后、保护与 scatter 之前）"
            }
        }
    }

    /// 规范方向说明语句。
    pub fn statement(self) -> &'static str {
        match self {
            ApplySide::InputColumns => INPUT_SIDE_STATEMENT,
            ApplySide::OutputRows => OUTPUT_SIDE_STATEMENT,
        }
    }
}

/// 分数延迟窗函数（§4.2：Kaiser/Hann；Blackman-Harris 为同族补充）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FractionalWindow {
    /// Kaiser 窗（beta 由调用方声明；V11 参考 8.96）。
    Kaiser {
        /// Kaiser beta。
        beta: f64,
    },
    /// Hann 窗（固定公式）。
    Hann,
    /// Blackman-Harris 4 项窗（固定公式）。
    BlackmanHarris,
}

impl FractionalWindow {
    /// V11 参考的 Kaiser beta=8.96。
    pub fn kaiser_v11() -> Self {
        FractionalWindow::Kaiser {
            beta: DEFAULT_KAISER_BETA,
        }
    }

    /// 任意 beta 的 Kaiser。
    pub fn kaiser(beta: f64) -> Self {
        FractionalWindow::Kaiser { beta }
    }

    /// window 名称。
    pub fn label(self) -> &'static str {
        match self {
            FractionalWindow::Kaiser { .. } => "kaiser",
            FractionalWindow::Hann => "hann",
            FractionalWindow::BlackmanHarris => "blackman_harris",
        }
    }

    /// Kaiser beta（非 Kaiser 为 None）。
    pub fn beta(self) -> Option<f64> {
        match self {
            FractionalWindow::Kaiser { beta } => Some(beta),
            _ => None,
        }
    }

    /// 生成窗值（长度 taps；taps>=2）。
    pub fn values(self, taps: usize) -> Vec<f64> {
        match self {
            FractionalWindow::Kaiser { beta } => kaiser_window_values(taps, beta),
            FractionalWindow::Hann => hann_window_values(taps),
            FractionalWindow::BlackmanHarris => blackman_harris_window_values(taps),
        }
    }
}

/// 分数延迟方法（§4.2：窗函数 sinc 或 Lagrange）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FractionalMethod {
    /// 窗函数 sinc：h[shift+k]=sinc(k−c−frac)·w[k]，需窗长 >=63 且奇数。
    WindowedSinc {
        /// 窗函数。
        window: FractionalWindow,
    },
    /// Lagrange 分数延迟：order+1 taps，低带/短支撑方法。
    Lagrange {
        /// 阶数（taps=order+1）。
        order: usize,
    },
}

impl FractionalMethod {
    /// Kaiser 窗 sinc 方法（V11 默认）。
    pub fn kaiser_sinc() -> Self {
        FractionalMethod::WindowedSinc {
            window: FractionalWindow::kaiser_v11(),
        }
    }

    /// Hann 窗 sinc 方法。
    pub fn hann_sinc() -> Self {
        FractionalMethod::WindowedSinc {
            window: FractionalWindow::Hann,
        }
    }

    /// Blackman-Harris 窗 sinc 方法。
    pub fn blackman_harris_sinc() -> Self {
        FractionalMethod::WindowedSinc {
            window: FractionalWindow::BlackmanHarris,
        }
    }

    /// 带窗的 sinc。
    pub fn windowed(window: FractionalWindow) -> Self {
        FractionalMethod::WindowedSinc { window }
    }

    /// Lagrange 方法。
    pub fn lagrange(order: usize) -> Self {
        FractionalMethod::Lagrange { order }
    }

    /// 方法名称。
    pub fn label(self) -> &'static str {
        match self {
            FractionalMethod::WindowedSinc { .. } => "windowed_sinc",
            FractionalMethod::Lagrange { .. } => "lagrange",
        }
    }

    /// 是否窗函数 sinc。
    pub fn is_windowed_sinc(self) -> bool {
        matches!(self, FractionalMethod::WindowedSinc { .. })
    }

    /// 窗函数（仅 WindowedSinc）。
    pub fn window(self) -> Option<FractionalWindow> {
        match self {
            FractionalMethod::WindowedSinc { window } => Some(window),
            FractionalMethod::Lagrange { .. } => None,
        }
    }

    /// Lagrange 阶（仅 Lagrange）。
    pub fn lagrange_order(self) -> Option<usize> {
        match self {
            FractionalMethod::Lagrange { order } => Some(order),
            FractionalMethod::WindowedSinc { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 延迟种类 / spec
// ---------------------------------------------------------------------------

/// 设计延迟种类（manifest/report 用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DelayKind {
    /// 整数延迟 D（samples）。
    Integer {
        /// 整数延迟 D。
        delay_samples: u64,
    },
    /// 分数延迟：整数部分 + 分数部分 d。
    Fractional {
        /// 整数延迟部分（floor(D)）。
        integer_delay_samples: u64,
        /// 分数延迟 d ∈ [0,1)。
        fractional_delay_d: f64,
        /// 方法。
        method: FractionalMethod,
    },
}

impl DelayKind {
    /// 是否整数延迟。
    pub fn is_integer(self) -> bool {
        matches!(self, DelayKind::Integer { .. })
    }

    /// 分数延迟 d（整数延迟为 None）。
    pub fn fractional_delay_d(self) -> Option<f64> {
        match self {
            DelayKind::Integer { .. } => None,
            DelayKind::Fractional {
                fractional_delay_d, ..
            } => Some(fractional_delay_d),
        }
    }

    /// 分数延迟方法（整数延迟为 None）。
    pub fn method(self) -> Option<FractionalMethod> {
        match self {
            DelayKind::Integer { .. } => None,
            DelayKind::Fractional { method, .. } => Some(method),
        }
    }

    /// schema 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            DelayKind::Integer { .. } => "integer_delay",
            DelayKind::Fractional { .. } => "fractional_delay",
        }
    }
}

/// 整数延迟 spec（§4.2：h[n]=delta[n−D]，D 整数 >=0）。
#[derive(Clone, Debug, PartialEq)]
pub struct IntegerDelaySpec {
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 整数延迟 D（samples，>=0）。
    pub delay_samples: u64,
    /// 显式声明支撑 taps；None=最小 D+1；Some(L) 必须 >=D+1 且 <=16384。
    pub taps: Option<usize>,
    /// 极性。
    pub polarity: Polarity,
    /// 作用方向。
    pub side: ApplySide,
    /// 显式声明的 delay_samples。
    pub declared_delay_samples: Option<f64>,
    /// 显式声明的 delay_ms。
    pub declared_delay_ms: Option<f64>,
    /// 目标总延迟（delay_removed/吸收口径）。
    pub target_delay_samples: Option<f64>,
}

impl IntegerDelaySpec {
    /// 最小支撑整数延迟（taps=D+1，Normal、输出行）。
    pub fn new(fs: f64, delay_samples: u64) -> Self {
        IntegerDelaySpec {
            fs,
            delay_samples,
            taps: None,
            polarity: Polarity::Normal,
            side: ApplySide::default_side(),
            declared_delay_samples: None,
            declared_delay_ms: None,
            target_delay_samples: None,
        }
    }

    /// D=0 恒等（taps=1）。
    pub fn identity(fs: f64) -> Self {
        Self::new(fs, 0)
    }

    /// 显式 taps（>=D+1）。
    pub fn with_taps(mut self, taps: usize) -> Self {
        self.taps = Some(taps);
        self
    }

    /// 极性。
    pub fn with_polarity(mut self, polarity: Polarity) -> Self {
        self.polarity = polarity;
        self
    }

    /// 作用方向。
    pub fn with_side(mut self, side: ApplySide) -> Self {
        self.side = side;
        self
    }

    /// 声明 delay_samples。
    pub fn with_declared_delay_samples(mut self, delay_samples: f64) -> Self {
        self.declared_delay_samples = Some(delay_samples);
        self
    }

    /// 声明 delay_ms。
    pub fn with_declared_delay_ms(mut self, delay_ms: f64) -> Self {
        self.declared_delay_ms = Some(delay_ms);
        self
    }

    /// 声明目标总延迟。
    pub fn with_target_delay_samples(mut self, target_delay_samples: f64) -> Self {
        self.target_delay_samples = Some(target_delay_samples);
        self
    }
}

/// 分数延迟 spec（§4.2：窗函数 sinc 或 Lagrange；声明 fractional_delay_d 与总 D）。
#[derive(Clone, Debug, PartialEq)]
pub struct FractionalDelaySpec {
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 声明总延迟 D（samples，>=0；分数部分 = D-floor(D)）。
    pub delay_samples: f64,
    /// 方法。
    pub method: FractionalMethod,
    /// 窗长 Lw（仅窗函数 sinc；默认 63，必须 >=63 且奇数）。
    pub window_taps: Option<usize>,
    /// 显式声明支撑 taps；None=按方法计算；Some(L) 必须等于计算支撑。
    pub taps: Option<usize>,
    /// 极性。
    pub polarity: Polarity,
    /// 作用方向。
    pub side: ApplySide,
    /// 验证频带 [lo,hi]；窗函数 sinc 必须为 §4.2 全带，Lagrange 可显式较窄。
    pub verify_band_hz: Option<[f64; 2]>,
    /// 显式声明的 delay_samples。
    pub declared_delay_samples: Option<f64>,
    /// 显式声明的 delay_ms。
    pub declared_delay_ms: Option<f64>,
    /// 目标总延迟。
    pub target_delay_samples: Option<f64>,
}

impl FractionalDelaySpec {
    /// 窗函数 sinc 分数延迟（默认 Kaiser beta=8.96、63-tap、Normal、输出行）。
    pub fn windowed_sinc(fs: f64, delay_samples: f64) -> Self {
        FractionalDelaySpec {
            fs,
            delay_samples,
            method: FractionalMethod::kaiser_sinc(),
            window_taps: None,
            taps: None,
            polarity: Polarity::Normal,
            side: ApplySide::default_side(),
            verify_band_hz: None,
            declared_delay_samples: None,
            declared_delay_ms: None,
            target_delay_samples: None,
        }
    }

    /// V11 参考：63-tap Kaiser beta 8.96、d=0.5、D=31.5。
    pub fn v11_reference(fs: f64) -> Self {
        Self::windowed_sinc(fs, V11_TOTAL_DELAY_SAMPLES)
    }

    /// Lagrange 分数延迟（D 必须落在 [order/2, order/2+1)）。
    pub fn lagrange(fs: f64, delay_samples: f64, order: usize) -> Self {
        let mut spec = Self::windowed_sinc(fs, delay_samples);
        spec.method = FractionalMethod::lagrange(order);
        spec.window_taps = None;
        spec
    }

    /// 方法。
    pub fn with_method(mut self, method: FractionalMethod) -> Self {
        self.method = method;
        self
    }

    /// 窗长（仅窗函数 sinc）。
    pub fn with_window_taps(mut self, window_taps: usize) -> Self {
        self.window_taps = Some(window_taps);
        self
    }

    /// 显式声明支撑。
    pub fn with_taps(mut self, taps: usize) -> Self {
        self.taps = Some(taps);
        self
    }

    /// 极性。
    pub fn with_polarity(mut self, polarity: Polarity) -> Self {
        self.polarity = polarity;
        self
    }

    /// 作用方向。
    pub fn with_side(mut self, side: ApplySide) -> Self {
        self.side = side;
        self
    }

    /// 显式验证频带（Lagrange 低带用法）。
    pub fn with_verify_band(mut self, band_hz: [f64; 2]) -> Self {
        self.verify_band_hz = Some(band_hz);
        self
    }

    /// 声明 delay_samples。
    pub fn with_declared_delay_samples(mut self, delay_samples: f64) -> Self {
        self.declared_delay_samples = Some(delay_samples);
        self
    }

    /// 声明 delay_ms。
    pub fn with_declared_delay_ms(mut self, delay_ms: f64) -> Self {
        self.declared_delay_ms = Some(delay_ms);
        self
    }

    /// 声明目标总延迟。
    pub fn with_target_delay_samples(mut self, target_delay_samples: f64) -> Self {
        self.target_delay_samples = Some(target_delay_samples);
        self
    }
}

/// 统一 spec（integer / fractional）。
#[derive(Clone, Debug, PartialEq)]
pub enum PhaseSpec {
    /// 整数延迟。
    Integer(IntegerDelaySpec),
    /// 分数延迟。
    Fractional(FractionalDelaySpec),
}

impl PhaseSpec {
    /// fs。
    pub fn fs(&self) -> f64 {
        match self {
            PhaseSpec::Integer(s) => s.fs,
            PhaseSpec::Fractional(s) => s.fs,
        }
    }

    /// 作用方向。
    pub fn side(&self) -> ApplySide {
        match self {
            PhaseSpec::Integer(s) => s.side,
            PhaseSpec::Fractional(s) => s.side,
        }
    }

    /// 极性。
    pub fn polarity(&self) -> Polarity {
        match self {
            PhaseSpec::Integer(s) => s.polarity,
            PhaseSpec::Fractional(s) => s.polarity,
        }
    }

    /// 声明总延迟 D。
    pub fn delay_samples(&self) -> f64 {
        match self {
            PhaseSpec::Integer(s) => s.delay_samples as f64,
            PhaseSpec::Fractional(s) => s.delay_samples,
        }
    }
}

// ---------------------------------------------------------------------------
// 基础数学（sinc / 窗 / 响应）
// ---------------------------------------------------------------------------

/// 归一化 sinc：sin(pi·x)/(pi·x)，x=0 时为 1。
pub fn sinc_normalized(x: f64) -> f64 {
    if x.abs() < 1e-15 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

/// 第一类零阶修正 Bessel I0（Kaiser 窗）。
pub fn bessel_i0(x: f64) -> f64 {
    let y = (x * x) / 4.0;
    let mut sum = 1.0_f64;
    let mut term = 1.0_f64;
    for k in 1..200 {
        term *= y / ((k as f64) * (k as f64));
        sum += term;
        if term.abs() < 1e-18 * sum.abs() {
            break;
        }
    }
    sum
}

/// Kaiser 窗值（taps>=2；k=0..taps-1）。
pub fn kaiser_window_values(taps: usize, beta: f64) -> Vec<f64> {
    if taps == 0 {
        return Vec::new();
    }
    if taps == 1 {
        return vec![1.0];
    }
    let denom = bessel_i0(beta);
    let n = taps - 1;
    (0..taps)
        .map(|k| {
            let t = 2.0 * (k as f64) / (n as f64) - 1.0;
            let arg = 1.0 - t * t;
            bessel_i0(beta * arg.max(0.0).sqrt()) / denom
        })
        .collect()
}

/// Hann 窗值（taps>=2）。
pub fn hann_window_values(taps: usize) -> Vec<f64> {
    if taps == 0 {
        return Vec::new();
    }
    if taps == 1 {
        return vec![1.0];
    }
    let n = (taps - 1) as f64;
    (0..taps)
        .map(|k| 0.5 - 0.5 * (2.0 * PI * (k as f64) / n).cos())
        .collect()
}

/// Blackman-Harris 4 项窗值（taps>=2）。
pub fn blackman_harris_window_values(taps: usize) -> Vec<f64> {
    if taps == 0 {
        return Vec::new();
    }
    if taps == 1 {
        return vec![1.0];
    }
    let n = (taps - 1) as f64;
    (0..taps)
        .map(|k| {
            let x = 2.0 * PI * (k as f64) / n;
            0.358_75 - 0.488_29 * x.cos() + 0.141_28 * (2.0 * x).cos() - 0.011_68 * (3.0 * x).cos()
        })
        .collect()
}

/// 相位 wrap 到 (−180,180]（deg）。
pub fn wrap_deg_180(deg: f64) -> f64 {
    if !deg.is_finite() {
        return deg;
    }
    let mut r = deg.rem_euclid(360.0);
    if r > 180.0 {
        r -= 360.0;
    }
    r
}

/// 线性卷积（零填充；等价 §7.1 的 zero padding 线性卷积，不做循环混叠）。
pub fn convolve_linear(a: &[f64], b: &[f64]) -> Vec<f64> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0.0_f64; a.len() + b.len() - 1];
    for (i, &x) in a.iter().enumerate() {
        if x == 0.0 {
            continue;
        }
        for (j, &y) in b.iter().enumerate() {
            out[i + j] += x * y;
        }
    }
    out
}

/// 规范测量网格：对数频率 [lo,hi] 上 points 点（§1.2：20 Hz–min(20k,0.499fs)）。
pub fn measurement_grid(lo_hz: f64, hi_hz: f64, points: usize) -> Vec<f64> {
    if points == 0 {
        return Vec::new();
    }
    if points == 1 || hi_hz <= lo_hz {
        return vec![lo_hz];
    }
    let l0 = lo_hz.ln();
    let l1 = hi_hz.ln();
    (0..points)
        .map(|i| {
            let t = (i as f64) / ((points - 1) as f64);
            (l0 + t * (l1 - l0)).exp()
        })
        .collect()
}

/// 单频复响应 + 解析群延迟（§1.2/§1.3）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseResponse {
    /// 频点（Hz）。
    pub f_hz: f64,
    /// 实部 Re H。
    pub re: f64,
    /// 虚部 Im H。
    pub im: f64,
    /// |H|。
    pub magnitude: f64,
    /// 20log10(max(|H|,1e-12))（dB）。
    pub magnitude_db: f64,
    /// wrap 到 (−180,180] 的相位（deg）。
    pub phase_deg: f64,
    /// 解析群延迟 GD=Re(B/H)（samples）。
    pub group_delay_samples: f64,
    /// GD 换算 ms。
    pub group_delay_ms: f64,
}

impl PhaseResponse {
    /// 幅度线性值。
    pub fn gain(&self) -> f64 {
        self.magnitude
    }
}

fn response_terms(coeffs: &[f64], fs: f64, f_hz: f64) -> (f64, f64, f64, f64) {
    let w = 2.0 * PI * f_hz / fs;
    let mut re = 0.0_f64;
    let mut im = 0.0_f64;
    let mut b_re = 0.0_f64;
    let mut b_im = 0.0_f64;
    for (n, &h) in coeffs.iter().enumerate() {
        if h == 0.0 {
            continue;
        }
        let (s, c) = (w * (n as f64)).sin_cos();
        re += h * c;
        im -= h * s;
        b_re += (n as f64) * h * c;
        b_im -= (n as f64) * h * s;
    }
    (re, im, b_re, b_im)
}

/// 任意系数的单频响应（直接 DTFT；独立于设计构造，不做 FFT）。
pub fn response_at(coeffs: &[f64], fs: f64, f_hz: f64) -> PhaseResponse {
    if coeffs.is_empty() || !fs.is_finite() || fs <= 0.0 || !f_hz.is_finite() {
        return PhaseResponse {
            f_hz,
            re: f64::NAN,
            im: f64::NAN,
            magnitude: f64::NAN,
            magnitude_db: f64::NAN,
            phase_deg: f64::NAN,
            group_delay_samples: f64::NAN,
            group_delay_ms: f64::NAN,
        };
    }
    let (re, im, b_re, b_im) = response_terms(coeffs, fs, f_hz);
    let magnitude = (re * re + im * im).sqrt();
    let magnitude_db = if magnitude <= MAGNITUDE_FLOOR {
        20.0 * MAGNITUDE_FLOOR.log10()
    } else {
        20.0 * magnitude.log10()
    };
    let phase_deg = wrap_deg_180(im.atan2(re).to_degrees());
    let power = re * re + im * im;
    let group_delay_samples = if power > 0.0 {
        (b_re * re + b_im * im) / power
    } else {
        f64::NAN
    };
    PhaseResponse {
        f_hz,
        re,
        im,
        magnitude,
        magnitude_db,
        phase_deg,
        group_delay_samples,
        group_delay_ms: group_delay_samples / fs * 1000.0,
    }
}

/// 单频 |H|dB。
pub fn magnitude_db_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).magnitude_db
}

/// 单频解析群延迟（samples）。
pub fn group_delay_at(coeffs: &[f64], fs: f64, f_hz: f64) -> f64 {
    response_at(coeffs, fs, f_hz).group_delay_samples
}

/// 有效支撑（§1.3：|h|>eps·max|h|；返回 (first,last,effective_taps)；全零返回 (0,0,0)）。
pub fn effective_support(coeffs: &[f64], eps: f64) -> (usize, usize, usize) {
    let max_abs = coeffs.iter().fold(0.0_f64, |m, &h| m.max(h.abs()));
    if max_abs == 0.0 || coeffs.is_empty() {
        return (0, 0, 0);
    }
    let threshold = eps * max_abs;
    let mut first = None;
    let mut last = 0_usize;
    for (n, &h) in coeffs.iter().enumerate() {
        if h.abs() > threshold {
            if first.is_none() {
                first = Some(n);
            }
            last = n;
        }
    }
    match first {
        Some(f) => (f, last, last - f + 1),
        None => (0, 0, 0),
    }
}

// ---------------------------------------------------------------------------
// 验证选项 / 统计结构
// ---------------------------------------------------------------------------

/// 验证选项（频带/网格/判据容差）。
#[derive(Clone, Debug, PartialEq)]
pub struct VerifyOptions {
    /// 验证频带 [lo,hi]（Hz）。
    pub band_hz: [f64; 2],
    /// 网格点数；0=自动 max(8192, 8·taps)。小于规范下限时 grid_meets_spec=false。
    pub grid_points: usize,
    /// 幅度判据容差（dB）。
    pub magnitude_tol_db: f64,
    /// 群延迟判据容差（samples）。
    pub group_delay_tol_samples: f64,
}

impl VerifyOptions {
    /// §4.2 全带默认：频带 [20, 0.9·fs/2]、自动网格、0.1 dB / 0.02 samples。
    pub fn spec(fs: f64) -> Self {
        VerifyOptions {
            band_hz: spec_band_hz(fs),
            grid_points: 0,
            magnitude_tol_db: FRACTIONAL_MAG_TOL_DB,
            group_delay_tol_samples: FRACTIONAL_GD_TOL_SAMPLES,
        }
    }

    /// 指定频带（判据容差同 spec）。
    pub fn band(fs: f64, band_hz: [f64; 2]) -> Self {
        let mut opts = Self::spec(fs);
        opts.band_hz = band_hz;
        opts
    }

    /// 设计自身声明的验证频带。
    pub fn for_design(design: &PhaseDesign) -> Self {
        Self::band(design.fs, design.verify_band_hz)
    }

    /// 指定网格点数（0=自动；小于规范下限会如实标 grid_meets_spec=false）。
    pub fn with_grid(mut self, grid_points: usize) -> Self {
        self.grid_points = grid_points;
        self
    }

    /// 指定判据容差。
    pub fn with_tolerances(mut self, magnitude_tol_db: f64, group_delay_tol_samples: f64) -> Self {
        self.magnitude_tol_db = magnitude_tol_db;
        self.group_delay_tol_samples = group_delay_tol_samples;
        self
    }
}

/// §4.2 规范全带 [20 Hz, 0.9·fs/2]。
pub fn spec_band_hz(fs: f64) -> [f64; 2] {
    [MEASUREMENT_LO_HZ, FRACTIONAL_BAND_HI_FRACTION * 0.5 * fs]
}

/// 频带统计（幅度与群延迟在验证带内的极值/偏差）。
#[derive(Clone, Debug, PartialEq)]
pub struct BandMetrics {
    /// 频带下界（Hz）。
    pub lo_hz: f64,
    /// 频带上界（Hz）。
    pub hi_hz: f64,
    /// 实际网格点数。
    pub points: usize,
    /// |H|dB 最小值。
    pub mag_min_db: f64,
    /// |H|dB 最大值。
    pub mag_max_db: f64,
    /// max | |H|dB − 0 dB |（判据 §4.2 用 0.1 dB）。
    pub mag_max_abs_dev_db: f64,
    /// GD 最小值（samples）。
    pub gd_min_samples: f64,
    /// GD 最大值（samples）。
    pub gd_max_samples: f64,
    /// GD 均值（samples）。
    pub gd_mean_samples: f64,
    /// max |GD − 声明 D|（判据 §4.2 用 0.02 samples）。
    pub gd_max_abs_dev_samples: f64,
    /// 1 kHz 处 GD（samples）。
    pub gd_at_1khz_samples: f64,
    /// 声明总延迟 D（samples）。
    pub declared_delay_samples: f64,
}

/// 振铃统计（§1.4：W=ceil(0.000333·fs)；主峰两侧 W 内不计）。
#[derive(Clone, Debug, PartialEq)]
pub struct RingingStats {
    /// 排除窗 W（samples）。
    pub exclusion_samples: usize,
    /// 主峰位置 p。
    pub peak_index: usize,
    /// pre_ring_db（p−W<0 或 p+W>=L 时 None=n/a）。
    pub pre_ring_db: Option<f64>,
    /// post_ring_db（同上 None=n/a）。
    pub post_ring_db: Option<f64>,
    /// pre_ring_energy_db。
    pub pre_ring_energy_db: Option<f64>,
    /// post_ring_energy_db。
    pub post_ring_energy_db: Option<f64>,
    /// 口径说明/n-a 原因。
    pub note: String,
}

/// Endpoint 联合类型（§2.4：禁止裸字符串/省略）。
#[derive(Clone, Debug, PartialEq)]
pub enum Endpoint {
    /// 可测端点。
    Measured {
        /// 频点（Hz）。
        freq_hz: f64,
        /// |H|dB。
        mag_db: f64,
        /// wrap 后相位（deg）。
        phase_deg: f64,
    },
    /// 在运行 fs 下不可测。
    NotApplicableAtFs {
        /// 原因。
        reason: String,
    },
}

impl Endpoint {
    /// status 字段字符串。
    pub fn status(&self) -> &'static str {
        match self {
            Endpoint::Measured { .. } => "measured",
            Endpoint::NotApplicableAtFs { .. } => "not_applicable_at_fs",
        }
    }

    /// 可测时返回 (freq_hz, mag_db, phase_deg)。
    pub fn measured(&self) -> Option<(f64, f64, f64)> {
        match self {
            Endpoint::Measured {
                freq_hz,
                mag_db,
                phase_deg,
            } => Some((*freq_hz, *mag_db, *phase_deg)),
            Endpoint::NotApplicableAtFs { .. } => None,
        }
    }
}

/// 20 Hz / 20 kHz 端点（§2.4）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseEndpoints {
    /// 20 Hz。
    pub f20: Endpoint,
    /// 20 kHz（fs 不可测时 not_applicable_at_fs）。
    pub f20k: Endpoint,
}

/// 参考归一化统计（§1.5：hz=0 DC，|measured−declared|<=0.01 dB）。
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceStats {
    /// 参考频点（Hz；0=DC）。
    pub hz: f64,
    /// 声明 gain（dB）。
    pub gain_db: f64,
    /// 实测 gain（dB）。
    pub measured_gain_db: f64,
    /// 是否在 ±0.01 dB 内。
    pub ok: bool,
}

/// 延迟声明审计（§2.2 规则 2 / §3.6 残差口径）。
#[derive(Clone, Debug, PartialEq)]
pub struct DelayAudit {
    /// 设计实际 D（samples）。
    pub design_delay_samples: f64,
    /// 声明 delay_samples。
    pub declared_delay_samples: Option<f64>,
    /// 声明 delay_ms。
    pub declared_delay_ms: Option<f64>,
    /// 目标总延迟。
    pub target_delay_samples: Option<f64>,
    /// 声明样本残差。
    pub residual_vs_declared_samples: Option<f64>,
    /// 目标残差。
    pub residual_vs_target_samples: Option<f64>,
    /// samples 是否一致（<=0.01）。
    pub samples_ok: bool,
    /// delay_ms 是否一致（<=1e-6）。
    pub delay_ms_ok: bool,
    /// target 是否一致（<=0.01）。
    pub target_ok: bool,
}

impl DelayAudit {
    /// 计算审计。
    pub fn compute(
        fs: f64,
        design_delay_samples: f64,
        declared_delay_samples: Option<f64>,
        declared_delay_ms: Option<f64>,
        target_delay_samples: Option<f64>,
    ) -> Self {
        let residual_vs_declared_samples = declared_delay_samples.map(|d| d - design_delay_samples);
        let residual_vs_target_samples = target_delay_samples.map(|d| d - design_delay_samples);
        let samples_ok = residual_vs_declared_samples
            .map(|r| r.abs() <= DELAY_RESIDUAL_TOLERANCE)
            .unwrap_or(true);
        let delay_ms_ok = declared_delay_ms
            .map(|ms| (ms - design_delay_samples / fs * 1000.0).abs() <= DELAY_MS_TOLERANCE)
            .unwrap_or(true);
        let target_ok = residual_vs_target_samples
            .map(|r| r.abs() <= DELAY_RESIDUAL_TOLERANCE)
            .unwrap_or(true);
        DelayAudit {
            design_delay_samples,
            declared_delay_samples,
            declared_delay_ms,
            target_delay_samples,
            residual_vs_declared_samples,
            residual_vs_target_samples,
            samples_ok,
            delay_ms_ok,
            target_ok,
        }
    }
}

/// taps/内存/延迟预算事实（§7.2；准入归 T56/T55）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapsProfile {
    /// 常规 profile：<=4096 taps。
    Regular,
    /// extended profile：4097..=16384。
    Extended,
    /// 超过 16384：只能其它离线工具。
    OfflineOnly,
}

impl TapsProfile {
    /// schema 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            TapsProfile::Regular => "regular",
            TapsProfile::Extended => "extended",
            TapsProfile::OfflineOnly => "offline_only",
        }
    }
}

/// 预算事实（§7.2；只报事实，不宣称整链预算通过）。
#[derive(Clone, Debug, PartialEq)]
pub struct BudgetFacts {
    /// 支撑 taps。
    pub taps: usize,
    /// profile 分类。
    pub profile: TapsProfile,
    /// 该 profile 每路由 taps 上限。
    pub route_taps_cap: usize,
    /// 每路由 f32 内存 bytes。
    pub ram_bytes_f32: usize,
    /// 声明总延迟 D（samples）。
    pub delay_samples: f64,
    /// FIR 延迟（ms）。
    pub delay_ms: f64,
    /// 是否 <=4096。
    pub fits_regular_profile: bool,
    /// 是否 <=16384。
    pub fits_extended_profile: bool,
    /// 15/25/40 ms 附加延迟预算。
    pub delay_budget_ms: [f64; 3],
    /// 各预算是否覆盖 FIR delay_ms（整链还含 block/SRC/I-O，判定归 T55/T56）。
    pub fits_delay_budget_ms: [bool; 3],
    /// 口径说明。
    pub note: String,
}

/// 检查项（bools；passed() 为规范性判据）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseChecks {
    /// taps == len(h)。
    pub taps_len_ok: bool,
    /// 全部有限且 max|h|>0。
    pub finite_ok: bool,
    /// delay_ms == D/fs*1000。
    pub delay_report_ok: bool,
    /// 因果（support/D>=0、支撑从非负索引起）。
    pub causal_ok: bool,
    /// 声明/target 延迟审计通过。
    pub delay_declared_ok: bool,
    /// 极性符号一致（DC 增益符号 = polarity.sign()）。
    pub polarity_sign_ok: bool,
    /// 验证带内 |H|dB 与 0 dB 差 <= tol。
    pub magnitude_band_ok: bool,
    /// 验证带内 GD 与声明 D 差 <= tol。
    pub group_delay_band_ok: bool,
    /// 参考归一化 ±0.01 dB。
    pub reference_ok: bool,
    /// 设计是否把 §4.2 全带判据作为强制要求。
    pub spec_band_required: bool,
    /// 验证带是否就是 §4.2 全带且判据达标（Lagrange 窄带为 false）。
    pub spec_band_compliant: bool,
}

impl PhaseChecks {
    /// 声明带内规范性判据全通过（不含“全带合规”附加声明）。
    pub fn passed(&self) -> bool {
        self.taps_len_ok
            && self.finite_ok
            && self.delay_report_ok
            && self.causal_ok
            && self.delay_declared_ok
            && self.polarity_sign_ok
            && self.magnitude_band_ok
            && self.group_delay_band_ok
            && self.reference_ok
    }
}

/// 验证结果。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseVerification {
    /// 设计支撑。
    pub design_taps: usize,
    /// 频带统计。
    pub band: BandMetrics,
    /// 检查项。
    pub checks: PhaseChecks,
    /// 20 Hz/20 kHz 端点。
    pub endpoints: PhaseEndpoints,
    /// 参考归一化。
    pub reference: ReferenceStats,
    /// 振铃。
    pub ringing: RingingStats,
    /// 1 kHz 群延迟（samples）。
    pub group_delay_at_1khz_samples: f64,
    /// 实际网格点数。
    pub grid_points: usize,
    /// 网格是否达到 §1.2 N>=max(8192,8L)。
    pub grid_meets_spec: bool,
    /// §4.2 全带。
    pub spec_band_hz: [f64; 2],
    /// 警告。
    pub warnings: Vec<String>,
}

impl PhaseVerification {
    /// 声明带内判据通过。
    pub fn passed(&self) -> bool {
        self.checks.passed()
    }

    /// 同时满足 §4.2 全带合规（窗函数 sinc 正常路径）。
    pub fn fully_spec_compliant(&self) -> bool {
        self.passed() && self.checks.spec_band_compliant
    }

    /// 证据摘要行。
    pub fn summary_lines(&self) -> Vec<String> {
        let b = &self.band;
        let mut lines = vec![
            format!(
                "band [{:.1},{:.1}] Hz, grid {} points (meets_spec={})",
                b.lo_hz, b.hi_hz, b.points, self.grid_meets_spec
            ),
            format!(
                "mag dev max {:.6} dB; GD dev max {:.6} samples (D={:.6}, @1k={:.6})",
                b.mag_max_abs_dev_db,
                b.gd_max_abs_dev_samples,
                b.declared_delay_samples,
                b.gd_at_1khz_samples
            ),
            format!(
                "checks passed={} spec_band_compliant={}",
                self.passed(),
                self.checks.spec_band_compliant
            ),
        ];
        for w in &self.warnings {
            lines.push(format!("warning: {w}"));
        }
        lines
    }
}

// ---------------------------------------------------------------------------
// PhaseDesign
// ---------------------------------------------------------------------------

/// phase/delay/polarity 设计产物（系数 + 延迟/极性/方向/验证元数据）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseDesign {
    /// 运行采样率（Hz）。
    pub fs: f64,
    /// 延迟种类。
    pub kind: DelayKind,
    /// 声明支撑 L=len(coefficients)。
    pub taps: usize,
    /// 因果 FIR 系数（已含极性符号；f64 全精度）。
    pub coefficients: Vec<f64>,
    /// 声明总延迟 D（samples）。
    pub delay_samples: f64,
    /// delay_ms = D/fs*1000。
    pub delay_ms: f64,
    /// 极性。
    pub polarity: Polarity,
    /// 作用方向。
    pub side: ApplySide,
    /// 窗长（仅窗函数 sinc）。
    pub window_taps: Option<usize>,
    /// 支撑在因果轴上的起点（窗函数 sinc 的 shift；整数延迟为 0）。
    pub support_start: usize,
    /// 设计自身声明的验证频带（§4.2 全带；Lagrange 可窄带）。
    pub verify_band_hz: [f64; 2],
    /// 是否把 §4.2 全带判据作为强制要求（窗函数 sinc=true，Lagrange=false）。
    pub spec_band_required: bool,
    /// 显式声明 delay_samples。
    pub declared_delay_samples: Option<f64>,
    /// 显式声明 delay_ms。
    pub declared_delay_ms: Option<f64>,
    /// 目标总延迟。
    pub target_delay_samples: Option<f64>,
}

impl PhaseDesign {
    /// 是否整数延迟。
    pub fn is_integer(&self) -> bool {
        self.kind.is_integer()
    }

    /// 分数延迟 d（整数延迟为 None）。
    pub fn fractional_delay_d(&self) -> Option<f64> {
        self.kind.fractional_delay_d()
    }

    /// 分数延迟方法（整数延迟为 None）。
    pub fn method(&self) -> Option<FractionalMethod> {
        self.kind.method()
    }

    /// 窗长（仅窗函数 sinc）。
    pub fn window_taps(&self) -> Option<usize> {
        self.window_taps
    }

    /// 有效支撑 first。
    pub fn onset_samples(&self) -> usize {
        effective_support(&self.coefficients, SUPPORT_EPS).0
    }

    /// onset latency（ms；§1.3：first/fs*1000，与 delay_ms 分开报告）。
    pub fn onset_ms(&self) -> f64 {
        self.onset_samples() as f64 / self.fs * 1000.0
    }

    /// 有效支撑 (first,last,effective_taps)。
    pub fn effective_support(&self) -> (usize, usize, usize) {
        effective_support(&self.coefficients, SUPPORT_EPS)
    }

    /// 尾部零填充数量（整数延迟显式更长 taps 时非零）。
    pub fn trailing_zeros(&self) -> usize {
        let (_, last, _) = self.effective_support();
        self.coefficients.len().saturating_sub(last + 1)
    }

    /// 单频响应。
    pub fn response_at(&self, f_hz: f64) -> PhaseResponse {
        response_at(&self.coefficients, self.fs, f_hz)
    }

    /// 单频解析 GD。
    pub fn group_delay_at(&self, f_hz: f64) -> f64 {
        self.response_at(f_hz).group_delay_samples
    }

    /// 单频 |H|dB。
    pub fn magnitude_db_at(&self, f_hz: f64) -> f64 {
        self.response_at(f_hz).magnitude_db
    }

    /// 参考归一化统计（hz=0 DC）。
    pub fn reference_stats(&self) -> ReferenceStats {
        let sum: f64 = self.coefficients.iter().sum();
        let measured_gain_db = if sum.abs() <= MAGNITUDE_FLOOR {
            20.0 * MAGNITUDE_FLOOR.log10()
        } else {
            20.0 * sum.abs().log10()
        };
        ReferenceStats {
            hz: 0.0,
            gain_db: 0.0,
            measured_gain_db,
            ok: (measured_gain_db - 0.0).abs() <= REFERENCE_TOLERANCE_DB,
        }
    }

    /// 20 Hz / 20 kHz 端点（§2.4）。
    pub fn endpoints(&self) -> PhaseEndpoints {
        let f20 = Endpoint::Measured {
            freq_hz: MEASUREMENT_LO_HZ,
            mag_db: self.magnitude_db_at(MEASUREMENT_LO_HZ),
            phase_deg: self.response_at(MEASUREMENT_LO_HZ).phase_deg,
        };
        let hi_measurable = NYQUIST_GUARD_FRACTION * self.fs >= ENDPOINT_HI_HZ;
        let f20k = if hi_measurable {
            Endpoint::Measured {
                freq_hz: ENDPOINT_HI_HZ,
                mag_db: self.magnitude_db_at(ENDPOINT_HI_HZ),
                phase_deg: self.response_at(ENDPOINT_HI_HZ).phase_deg,
            }
        } else {
            Endpoint::NotApplicableAtFs {
                reason: format!(
                    "0.499·fs={:.1} Hz < 20000 Hz（fs={:.0}）",
                    NYQUIST_GUARD_FRACTION * self.fs,
                    self.fs
                ),
            }
        };
        PhaseEndpoints { f20, f20k }
    }

    /// 振铃统计（§1.4 口径）。
    pub fn ringing(&self) -> RingingStats {
        ringing_stats(&self.coefficients, self.fs)
    }

    /// 预算事实（§7.2；准入归 T56）。
    pub fn budget_facts(&self) -> BudgetFacts {
        let taps = self.taps;
        let profile = if taps <= REGULAR_PROFILE_TAPS_CAP {
            TapsProfile::Regular
        } else if taps <= EXTENDED_PROFILE_TAPS_CAP {
            TapsProfile::Extended
        } else {
            TapsProfile::OfflineOnly
        };
        let route_taps_cap = match profile {
            TapsProfile::Regular => REGULAR_PROFILE_TAPS_CAP,
            TapsProfile::Extended => EXTENDED_PROFILE_TAPS_CAP,
            TapsProfile::OfflineOnly => EXTENDED_PROFILE_TAPS_CAP,
        };
        let mut fits = [false; 3];
        for (i, budget) in DELAY_BUDGET_MS.iter().enumerate() {
            fits[i] = self.delay_ms <= *budget;
        }
        BudgetFacts {
            taps,
            profile,
            route_taps_cap,
            ram_bytes_f32: F32_BYTES * taps,
            delay_samples: self.delay_samples,
            delay_ms: self.delay_ms,
            fits_regular_profile: taps <= REGULAR_PROFILE_TAPS_CAP,
            fits_extended_profile: taps <= EXTENDED_PROFILE_TAPS_CAP,
            delay_budget_ms: DELAY_BUDGET_MS,
            fits_delay_budget_ms: fits,
            note: "只报 FIR delay_ms 与 taps 事实；15/25/40 ms 是整链附加预算（含 block/SRC/I-O），profile 准入与整链判定归 T55/T56".to_string(),
        }
    }

    /// 延迟声明审计（使用设计内声明的 declared/target 字段）。
    pub fn delay_audit(&self) -> DelayAudit {
        DelayAudit::compute(
            self.fs,
            self.delay_samples,
            self.declared_delay_samples,
            self.declared_delay_ms,
            self.target_delay_samples,
        )
    }

    /// 与校准 invert 的 effective 极性（XOR；运行时不得双乘）。
    pub fn effective_polarity(&self, calibration: Polarity) -> Polarity {
        self.polarity.xor(calibration)
    }

    /// 默认按设计声明频带验证（窗函数 sinc 为 §4.2 全带）。
    pub fn verify(&self) -> PhaseVerification {
        self.verify_with(&VerifyOptions::for_design(self))
    }

    /// 按指定选项验证（直接 DTFT + 解析 GD；独立于设计构造）。
    pub fn verify_with(&self, opts: &VerifyOptions) -> PhaseVerification {
        let fs = self.fs;
        let band = opts.band_hz;
        let required_points = self
            .taps
            .saturating_mul(MEASUREMENT_GRID_FACTOR)
            .max(MIN_MEASUREMENT_GRID_POINTS);
        let points = if opts.grid_points == 0 {
            required_points
        } else {
            opts.grid_points
        };
        let grid_meets_spec = points >= required_points;
        let mut warnings = Vec::new();
        if !grid_meets_spec {
            warnings.push(format!(
                "测量网格 {points} 点低于 §1.2 规范下限 max(8192,8L)={required_points}"
            ));
        }
        let spec_band = spec_band_hz(fs);
        let band_is_spec =
            (band[0] - spec_band[0]).abs() <= 1e-9 && (band[1] - spec_band[1]).abs() <= 1e-9;
        let band_in_spec = band[0].is_finite()
            && band[1].is_finite()
            && band[0] >= MEASUREMENT_LO_HZ - 1e-9
            && band[1] <= spec_band[1] + 1e-9
            && band[1] > band[0];
        if !band_in_spec {
            warnings.push(format!(
                "验证频带 [{},{}] 越出 §4.2 规范 [20, 0.9·fs/2={:.3}]，结论不得当作 §4.2 合规",
                band[0], band[1], spec_band[1]
            ));
        }

        let grid = measurement_grid(band[0], band[1], points);
        let mut mag_min = f64::INFINITY;
        let mut mag_max = f64::NEG_INFINITY;
        let mut mag_dev = 0.0_f64;
        let mut gd_min = f64::INFINITY;
        let mut gd_max = f64::NEG_INFINITY;
        let mut gd_sum = 0.0_f64;
        let mut gd_dev = 0.0_f64;
        for &f in &grid {
            let r = response_at(&self.coefficients, fs, f);
            mag_min = mag_min.min(r.magnitude_db);
            mag_max = mag_max.max(r.magnitude_db);
            mag_dev = mag_dev.max(r.magnitude_db.abs());
            gd_min = gd_min.min(r.group_delay_samples);
            gd_max = gd_max.max(r.group_delay_samples);
            gd_sum += r.group_delay_samples;
            gd_dev = gd_dev.max((r.group_delay_samples - self.delay_samples).abs());
        }
        let n = grid.len().max(1) as f64;
        let gd_mean = gd_sum / n;
        let gd_at_1khz = self.group_delay_at(1000.0);
        let magnitude_band_ok = mag_dev <= opts.magnitude_tol_db;
        let group_delay_band_ok = gd_dev <= opts.group_delay_tol_samples;

        let endpoints = self.endpoints();
        let reference = self.reference_stats();
        let ringing = self.ringing();
        let audit = self.delay_audit();
        let delay_report_ok =
            (self.delay_ms - self.delay_samples / fs * 1000.0).abs() <= DELAY_MS_TOLERANCE;
        let taps_len_ok = self.coefficients.len() == self.taps;
        let max_abs = self
            .coefficients
            .iter()
            .fold(0.0_f64, |m, &h| m.max(h.abs()));
        let finite_ok = self.coefficients.iter().all(|h| h.is_finite()) && max_abs > 0.0;
        let causal_ok = self.delay_samples.is_finite()
            && self.delay_samples >= 0.0
            && self.support_start < self.coefficients.len();
        let dc: f64 = self.coefficients.iter().sum();
        let polarity_sign_ok = dc * self.polarity.sign() > 0.0;
        let spec_band_compliant = band_is_spec && magnitude_band_ok && group_delay_band_ok;
        if !self.spec_band_required && !spec_band_compliant {
            warnings.push(
                "Lagrange 低带方法：只在声明验证频带内通过判据，不是 §4.2 全带合规（spec_band_compliant=false）".to_string(),
            );
        }
        if band_is_spec && self.spec_band_required && !spec_band_compliant {
            warnings.push("§4.2 全带分数延迟判据未通过（INFEASIBLE；不得静默放宽）".to_string());
        }
        let checks = PhaseChecks {
            taps_len_ok,
            finite_ok,
            delay_report_ok,
            causal_ok,
            delay_declared_ok: audit.samples_ok && audit.delay_ms_ok && audit.target_ok,
            polarity_sign_ok,
            magnitude_band_ok,
            group_delay_band_ok,
            reference_ok: reference.ok,
            spec_band_required: self.spec_band_required,
            spec_band_compliant,
        };
        PhaseVerification {
            design_taps: self.taps,
            band: BandMetrics {
                lo_hz: band[0],
                hi_hz: band[1],
                points,
                mag_min_db: mag_min,
                mag_max_db: mag_max,
                mag_max_abs_dev_db: mag_dev,
                gd_min_samples: gd_min,
                gd_max_samples: gd_max,
                gd_mean_samples: gd_mean,
                gd_max_abs_dev_samples: gd_dev,
                gd_at_1khz_samples: gd_at_1khz,
                declared_delay_samples: self.delay_samples,
            },
            checks,
            endpoints,
            reference,
            ringing,
            group_delay_at_1khz_samples: gd_at_1khz,
            grid_points: points,
            grid_meets_spec,
            spec_band_hz: spec_band,
            warnings,
        }
    }

    /// 规范化报告文本（§4.2：必须写清作用侧、D/delay_ms、极性 XOR、因果）。
    pub fn report_text(&self) -> String {
        let method_label = self.method().map(|m| m.label()).unwrap_or("none");
        let window_label = self
            .method()
            .and_then(|m| m.window())
            .map(|w| w.label())
            .unwrap_or("none");
        let beta = self
            .method()
            .and_then(|m| m.window())
            .and_then(|w| w.beta());
        let mut lines = Vec::new();
        lines.push(format!("tool: {} {}", TOOL_KIND, TOOL_VERSION));
        lines.push(format!("family_note: {FAMILY_NOTE}"));
        lines.push(format!(
            "kind: {} taps: {} support_start: {}",
            self.kind.as_str(),
            self.taps,
            self.support_start
        ));
        lines.push(format!(
            "delay_samples: {:.6} delay_ms: {:.6}",
            self.delay_samples, self.delay_ms
        ));
        if let Some(d) = self.fractional_delay_d() {
            lines.push(format!(
                "fractional_delay_d: {:.6} method: {} window: {} window_taps: {} beta: {}",
                d,
                method_label,
                window_label,
                self.window_taps.unwrap_or(0),
                beta.map(|b| format!("{b:.2}"))
                    .unwrap_or_else(|| "n/a".to_string())
            ));
        }
        let (first, last, effective) = self.effective_support();
        lines.push(format!(
            "effective_support: first={} last={} effective_taps={} onset_ms={:.6} trailing_zeros={}",
            first,
            last,
            effective,
            self.onset_ms(),
            self.trailing_zeros()
        ));
        lines.push(format!(
            "side: {} ({})",
            self.side.as_str(),
            self.side.scope()
        ));
        lines.push(format!("direction: {}", self.side.statement()));
        lines.push(format!("commutativity: {}", COMMUTATIVITY_STATEMENT));
        lines.push(format!(
            "polarity: {} (effective with calibration invert = XOR)",
            self.polarity.as_str()
        ));
        lines.push(format!("calibration_xor: {}", CALIBRATION_XOR_STATEMENT));
        lines.push(format!("causality: {}", CAUSALITY_STATEMENT));
        lines.push(format!("chain: {}", CANONICAL_CHAIN_STATEMENT));
        lines.join("\n")
    }

    /// 关键摘要行（测试/证据日志）。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .report_text()
            .lines()
            .map(|line| line.to_string())
            .collect();
        let v = self.verify();
        lines.extend(v.summary_lines());
        let budget = self.budget_facts();
        lines.push(format!(
            "budget: profile={} taps={} ram_bytes_f32={} delay_ms={:.6} fits_delay_budget_ms={:?}",
            budget.profile.as_str(),
            budget.taps,
            budget.ram_bytes_f32,
            budget.delay_ms,
            budget.fits_delay_budget_ms
        ));
        lines
    }

    /// manifest 声明（T55 消费；family 字段未列此工具，见 FAMILY_NOTE）。
    pub fn manifest_declaration(&self) -> PhaseManifestDeclaration {
        let method = self.method();
        let window = method.and_then(|m| m.window());
        PhaseManifestDeclaration {
            tool: TOOL_KIND,
            version: TOOL_VERSION,
            kind: self.kind.as_str(),
            taps: self.taps,
            delay_samples: self.delay_samples,
            delay_ms: self.delay_ms,
            fractional_delay_d: self.fractional_delay_d(),
            method: method.map(|m| m.label()).unwrap_or("none"),
            window: window.map(|w| w.label()).unwrap_or("none"),
            window_taps: self.window_taps,
            window_beta: window.and_then(|w| w.beta()),
            polarity: self.polarity,
            side: self.side,
            apply_scope: self.side.scope(),
            spec_band_hz: self.verify_band_hz,
            spec_band_required: self.spec_band_required,
            family_note: FAMILY_NOTE,
        }
    }
}

/// manifest/报告声明（不扩 tspi-fir/v1 顶层 schema；T55 放入 design/report 段）。
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseManifestDeclaration {
    /// 工具标识。
    pub tool: &'static str,
    /// 工具版本。
    pub version: &'static str,
    /// integer_delay / fractional_delay。
    pub kind: &'static str,
    /// 支撑 taps。
    pub taps: usize,
    /// 总延迟 D（samples）。
    pub delay_samples: f64,
    /// delay_ms。
    pub delay_ms: f64,
    /// 分数延迟 d。
    pub fractional_delay_d: Option<f64>,
    /// 方法。
    pub method: &'static str,
    /// 窗。
    pub window: &'static str,
    /// 窗长。
    pub window_taps: Option<usize>,
    /// Kaiser beta。
    pub window_beta: Option<f64>,
    /// 极性。
    pub polarity: Polarity,
    /// 作用方向。
    pub side: ApplySide,
    /// 作用范围说明。
    pub apply_scope: &'static str,
    /// 声明验证频带。
    pub spec_band_hz: [f64; 2],
    /// 是否 §4.2 全带强制。
    pub spec_band_required: bool,
    /// 七家族登记说明。
    pub family_note: &'static str,
}

// ---------------------------------------------------------------------------
// 设计入口（integer / fractional）
// ---------------------------------------------------------------------------

fn validate_fs(fs: f64) -> Result<(), PhaseError> {
    if !fs.is_finite() || fs < FS_ENGINEERING_MIN_HZ || fs > FS_ENGINEERING_MAX_HZ {
        return Err(PhaseError::band(format!(
            "fs 必须是 [{},{}] Hz 内的有限值，实际 {}",
            FS_ENGINEERING_MIN_HZ, FS_ENGINEERING_MAX_HZ, fs
        )));
    }
    Ok(())
}

fn validate_declared_delay(
    fs: f64,
    design_delay_samples: f64,
    declared_delay_samples: Option<f64>,
    declared_delay_ms: Option<f64>,
    target_delay_samples: Option<f64>,
) -> Result<DelayAudit, PhaseError> {
    let audit = DelayAudit::compute(
        fs,
        design_delay_samples,
        declared_delay_samples,
        declared_delay_ms,
        target_delay_samples,
    );
    if !audit.samples_ok {
        return Err(PhaseError::delay(format!(
            "声明 delay_samples={:?} 与设计 D={:.6} 残差 {:?} 超过 {:.6} sample",
            declared_delay_samples,
            design_delay_samples,
            audit.residual_vs_declared_samples,
            DELAY_RESIDUAL_TOLERANCE
        )));
    }
    if !audit.delay_ms_ok {
        return Err(PhaseError::delay(format!(
            "声明 delay_ms={:?} 与 D/fs*1000={:.9} 不一致（容差 {:.1e} ms）",
            declared_delay_ms,
            design_delay_samples / fs * 1000.0,
            DELAY_MS_TOLERANCE
        )));
    }
    if !audit.target_ok {
        return Err(PhaseError::delay(format!(
            "target_delay_samples={:?} 与设计 D={:.6} 残差 {:?} 超过 {:.6} sample（delay_removed 不猜测）",
            target_delay_samples,
            design_delay_samples,
            audit.residual_vs_target_samples,
            DELAY_RESIDUAL_TOLERANCE
        )));
    }
    Ok(audit)
}

/// 设计整数延迟（§4.2：h[n]=delta[n−D]）。
pub fn design_integer(spec: &IntegerDelaySpec) -> Result<PhaseDesign, PhaseError> {
    if !spec.fs.is_finite() || spec.fs <= 0.0 {
        return Err(PhaseError::numeric(format!("fs 非法：{}", spec.fs)));
    }
    validate_fs(spec.fs)?;
    let delay_usize = spec.delay_samples as usize;
    let min_taps = delay_usize.saturating_add(1);
    if min_taps > MAX_TAPS {
        return Err(PhaseError::budget(format!(
            "整数延迟 D={} 的最小支撑 D+1={} 超过设计上限 {} taps（更大只能其它离线工具）",
            spec.delay_samples, min_taps, MAX_TAPS
        )));
    }
    let taps = match spec.taps {
        Some(taps) => taps,
        None => min_taps,
    };
    if taps < min_taps {
        return Err(PhaseError::taps(format!(
            "taps={} < D+1={}：因果整数延迟不能缩短支撑到 D 之前",
            taps, min_taps
        )));
    }
    if taps > MAX_TAPS {
        return Err(PhaseError::budget(format!(
            "taps={} 超过设计上限 {}",
            taps, MAX_TAPS
        )));
    }
    let delay_samples = spec.delay_samples as f64;
    let delay_ms = delay_samples / spec.fs * 1000.0;
    validate_declared_delay(
        spec.fs,
        delay_samples,
        spec.declared_delay_samples,
        spec.declared_delay_ms,
        spec.target_delay_samples,
    )?;
    let mut coefficients = vec![0.0_f64; taps];
    coefficients[delay_usize] = spec.polarity.sign();
    Ok(PhaseDesign {
        fs: spec.fs,
        kind: DelayKind::Integer {
            delay_samples: spec.delay_samples,
        },
        taps,
        coefficients,
        delay_samples,
        delay_ms,
        polarity: spec.polarity,
        side: spec.side,
        window_taps: None,
        support_start: 0,
        verify_band_hz: spec_band_hz(spec.fs),
        spec_band_required: true,
        declared_delay_samples: spec.declared_delay_samples,
        declared_delay_ms: spec.declared_delay_ms,
        target_delay_samples: spec.target_delay_samples,
    })
}

/// 验证频带是否合法（§4.2：lo>=20、hi<=0.9·fs/2、hi>lo）。
fn validate_verify_band(fs: f64, band_hz: [f64; 2]) -> Result<(), PhaseError> {
    let [lo, hi] = band_hz;
    let hi_max = FRACTIONAL_BAND_HI_FRACTION * 0.5 * fs;
    if !lo.is_finite()
        || !hi.is_finite()
        || lo < MEASUREMENT_LO_HZ
        || hi > hi_max + 1e-9
        || hi <= lo
    {
        return Err(PhaseError::band(format!(
            "验证频带 [{},{}] 非法：要求 {}<=lo<hi<=0.9·fs/2={:.3}",
            lo, hi, MEASUREMENT_LO_HZ, hi_max
        )));
    }
    Ok(())
}

/// 设计分数延迟（§4.2：窗函数 sinc 或 Lagrange）。
pub fn design_fractional(spec: &FractionalDelaySpec) -> Result<PhaseDesign, PhaseError> {
    if !spec.fs.is_finite() || spec.fs <= 0.0 {
        return Err(PhaseError::numeric(format!("fs 非法：{}", spec.fs)));
    }
    validate_fs(spec.fs)?;
    if !spec.delay_samples.is_finite() {
        return Err(PhaseError::numeric(format!(
            "delay_samples 非有限：{}",
            spec.delay_samples
        )));
    }
    if spec.delay_samples < 0.0 {
        return Err(PhaseError::noncausal(format!(
            "总延迟 D={} < 0：负/提前延迟必须由 bulk target delay 吸收",
            spec.delay_samples
        )));
    }
    match spec.method {
        FractionalMethod::WindowedSinc { window } => design_windowed_sinc(spec, window),
        FractionalMethod::Lagrange { order } => design_lagrange(spec, order),
    }
}

fn design_windowed_sinc(
    spec: &FractionalDelaySpec,
    window: FractionalWindow,
) -> Result<PhaseDesign, PhaseError> {
    if let FractionalWindow::Kaiser { beta } = window {
        if !beta.is_finite() || beta <= 0.0 {
            return Err(PhaseError::numeric(format!(
                "Kaiser beta 必须为有限正数，实际 {}",
                beta
            )));
        }
    }
    let window_taps = spec.window_taps.unwrap_or(MIN_WINDOW_TAPS);
    if window_taps < MIN_WINDOW_TAPS {
        return Err(PhaseError::taps(format!(
            "窗函数 sinc 的窗长 {} < §4.2 最小 {} taps",
            window_taps, MIN_WINDOW_TAPS
        )));
    }
    if window_taps > MAX_TAPS {
        return Err(PhaseError::budget(format!(
            "窗长 {} 超过设计上限 {}",
            window_taps, MAX_TAPS
        )));
    }
    if window_taps % 2 == 0 {
        return Err(PhaseError::taps(format!(
            "窗长 {} 必须为奇数才能对齐整数中心 c=(Lw−1)/2；偶数请显式改奇数窗长",
            window_taps
        )));
    }
    let center = (window_taps - 1) / 2;
    let floor_d = spec.delay_samples.floor();
    let frac = spec.delay_samples - floor_d;
    if floor_d < center as f64 {
        return Err(PhaseError::noncausal(format!(
            "总延迟 D={} < 窗中心 c={}：窗函数 sinc 会需要 n<0 的抽头（不可因果），必须由 bulk D 吸收或减小窗长",
            spec.delay_samples, center
        )));
    }
    let shift = (floor_d as usize) - center;
    let support = shift
        .checked_add(window_taps)
        .ok_or_else(|| PhaseError::budget("支撑长度溢出 usize".to_string()))?;
    if support > MAX_TAPS {
        return Err(PhaseError::budget(format!(
            "支撑 L={}=floor(D)−c+Lw 超过设计上限 {} taps",
            support, MAX_TAPS
        )));
    }
    if let Some(taps) = spec.taps {
        if taps != support {
            return Err(PhaseError::taps(format!(
                "显式 taps={} 与窗函数 sinc 计算支撑 L={} 不一致（floor(D)−c+Lw）",
                taps, support
            )));
        }
    }
    let mut band = spec_band_hz(spec.fs);
    if let Some(b) = spec.verify_band_hz {
        if (b[0] - band[0]).abs() > 1e-9 || (b[1] - band[1]).abs() > 1e-9 {
            return Err(PhaseError::band(format!(
                "窗函数 sinc 必须按 §4.2 全带 [{:.3},{:.3}] 判据，不能声明窄带 {:?} 放宽",
                band[0], band[1], b
            )));
        }
        band = b;
    }
    validate_verify_band(spec.fs, band)?;
    let delay_samples = spec.delay_samples;
    let delay_ms = delay_samples / spec.fs * 1000.0;
    validate_declared_delay(
        spec.fs,
        delay_samples,
        spec.declared_delay_samples,
        spec.declared_delay_ms,
        spec.target_delay_samples,
    )?;
    let window_values = window.values(window_taps);
    let mut coefficients = vec![0.0_f64; support];
    for (k, &w) in window_values.iter().enumerate() {
        let x = (k as f64) - (center as f64) - frac;
        coefficients[shift + k] = spec.polarity.sign() * sinc_normalized(x) * w;
    }
    Ok(PhaseDesign {
        fs: spec.fs,
        kind: DelayKind::Fractional {
            integer_delay_samples: floor_d as u64,
            fractional_delay_d: frac,
            method: FractionalMethod::WindowedSinc { window },
        },
        taps: support,
        coefficients,
        delay_samples,
        delay_ms,
        polarity: spec.polarity,
        side: spec.side,
        window_taps: Some(window_taps),
        support_start: shift,
        verify_band_hz: band,
        spec_band_required: true,
        declared_delay_samples: spec.declared_delay_samples,
        declared_delay_ms: spec.declared_delay_ms,
        target_delay_samples: spec.target_delay_samples,
    })
}

fn design_lagrange(spec: &FractionalDelaySpec, order: usize) -> Result<PhaseDesign, PhaseError> {
    if order == 0 || order > MAX_LAGRANGE_ORDER {
        return Err(PhaseError::taps(format!(
            "Lagrange order={} 必须 ∈ [1,{}]（taps=order+1）",
            order, MAX_LAGRANGE_ORDER
        )));
    }
    if spec.window_taps.is_some() {
        return Err(PhaseError::arg(
            "Lagrange 方法不使用窗长 window_taps；请改用窗函数 sinc 或移除 window_taps",
        ));
    }
    let taps = order + 1;
    if let Some(t) = spec.taps {
        if t != taps {
            return Err(PhaseError::taps(format!(
                "Lagrange order={} 的支撑固定为 taps=order+1={}，显式 taps={} 不一致",
                order, taps, t
            )));
        }
    }
    let center = order as f64 / 2.0;
    let frac = spec.delay_samples - center;
    if !(0.0..1.0).contains(&frac) {
        return Err(PhaseError::delay(format!(
            "Lagrange order={} 的总延迟 D={} 必须 ∈ [order/2, order/2+1) = [{}, {})，实际分数部分 {}",
            order,
            spec.delay_samples,
            center,
            center + 1.0,
            frac
        )));
    }
    let band = match spec.verify_band_hz {
        Some(b) => {
            validate_verify_band(spec.fs, b)?;
            b
        }
        None => spec_band_hz(spec.fs),
    };
    let delay_samples = spec.delay_samples;
    let delay_ms = delay_samples / spec.fs * 1000.0;
    validate_declared_delay(
        spec.fs,
        delay_samples,
        spec.declared_delay_samples,
        spec.declared_delay_ms,
        spec.target_delay_samples,
    )?;
    // h[n] = Π_{j≠n} (D−j)/(n−j)，n=0..order（§4.2 Lagrange；精确插值多项式）。
    let mut coefficients = vec![0.0_f64; taps];
    for n in 0..=order {
        let mut p = 1.0_f64;
        for j in 0..=order {
            if j == n {
                continue;
            }
            p *= (delay_samples - j as f64) / ((n as f64) - (j as f64));
        }
        coefficients[n] = spec.polarity.sign() * p;
    }
    Ok(PhaseDesign {
        fs: spec.fs,
        kind: DelayKind::Fractional {
            integer_delay_samples: delay_samples.floor() as u64,
            fractional_delay_d: frac,
            method: FractionalMethod::Lagrange { order },
        },
        taps,
        coefficients,
        delay_samples,
        delay_ms,
        polarity: spec.polarity,
        side: spec.side,
        window_taps: None,
        support_start: 0,
        verify_band_hz: band,
        spec_band_required: false,
        declared_delay_samples: spec.declared_delay_samples,
        declared_delay_ms: spec.declared_delay_ms,
        target_delay_samples: spec.target_delay_samples,
    })
}

/// 统一设计入口。
pub fn design(spec: &PhaseSpec) -> Result<PhaseDesign, PhaseError> {
    match spec {
        PhaseSpec::Integer(s) => design_integer(s),
        PhaseSpec::Fractional(s) => design_fractional(s),
    }
}

/// 统一设计入口 + 声明频带判据强制（失败 E_FIR_BUDGET=INFEASIBLE，不静默截断/放宽）。
///
/// - 窗函数 sinc：按 §4.2 全带判据；失败即 INFEASIBLE。
/// - Lagrange：按 spec.verify_band_hz（缺省=全带，通常会 INFEASIBLE）；给出窄带时通过但
///   verification.checks.spec_band_compliant=false + warning，报告不得写成 §4.2 全带合规。
pub fn design_checked(spec: &PhaseSpec) -> Result<PhaseDesign, PhaseError> {
    let design = design(spec)?;
    let verification = design.verify();
    if !verification.passed() {
        return Err(PhaseError::budget(format!(
            "分数延迟 INFEASIBLE：band [{:.1},{:.1}] Hz 内 |H| 最大偏差 {:.6} dB（限 {:.3}）/ GD 最大偏差 {:.6} samples（限 {:.4}）",
            verification.band.lo_hz,
            verification.band.hi_hz,
            verification.band.mag_max_abs_dev_db,
            FRACTIONAL_MAG_TOL_DB,
            verification.band.gd_max_abs_dev_samples,
            FRACTIONAL_GD_TOL_SAMPLES
        )));
    }
    Ok(design)
}

// ---------------------------------------------------------------------------
// 因果 plan / 延迟声明检查
// ---------------------------------------------------------------------------

/// 因果延迟 plan：relative（可负）+ bulk D 吸收后的总延迟（§4.2 因果）。
#[derive(Clone, Debug, PartialEq)]
pub struct CausalDelayPlan {
    /// 运行采样率。
    pub fs: f64,
    /// 相对延迟（samples，可负=提前）。
    pub relative_delay_samples: f64,
    /// bulk/目标延迟（samples，D 吸收提前量）。
    pub bulk_delay_samples: f64,
    /// 总延迟 total=bulk+relative（>=0）。
    pub total_delay_samples: f64,
    /// total 的 ms。
    pub delay_ms: f64,
    /// 相对 bulk 的偏移（=relative）。
    pub relative_vs_bulk_samples: f64,
}

impl CausalDelayPlan {
    /// 是否整数总延迟。
    pub fn is_integer_total(&self) -> bool {
        let r = self.total_delay_samples.round();
        (self.total_delay_samples - r).abs() <= 1e-12
    }

    /// 总延迟的整数部分（仅整数总延迟）。
    pub fn integer_total(&self) -> Option<u64> {
        if self.is_integer_total() {
            Some(self.total_delay_samples.round() as u64)
        } else {
            None
        }
    }
}

/// 计算因果延迟：total = bulk + relative，任何负 total 都由 bulk 吸收失败 → E_FIR_NONCAUSAL。
pub fn plan_causal_delay(
    fs: f64,
    relative_delay_samples: f64,
    bulk_delay_samples: f64,
) -> Result<CausalDelayPlan, PhaseError> {
    validate_fs(fs)?;
    if !relative_delay_samples.is_finite() || !bulk_delay_samples.is_finite() {
        return Err(PhaseError::numeric(format!(
            "relative/bulk delay 必须有限：{}, {}",
            relative_delay_samples, bulk_delay_samples
        )));
    }
    if bulk_delay_samples < 0.0 {
        return Err(PhaseError::noncausal(format!(
            "bulk/target delay D={} < 0 非法",
            bulk_delay_samples
        )));
    }
    let total = bulk_delay_samples + relative_delay_samples;
    if total < 0.0 {
        return Err(PhaseError::noncausal(format!(
            "total=bulk({})+relative({})={} < 0：负/提前延迟必须由 target_delay_samples/D 吸收，否则不可因果",
            bulk_delay_samples, relative_delay_samples, total
        )));
    }
    Ok(CausalDelayPlan {
        fs,
        relative_delay_samples,
        bulk_delay_samples,
        total_delay_samples: total,
        delay_ms: total / fs * 1000.0,
        relative_vs_bulk_samples: relative_delay_samples,
    })
}

// ---------------------------------------------------------------------------
// 方向（输入列 / 输出行）
// ---------------------------------------------------------------------------

/// 单通道 delay+polarity 操作（方向 plan 的元素）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelPhaseOp {
    /// 延迟（samples，>=0；方向 plan 内允许分数）。
    pub delay_samples: f64,
    /// 极性。
    pub polarity: Polarity,
}

impl ChannelPhaseOp {
    /// 构造（Normal）。
    pub fn delay(delay_samples: f64) -> Self {
        ChannelPhaseOp {
            delay_samples,
            polarity: Polarity::Normal,
        }
    }

    /// 设极性。
    pub fn with_polarity(mut self, polarity: Polarity) -> Self {
        self.polarity = polarity;
        self
    }

    /// 反相。
    pub fn inverted(self) -> Self {
        ChannelPhaseOp {
            delay_samples: self.delay_samples,
            polarity: self.polarity.inverted(),
        }
    }
}

/// 方向 plan（输入列 / 输出行）。
#[derive(Clone, Debug, PartialEq)]
pub struct DirectionalPlan {
    /// 作用侧。
    pub side: ApplySide,
    /// 每列（输入）或每行（输出）的 delay/polarity。
    pub ops: Vec<ChannelPhaseOp>,
}

impl DirectionalPlan {
    /// 输入侧 plan（ops.len()=cols）。
    pub fn input(ops: Vec<ChannelPhaseOp>) -> Self {
        DirectionalPlan {
            side: ApplySide::InputColumns,
            ops,
        }
    }

    /// 输出侧 plan（ops.len()=rows）。
    pub fn output(ops: Vec<ChannelPhaseOp>) -> Self {
        DirectionalPlan {
            side: ApplySide::OutputRows,
            ops,
        }
    }

    /// plan 维度校验。
    pub fn validate(&self, rows: usize, cols: usize) -> Result<(), PhaseError> {
        let expected = match self.side {
            ApplySide::InputColumns => cols,
            ApplySide::OutputRows => rows,
        };
        if self.ops.len() != expected {
            return Err(PhaseError::arg(format!(
                "方向 plan 的 ops 数量 {} != {}（side={}，行数={} 列数={}）",
                self.ops.len(),
                expected,
                self.side.as_str(),
                rows,
                cols
            )));
        }
        Ok(())
    }
}

/// 单通道 delay/polarity FIR（供方向合成；整数延迟用最小 delta，分数延迟用 63-tap Kaiser sinc）。
pub fn channel_op_fir(fs: f64, op: &ChannelPhaseOp) -> Result<Vec<f64>, PhaseError> {
    if !op.delay_samples.is_finite() || op.delay_samples < 0.0 {
        return Err(PhaseError::noncausal(format!(
            "channel op delay={} 必须为有限 >=0（负/提前由 bulk D 吸收）",
            op.delay_samples
        )));
    }
    let rounded = op.delay_samples.round();
    if (op.delay_samples - rounded).abs() <= 1e-12 {
        let spec = IntegerDelaySpec::new(fs, rounded as u64).with_polarity(op.polarity);
        return Ok(design_integer(&spec)?.coefficients);
    }
    let spec = FractionalDelaySpec::windowed_sinc(fs, op.delay_samples).with_polarity(op.polarity);
    Ok(design_fractional(&spec)?.coefficients)
}

/// 方向合成结果。
#[derive(Clone, Debug, PartialEq)]
pub struct DirectionalApplication {
    /// 作用侧。
    pub side: ApplySide,
    /// 行数。
    pub rows: usize,
    /// 列数。
    pub cols: usize,
    /// 每列/行的 delay/polarity FIR。
    pub channel_firs: Vec<Vec<f64>>,
    /// 合成后的路由 FIR（行优先 p*cols+i，零路由为空 vec）。
    pub route_firs: Vec<Vec<f64>>,
    /// 方向说明。
    pub statement: &'static str,
}

/// 对矩阵路由 FIR 应用方向性 delay/polarity（§7.1：输入列作用 A/G 前、输出行作用 P 输出）。
///
/// matrix 长度必须 = rows*cols，行优先 p*cols+i；零路由用空 vec 表示。
pub fn apply_directional_phase(
    fs: f64,
    rows: usize,
    cols: usize,
    matrix: &[Vec<f64>],
    plan: &DirectionalPlan,
) -> Result<DirectionalApplication, PhaseError> {
    validate_fs(fs)?;
    if rows == 0 || cols == 0 {
        return Err(PhaseError::arg("rows/cols 必须 >=1"));
    }
    if matrix.len() != rows * cols {
        return Err(PhaseError::arg(format!(
            "矩阵路由数量 {} != rows*cols={}（rows={}, cols={}）",
            matrix.len(),
            rows * cols,
            rows,
            cols
        )));
    }
    plan.validate(rows, cols)?;
    let mut channel_firs = Vec::with_capacity(plan.ops.len());
    for op in &plan.ops {
        channel_firs.push(channel_op_fir(fs, op)?);
    }
    let mut route_firs = Vec::with_capacity(rows * cols);
    for p in 0..rows {
        for i in 0..cols {
            let base = &matrix[p * cols + i];
            if base.is_empty() {
                route_firs.push(Vec::new());
                continue;
            }
            let channel = match plan.side {
                ApplySide::InputColumns => &channel_firs[i],
                ApplySide::OutputRows => &channel_firs[p],
            };
            route_firs.push(convolve_linear(base, channel));
        }
    }
    Ok(DirectionalApplication {
        side: plan.side,
        rows,
        cols,
        channel_firs,
        route_firs,
        statement: plan.side.statement(),
    })
}

/// 极性 XOR 组合 plan（设计极性 + 校准 invert；禁止双乘）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolarityPlan {
    /// 设计系数极性。
    pub design: Polarity,
    /// 校准 invert 字段（CONTRACT §3.2）。
    pub calibration: Polarity,
}

impl PolarityPlan {
    /// 构造。
    pub fn new(design: Polarity, calibration: Polarity) -> Self {
        PolarityPlan {
            design,
            calibration,
        }
    }

    /// effective 极性 = design XOR calibration。
    pub fn effective(&self) -> Polarity {
        self.design.xor(self.calibration)
    }

    /// effective 符号 = design.sign()·calibration.sign()。
    pub fn effective_sign(&self) -> f64 {
        self.design.sign() * self.calibration.sign()
    }

    /// 是否两处都写 invert（effective 反而 normal，报告必须写 effective）。
    pub fn both_inverted(&self) -> bool {
        self.design == Polarity::Invert && self.calibration == Polarity::Invert
    }

    /// 报告说明。
    pub fn note(&self) -> &'static str {
        CALIBRATION_XOR_STATEMENT
    }
}

// ---------------------------------------------------------------------------
// 振铃（§1.4）
// ---------------------------------------------------------------------------

fn db_ratio_20log10(numerator: f64, denominator: f64) -> Option<f64> {
    if denominator <= 0.0 {
        return None;
    }
    if numerator <= 0.0 {
        return Some(f64::NEG_INFINITY);
    }
    Some(20.0 * (numerator / denominator).log10())
}

fn db_ratio_10log10(numerator: f64, denominator: f64) -> Option<f64> {
    if denominator <= 0.0 {
        return None;
    }
    if numerator <= 0.0 {
        return Some(f64::NEG_INFINITY);
    }
    Some(10.0 * (numerator / denominator).log10())
}

/// 振铃统计（§1.4 口径；系数上直接测，不做循环卷积）。
pub fn ringing_stats(coeffs: &[f64], fs: f64) -> RingingStats {
    let exclusion_samples = (RINGING_EXCLUSION_SECONDS * fs).ceil() as usize;
    if coeffs.is_empty() {
        return RingingStats {
            exclusion_samples,
            peak_index: 0,
            pre_ring_db: None,
            post_ring_db: None,
            pre_ring_energy_db: None,
            post_ring_energy_db: None,
            note: "空系数：振铃 n/a".to_string(),
        };
    }
    let peak_index = coeffs
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let peak_abs = coeffs[peak_index].abs();
    let total_energy: f64 = coeffs.iter().map(|h| h * h).sum();
    let len = coeffs.len();
    if peak_index < exclusion_samples || peak_index + exclusion_samples >= len {
        return RingingStats {
            exclusion_samples,
            peak_index,
            pre_ring_db: None,
            post_ring_db: None,
            pre_ring_energy_db: None,
            post_ring_energy_db: None,
            note: format!(
                "支撑过短：p−W={} < 0 或 p+W={} >= L={}，§1.4 振铃 n/a",
                peak_index as isize - exclusion_samples as isize,
                peak_index + exclusion_samples,
                len
            ),
        };
    }
    let pre = &coeffs[..peak_index - exclusion_samples];
    let post = &coeffs[peak_index + exclusion_samples + 1..];
    let pre_max = pre.iter().fold(0.0_f64, |m, h| m.max(h.abs()));
    let post_max = post.iter().fold(0.0_f64, |m, h| m.max(h.abs()));
    let pre_energy: f64 = pre.iter().map(|h| h * h).sum();
    let post_energy: f64 = post.iter().map(|h| h * h).sum();
    RingingStats {
        exclusion_samples,
        peak_index,
        pre_ring_db: db_ratio_20log10(pre_max, peak_abs),
        post_ring_db: db_ratio_20log10(post_max, peak_abs),
        pre_ring_energy_db: db_ratio_10log10(pre_energy, total_energy),
        post_ring_energy_db: db_ratio_10log10(post_energy, total_energy),
        note: format!(
            "W=ceil(0.000333·fs)={}，主峰 p={}，前后振铃按 §1.4 口径",
            exclusion_samples, peak_index
        ),
    }
}

// ---------------------------------------------------------------------------
// 私有测试（include! tests/T40；integrator 不要再注册进 public_registration.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod phase_tests {
    use super::*;

    include!("../tests/T40/phase_cases.rs");
}
